//! Failure-aware multi-directory publication for stripped templates.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
use rustix::fs::linkat;
use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, RenameFlags, fstat, fsync, openat, renameat_with, statat,
    unlinkat,
};
use rustix::io::Errno;

use crate::CPU_TEMPLATE_DOCUMENT_MAX_BYTES;
use crate::input::{FileIdentity, PreparedStripInput, StripOutputMode};

static PRIVATE_NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Failure while publishing a complete strip batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripPublicationError {
    ArtifactCountMismatch,
    ArtifactTooLarge,
    InputChanged,
    OutputInspection,
    Collision,
    AtomicCommitUnsupported,
    Staging,
    Commit,
    PrecommitCleanupUncertain,
    PublicationRolledBack,
    RollbackUncertain,
    CommittedDurabilityUncertain,
    CommittedCleanupUncertain,
}

impl fmt::Display for StripPublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ArtifactCountMismatch => "strip output count does not match its inputs",
            Self::ArtifactTooLarge => "strip output artifact exceeds the size limit",
            Self::InputChanged => "strip input identity changed before publication",
            Self::OutputInspection => "strip output target could not be inspected safely",
            Self::Collision => "strip output target already exists",
            Self::AtomicCommitUnsupported => {
                "strip output filesystem lacks required atomic rename support"
            }
            Self::Staging => "complete strip outputs could not be staged",
            Self::Commit => "strip output could not be committed",
            Self::PrecommitCleanupUncertain => {
                "strip output was not committed, but private cleanup is uncertain"
            }
            Self::PublicationRolledBack => {
                "strip publication failed and the original batch was restored"
            }
            Self::RollbackUncertain => {
                "strip publication failed and batch restoration is uncertain"
            }
            Self::CommittedDurabilityUncertain => {
                "strip outputs were committed, but directory durability is uncertain"
            }
            Self::CommittedCleanupUncertain => {
                "strip outputs were committed, but private cleanup is uncertain"
            }
        })
    }
}

impl std::error::Error for StripPublicationError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PathState {
    identity: FileIdentity,
    file_type: FileType,
    link_count: u64,
}

#[derive(Debug)]
struct StagedArtifact {
    input: PreparedStripInput,
    stage_name: OsString,
    staged_identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrivateCreationError {
    Operation,
    CleanupUncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublishOneError {
    NoMutation(StripPublicationError),
    MutationUncertain,
}

#[derive(Debug, Default)]
struct StripFaults {
    force_unsupported_directory: Option<usize>,
    fail_stage_at: Option<usize>,
    fail_write_at: Option<usize>,
    fail_flush_at: Option<usize>,
    fail_file_sync_at: Option<usize>,
    fail_precommit_sync_directory: Option<usize>,
    replace_stage_before_commit: Option<usize>,
    replace_output_before_commit: Option<usize>,
    fail_commit_at: Option<usize>,
    fail_after_commits: Option<usize>,
    link_rollback_stage_at: Option<usize>,
    fail_rollback_at: Vec<usize>,
    fail_postcommit_sync_directory: Option<usize>,
    fail_cleanup_at: Vec<usize>,
    fail_cleanup_sync_directory: Option<usize>,
}

/// Publish one complete canonical output for every prepared strip input.
pub(crate) fn publish_strip_artifacts(
    inputs: Vec<PreparedStripInput>,
    artifacts: Vec<Vec<u8>>,
) -> Result<(), StripPublicationError> {
    publish_strip_with_faults(inputs, artifacts, &StripFaults::default())
}

fn publish_strip_with_faults(
    inputs: Vec<PreparedStripInput>,
    artifacts: Vec<Vec<u8>>,
    faults: &StripFaults,
) -> Result<(), StripPublicationError> {
    if inputs.len() != artifacts.len() {
        return Err(StripPublicationError::ArtifactCountMismatch);
    }
    if artifacts
        .iter()
        .any(|artifact| artifact.len() > CPU_TEMPLATE_DOCUMENT_MAX_BYTES)
    {
        return Err(StripPublicationError::ArtifactTooLarge);
    }
    let mode = inputs
        .first()
        .map(PreparedStripInput::mode)
        .ok_or(StripPublicationError::ArtifactCountMismatch)?;
    if inputs.iter().any(|input| input.mode() != mode) {
        return Err(StripPublicationError::ArtifactCountMismatch);
    }

    inspect_all_inputs_and_outputs(&inputs)?;
    probe_all_directories(&inputs, mode, faults)?;

    let reserved_entries = inputs
        .iter()
        .flat_map(|input| {
            [
                (
                    input.directory_identity(),
                    input.input_name().to_os_string(),
                ),
                (
                    input.directory_identity(),
                    input.output_name().to_os_string(),
                ),
            ]
        })
        .collect::<Vec<_>>();
    let mut staged = Vec::with_capacity(inputs.len());
    for (index, (input, bytes)) in inputs.into_iter().zip(artifacts).enumerate() {
        let reserved = reserved_entries
            .iter()
            .filter(|(directory, _)| *directory == input.directory_identity())
            .map(|(_, name)| name.as_os_str())
            .collect::<Vec<_>>();
        let created = create_private(input.directory(), "stage", &bytes, &reserved, faults, index);
        match created {
            Ok((stage_name, staged_identity)) => staged.push(StagedArtifact {
                input,
                stage_name,
                staged_identity,
            }),
            Err(error) => {
                return cleanup_before_commit(
                    &staged,
                    map_private_error(error, StripPublicationError::Staging),
                    faults,
                );
            }
        }
    }

    if !sync_unique_directories(&staged, faults.fail_precommit_sync_directory) {
        return cleanup_before_commit(&staged, StripPublicationError::Staging, faults);
    }

    let mut published = 0;
    while published < staged.len() {
        let Some(entry) = staged.get(published) else {
            return Err(StripPublicationError::RollbackUncertain);
        };
        if faults.replace_stage_before_commit == Some(published)
            && replace_entry_for_test(entry.input.directory(), &entry.stage_name).is_err()
        {
            return rollback_after_failure(
                &staged,
                published,
                true,
                StripPublicationError::OutputInspection,
                faults,
            );
        }
        if faults.replace_output_before_commit == Some(published) {
            let _ = replace_output_for_test(entry);
            return rollback_after_failure(
                &staged,
                published,
                true,
                StripPublicationError::OutputInspection,
                faults,
            );
        }
        match publish_one(entry, published, faults) {
            Ok(()) => {}
            Err(PublishOneError::NoMutation(error)) => {
                return rollback_after_failure(&staged, published, false, error, faults);
            }
            Err(PublishOneError::MutationUncertain) => {
                return rollback_after_failure(
                    &staged,
                    published,
                    true,
                    StripPublicationError::Commit,
                    faults,
                );
            }
        }
        published += 1;
        if faults.fail_after_commits == Some(published) {
            return rollback_after_failure(
                &staged,
                published,
                false,
                StripPublicationError::Commit,
                faults,
            );
        }
    }

    if !sync_unique_directories(&staged, faults.fail_postcommit_sync_directory) {
        return Err(StripPublicationError::CommittedDurabilityUncertain);
    }
    if !cleanup_committed_stages(&staged, faults)
        || !sync_unique_directories(&staged, faults.fail_cleanup_sync_directory)
    {
        return Err(StripPublicationError::CommittedCleanupUncertain);
    }
    Ok(())
}

fn inspect_all_inputs_and_outputs(
    inputs: &[PreparedStripInput],
) -> Result<(), StripPublicationError> {
    for input in inputs {
        let descriptor = state_from_metadata(
            &fstat(input.input()).map_err(|_| StripPublicationError::InputChanged)?,
        )?;
        let entry = path_state(input.directory(), input.input_name())?
            .ok_or(StripPublicationError::InputChanged)?;
        if !is_expected_regular(descriptor, input.input_identity())
            || !is_expected_regular(entry, input.input_identity())
        {
            return Err(StripPublicationError::InputChanged);
        }
        match input.mode() {
            StripOutputMode::Absent => match path_state(input.directory(), input.output_name())? {
                None => {}
                Some(_) => return Err(StripPublicationError::Collision),
            },
            StripOutputMode::ReplaceInput => {
                if input.input_link_count() != 1
                    || descriptor.link_count != 1
                    || entry.link_count != 1
                    || input.input_name() != input.output_name()
                {
                    return Err(StripPublicationError::InputChanged);
                }
            }
        }
    }
    Ok(())
}

fn probe_all_directories(
    inputs: &[PreparedStripInput],
    mode: StripOutputMode,
    faults: &StripFaults,
) -> Result<(), StripPublicationError> {
    let mut seen = BTreeSet::new();
    let mut directory_position = 0;
    for input in inputs {
        if !seen.insert(input.directory_identity()) {
            continue;
        }
        let reserved = inputs
            .iter()
            .filter(|candidate| candidate.directory_identity() == input.directory_identity())
            .flat_map(|candidate| [candidate.input_name(), candidate.output_name()])
            .collect::<Vec<_>>();
        if faults.force_unsupported_directory == Some(directory_position) {
            return Err(StripPublicationError::AtomicCommitUnsupported);
        }
        probe_directory(input.directory(), mode, &reserved)?;
        directory_position += 1;
    }
    Ok(())
}

fn probe_directory<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    mode: StripOutputMode,
    reserved: &[&OsStr],
) -> Result<(), StripPublicationError> {
    let first = create_private_unfaulted(directory, "probe", b"a", reserved)
        .map_err(|error| map_private_error(error, StripPublicationError::Staging))?;
    let second = match create_private_unfaulted(directory, "probe", b"b", reserved) {
        Ok(second) => second,
        Err(error) => {
            let cleaned = unlink_if_identity(directory, &first.0, first.1).is_ok()
                && fsync(directory).is_ok();
            return if cleaned {
                Err(map_private_error(error, StripPublicationError::Staging))
            } else {
                Err(StripPublicationError::PrecommitCleanupUncertain)
            };
        }
    };

    let supported = match mode {
        StripOutputMode::Absent => matches!(
            renameat_with(
                directory,
                &first.0,
                directory,
                &second.0,
                RenameFlags::NOREPLACE,
            ),
            Err(error) if error == Errno::EXIST
        ),
        StripOutputMode::ReplaceInput => {
            let first_exchange = renameat_with(
                directory,
                &first.0,
                directory,
                &second.0,
                RenameFlags::EXCHANGE,
            )
            .is_ok()
                && identities_at(directory, &first.0, second.1, &second.0, first.1).is_ok();
            first_exchange
                && renameat_with(
                    directory,
                    &first.0,
                    directory,
                    &second.0,
                    RenameFlags::EXCHANGE,
                )
                .is_ok()
                && identities_at(directory, &first.0, first.1, &second.0, second.1).is_ok()
        }
    };

    let first_cleanup = unlink_if_known_identity(directory, &first.0, &[first.1, second.1]);
    let second_cleanup = unlink_if_known_identity(directory, &second.0, &[first.1, second.1]);
    if first_cleanup.is_err() || second_cleanup.is_err() || fsync(directory).is_err() {
        return Err(StripPublicationError::PrecommitCleanupUncertain);
    }
    if supported {
        Ok(())
    } else {
        Err(StripPublicationError::AtomicCommitUnsupported)
    }
}

fn create_private<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    kind: &str,
    contents: &[u8],
    reserved: &[&OsStr],
    faults: &StripFaults,
    index: usize,
) -> Result<(OsString, FileIdentity), PrivateCreationError> {
    if faults.fail_stage_at == Some(index) {
        return Err(PrivateCreationError::Operation);
    }
    create_private_inner(directory, kind, contents, reserved, Some((faults, index)))
}

fn create_private_unfaulted<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    kind: &str,
    contents: &[u8],
    reserved: &[&OsStr],
) -> Result<(OsString, FileIdentity), PrivateCreationError> {
    create_private_inner(directory, kind, contents, reserved, None)
}

fn create_private_inner<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    kind: &str,
    contents: &[u8],
    reserved: &[&OsStr],
    fault: Option<(&StripFaults, usize)>,
) -> Result<(OsString, FileIdentity), PrivateCreationError> {
    for _ in 0..128 {
        let serial = PRIVATE_NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name: OsString = format!(
            ".bangbang-cpu-template-helper.{kind}.{}.{}",
            std::process::id(),
            serial
        )
        .into();
        if reserved.contains(&name.as_os_str()) {
            continue;
        }
        let descriptor = match openat(
            directory,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(descriptor) => descriptor,
            Err(error) if error == Errno::EXIST => continue,
            Err(_) => return Err(PrivateCreationError::Operation),
        };
        let mut file = File::from(descriptor);
        let mut operation_failed = false;
        if fault.is_some_and(|(faults, index)| faults.fail_write_at == Some(index)) {
            let prefix_length = contents.len().min(1);
            let _ = file.write_all(contents.get(..prefix_length).unwrap_or_default());
            operation_failed = true;
        } else if file.write_all(contents).is_err() {
            operation_failed = true;
        }
        if !operation_failed {
            operation_failed = fault
                .is_some_and(|(faults, index)| faults.fail_flush_at == Some(index))
                || file.flush().is_err();
        }
        if !operation_failed {
            operation_failed = fault
                .is_some_and(|(faults, index)| faults.fail_file_sync_at == Some(index))
                || file.sync_all().is_err();
        }
        let metadata = fstat(&file).map_err(|_| PrivateCreationError::CleanupUncertain)?;
        let state =
            state_from_metadata(&metadata).map_err(|_| PrivateCreationError::CleanupUncertain)?;
        drop(file);
        if state.file_type != FileType::RegularFile || state.link_count != 1 {
            return Err(PrivateCreationError::CleanupUncertain);
        }
        if operation_failed {
            return if unlink_if_identity(directory, &name, state.identity).is_ok()
                && fsync(directory).is_ok()
            {
                Err(PrivateCreationError::Operation)
            } else {
                Err(PrivateCreationError::CleanupUncertain)
            };
        }
        return Ok((name, state.identity));
    }
    Err(PrivateCreationError::Operation)
}

fn publish_one(
    entry: &StagedArtifact,
    index: usize,
    faults: &StripFaults,
) -> Result<(), PublishOneError> {
    if faults.fail_commit_at == Some(index) {
        return Err(PublishOneError::NoMutation(StripPublicationError::Commit));
    }
    ensure_identity(
        entry.input.directory(),
        &entry.stage_name,
        entry.staged_identity,
    )
    .map_err(|_| PublishOneError::NoMutation(StripPublicationError::OutputInspection))?;

    match entry.input.mode() {
        StripOutputMode::Absent => {
            ensure_absent(entry.input.directory(), entry.input.output_name())
                .map_err(|_| PublishOneError::NoMutation(StripPublicationError::Collision))?;
            renameat_with(
                entry.input.directory(),
                &entry.stage_name,
                entry.input.directory(),
                entry.input.output_name(),
                RenameFlags::NOREPLACE,
            )
            .map_err(|error| {
                let error = if error == Errno::EXIST {
                    StripPublicationError::Collision
                } else {
                    StripPublicationError::Commit
                };
                PublishOneError::NoMutation(error)
            })?;
            ensure_identity(
                entry.input.directory(),
                entry.input.output_name(),
                entry.staged_identity,
            )
            .map_err(|_| PublishOneError::MutationUncertain)
        }
        StripOutputMode::ReplaceInput => {
            ensure_input_identity(&entry.input)
                .map_err(|_| PublishOneError::NoMutation(StripPublicationError::InputChanged))?;
            renameat_with(
                entry.input.directory(),
                &entry.stage_name,
                entry.input.directory(),
                entry.input.output_name(),
                RenameFlags::EXCHANGE,
            )
            .map_err(|_| PublishOneError::NoMutation(StripPublicationError::Commit))?;
            if identities_at(
                entry.input.directory(),
                &entry.stage_name,
                entry.input.input_identity(),
                entry.input.output_name(),
                entry.staged_identity,
            )
            .is_ok()
                && ensure_single_link_file(entry.input.input(), entry.input.input_identity())
                    .is_ok()
            {
                return Ok(());
            }
            restore_captured_exchange(entry)
        }
    }
}

fn restore_captured_exchange(entry: &StagedArtifact) -> Result<(), PublishOneError> {
    ensure_identity(
        entry.input.directory(),
        entry.input.output_name(),
        entry.staged_identity,
    )
    .map_err(|_| PublishOneError::MutationUncertain)?;
    let captured = path_state(entry.input.directory(), &entry.stage_name)
        .map_err(|_| PublishOneError::MutationUncertain)?
        .filter(|state| state.file_type == FileType::RegularFile)
        .ok_or(PublishOneError::MutationUncertain)?;
    renameat_with(
        entry.input.directory(),
        &entry.stage_name,
        entry.input.directory(),
        entry.input.output_name(),
        RenameFlags::EXCHANGE,
    )
    .map_err(|_| PublishOneError::MutationUncertain)?;
    ensure_identity(
        entry.input.directory(),
        &entry.stage_name,
        entry.staged_identity,
    )
    .map_err(|_| PublishOneError::MutationUncertain)?;
    ensure_regular_identity(
        entry.input.directory(),
        entry.input.output_name(),
        captured.identity,
    )
    .map_err(|_| PublishOneError::MutationUncertain)?;
    Err(PublishOneError::NoMutation(
        StripPublicationError::InputChanged,
    ))
}

fn rollback_after_failure(
    entries: &[StagedArtifact],
    published: usize,
    mutation_uncertain: bool,
    original_error: StripPublicationError,
    faults: &StripFaults,
) -> Result<(), StripPublicationError> {
    let mut uncertain = mutation_uncertain;
    for index in (0..published).rev() {
        let Some(entry) = entries.get(index) else {
            uncertain = true;
            continue;
        };
        if faults.link_rollback_stage_at == Some(index)
            && link_rollback_stage_for_test(entry).is_err()
        {
            uncertain = true;
        }
        if faults.fail_rollback_at.contains(&index) || rollback_one(entry).is_err() {
            uncertain = true;
        }
    }
    if !cleanup_new_stages(entries, faults) {
        uncertain = true;
    }
    if !sync_unique_directories(entries, faults.fail_cleanup_sync_directory) {
        uncertain = true;
    }
    if uncertain {
        Err(StripPublicationError::RollbackUncertain)
    } else if published == 0 {
        Err(original_error)
    } else {
        Err(StripPublicationError::PublicationRolledBack)
    }
}

#[cfg(test)]
fn link_rollback_stage_for_test(entry: &StagedArtifact) -> Result<(), ()> {
    for _ in 0..128 {
        let serial = PRIVATE_NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
        let alias: OsString = format!(
            ".bangbang-cpu-template-helper.race-link.{}.{}",
            std::process::id(),
            serial
        )
        .into();
        match linkat(
            entry.input.directory(),
            &entry.stage_name,
            entry.input.directory(),
            &alias,
            AtFlags::empty(),
        ) {
            Ok(()) => return Ok(()),
            Err(error) if error == Errno::EXIST => continue,
            Err(_) => return Err(()),
        }
    }
    Err(())
}

#[cfg(not(test))]
fn link_rollback_stage_for_test(_: &StagedArtifact) -> Result<(), ()> {
    Err(())
}

fn rollback_one(entry: &StagedArtifact) -> Result<(), ()> {
    ensure_identity(
        entry.input.directory(),
        entry.input.output_name(),
        entry.staged_identity,
    )?;
    match entry.input.mode() {
        StripOutputMode::Absent => {
            ensure_absent(entry.input.directory(), &entry.stage_name)?;
            renameat_with(
                entry.input.directory(),
                entry.input.output_name(),
                entry.input.directory(),
                &entry.stage_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|_| ())?;
            ensure_identity(
                entry.input.directory(),
                &entry.stage_name,
                entry.staged_identity,
            )?;
            ensure_absent(entry.input.directory(), entry.input.output_name())
        }
        StripOutputMode::ReplaceInput => {
            let stage = ensure_regular_identity(
                entry.input.directory(),
                &entry.stage_name,
                entry.input.input_identity(),
            )?;
            let descriptor = state_from_metadata(&fstat(entry.input.input()).map_err(|_| ())?)
                .map_err(|_| ())?;
            if !is_expected_regular(descriptor, entry.input.input_identity()) {
                return Err(());
            }
            let link_count_changed = stage.link_count != 1 || descriptor.link_count != 1;
            renameat_with(
                entry.input.directory(),
                &entry.stage_name,
                entry.input.directory(),
                entry.input.output_name(),
                RenameFlags::EXCHANGE,
            )
            .map_err(|_| ())?;
            ensure_identity(
                entry.input.directory(),
                &entry.stage_name,
                entry.staged_identity,
            )?;
            ensure_regular_identity(
                entry.input.directory(),
                entry.input.output_name(),
                entry.input.input_identity(),
            )?;
            if link_count_changed { Err(()) } else { Ok(()) }
        }
    }
}

fn cleanup_before_commit(
    entries: &[StagedArtifact],
    error: StripPublicationError,
    faults: &StripFaults,
) -> Result<(), StripPublicationError> {
    if cleanup_new_stages(entries, faults)
        && sync_unique_directories(entries, faults.fail_cleanup_sync_directory)
    {
        Err(error)
    } else {
        Err(StripPublicationError::PrecommitCleanupUncertain)
    }
}

fn cleanup_new_stages(entries: &[StagedArtifact], faults: &StripFaults) -> bool {
    let mut succeeded = true;
    for (index, entry) in entries.iter().enumerate() {
        if faults.fail_cleanup_at.contains(&index)
            || unlink_if_identity(
                entry.input.directory(),
                &entry.stage_name,
                entry.staged_identity,
            )
            .is_err()
        {
            succeeded = false;
        }
    }
    succeeded
}

fn cleanup_committed_stages(entries: &[StagedArtifact], faults: &StripFaults) -> bool {
    let mut succeeded = true;
    for (index, entry) in entries.iter().enumerate() {
        if faults.fail_cleanup_at.contains(&index) {
            succeeded = false;
            continue;
        }
        let result = match entry.input.mode() {
            StripOutputMode::Absent => ensure_absent(entry.input.directory(), &entry.stage_name),
            StripOutputMode::ReplaceInput => {
                ensure_single_link_file(entry.input.input(), entry.input.input_identity()).and_then(
                    |()| {
                        unlink_if_identity(
                            entry.input.directory(),
                            &entry.stage_name,
                            entry.input.input_identity(),
                        )
                    },
                )
            }
        };
        if result.is_err() {
            succeeded = false;
        }
    }
    succeeded
}

fn sync_unique_directories(entries: &[StagedArtifact], failing_position: Option<usize>) -> bool {
    let mut seen = BTreeSet::new();
    let mut position = 0;
    let mut succeeded = true;
    for entry in entries {
        if !seen.insert(entry.input.directory_identity()) {
            continue;
        }
        if failing_position == Some(position) || fsync(entry.input.directory()).is_err() {
            succeeded = false;
        }
        position += 1;
    }
    succeeded
}

fn ensure_input_identity(input: &PreparedStripInput) -> Result<(), ()> {
    ensure_single_link_file(input.input(), input.input_identity())?;
    let state = path_state(input.directory(), input.input_name())
        .map_err(|_| ())?
        .ok_or(())?;
    if is_expected_regular(state, input.input_identity()) && state.link_count == 1 {
        Ok(())
    } else {
        Err(())
    }
}

fn ensure_single_link_file<Fd: std::os::fd::AsFd>(
    descriptor: &Fd,
    expected: FileIdentity,
) -> Result<(), ()> {
    let state = state_from_metadata(&fstat(descriptor).map_err(|_| ())?).map_err(|_| ())?;
    if is_expected_regular(state, expected) && state.link_count == 1 {
        Ok(())
    } else {
        Err(())
    }
}

fn ensure_identity<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    name: &OsStr,
    expected: FileIdentity,
) -> Result<(), ()> {
    match path_state(directory, name).map_err(|_| ())? {
        Some(state) if is_expected_regular(state, expected) && state.link_count == 1 => Ok(()),
        Some(_) | None => Err(()),
    }
}

fn ensure_regular_identity<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    name: &OsStr,
    expected: FileIdentity,
) -> Result<PathState, ()> {
    match path_state(directory, name).map_err(|_| ())? {
        Some(state) if is_expected_regular(state, expected) => Ok(state),
        Some(_) | None => Err(()),
    }
}

fn identities_at<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    first_name: &OsStr,
    first_identity: FileIdentity,
    second_name: &OsStr,
    second_identity: FileIdentity,
) -> Result<(), ()> {
    ensure_identity(directory, first_name, first_identity)?;
    ensure_identity(directory, second_name, second_identity)
}

fn ensure_absent<Fd: std::os::fd::AsFd>(directory: &Fd, name: &OsStr) -> Result<(), ()> {
    match path_state(directory, name).map_err(|_| ())? {
        None => Ok(()),
        Some(_) => Err(()),
    }
}

fn unlink_if_identity<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    name: &OsStr,
    expected: FileIdentity,
) -> Result<(), ()> {
    match path_state(directory, name).map_err(|_| ())? {
        None => Ok(()),
        Some(state) if is_expected_regular(state, expected) && state.link_count == 1 => {
            unlinkat(directory, name, AtFlags::empty()).map_err(|_| ())
        }
        Some(_) => Err(()),
    }
}

fn unlink_if_known_identity<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    name: &OsStr,
    expected: &[FileIdentity],
) -> Result<(), ()> {
    match path_state(directory, name).map_err(|_| ())? {
        None => Ok(()),
        Some(state)
            if state.file_type == FileType::RegularFile
                && state.link_count == 1
                && expected.contains(&state.identity) =>
        {
            unlinkat(directory, name, AtFlags::empty()).map_err(|_| ())
        }
        Some(_) => Err(()),
    }
}

fn path_state<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    name: &OsStr,
) -> Result<Option<PathState>, StripPublicationError> {
    match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => state_from_metadata(&metadata).map(Some),
        Err(error) if error == Errno::NOENT => Ok(None),
        Err(_) => Err(StripPublicationError::OutputInspection),
    }
}

fn state_from_metadata(metadata: &rustix::fs::Stat) -> Result<PathState, StripPublicationError> {
    Ok(PathState {
        identity: FileIdentity::from_metadata(metadata),
        file_type: FileType::from_raw_mode(metadata.st_mode),
        link_count: metadata.st_nlink.into(),
    })
}

fn is_expected_regular(state: PathState, identity: FileIdentity) -> bool {
    state.identity == identity && state.file_type == FileType::RegularFile
}

fn map_private_error(
    error: PrivateCreationError,
    operation_error: StripPublicationError,
) -> StripPublicationError {
    match error {
        PrivateCreationError::Operation => operation_error,
        PrivateCreationError::CleanupUncertain => StripPublicationError::PrecommitCleanupUncertain,
    }
}

#[cfg(test)]
fn replace_entry_for_test<Fd: std::os::fd::AsFd>(directory: &Fd, name: &OsStr) -> Result<(), ()> {
    unlinkat(directory, name, AtFlags::empty()).map_err(|_| ())?;
    let descriptor = openat(
        directory,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|_| ())?;
    let mut file = File::from(descriptor);
    file.write_all(b"racing-replacement").map_err(|_| ())?;
    file.sync_all().map_err(|_| ())
}

#[cfg(not(test))]
fn replace_entry_for_test<Fd: std::os::fd::AsFd>(_: &Fd, _: &OsStr) -> Result<(), ()> {
    Err(())
}

#[cfg(test)]
fn replace_output_for_test(entry: &StagedArtifact) -> Result<(), ()> {
    match entry.input.mode() {
        StripOutputMode::Absent => {
            let descriptor = openat(
                entry.input.directory(),
                entry.input.output_name(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|_| ())?;
            let mut file = File::from(descriptor);
            file.write_all(b"racing-winner").map_err(|_| ())?;
            file.sync_all().map_err(|_| ())
        }
        StripOutputMode::ReplaceInput => {
            replace_entry_for_test(entry.input.directory(), entry.input.output_name())
        }
    }
}

#[cfg(not(test))]
fn replace_output_for_test(_: &StagedArtifact) -> Result<(), ()> {
    Err(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::indexing_slicing, clippy::unwrap_used)]

    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::input::{prepare_strip_input, validate_prepared_strip_inputs};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should follow epoch")
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-cpu-template-strip-publication-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should be created");
            Self(path)
        }

        fn stage_count(&self) -> usize {
            fs::read_dir(&self.0)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".bangbang-cpu-template-helper.")
                })
                .count()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn prepare(paths: &[&Path], suffix: &str) -> Vec<PreparedStripInput> {
        let inputs = paths
            .iter()
            .map(|path| prepare_strip_input(path, suffix).unwrap().0)
            .collect::<Vec<_>>();
        validate_prepared_strip_inputs(&inputs).unwrap();
        inputs
    }

    #[test]
    fn publishes_absent_outputs_across_multiple_directories() {
        let first = TestDirectory::new();
        let second = TestDirectory::new();
        let first_input = first.0.join("cpu.json");
        let second_input = second.0.join("cpu.json");
        fs::write(&first_input, b"first-original").unwrap();
        fs::write(&second_input, b"second-original").unwrap();
        let inputs = prepare(&[&first_input, &second_input], "_stripped");

        publish_strip_artifacts(inputs, vec![b"first-new".to_vec(), b"second-new".to_vec()])
            .expect("publication should succeed");

        assert_eq!(fs::read(&first_input).unwrap(), b"first-original");
        assert_eq!(fs::read(&second_input).unwrap(), b"second-original");
        assert_eq!(
            fs::read(first.0.join("cpu_stripped.json")).unwrap(),
            b"first-new"
        );
        assert_eq!(
            fs::read(second.0.join("cpu_stripped.json")).unwrap(),
            b"second-new"
        );
        assert_eq!(first.stage_count(), 0);
        assert_eq!(second.stage_count(), 0);
    }

    #[test]
    fn replaces_exact_single_link_inputs_and_removes_old_inodes() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first.json");
        let second = directory.0.join("second.json");
        fs::write(&first, b"first-original").unwrap();
        fs::write(&second, b"second-original").unwrap();
        let first_inode = fs::metadata(&first).unwrap().ino();
        let second_inode = fs::metadata(&second).unwrap().ino();
        let inputs = prepare(&[&first, &second], "");

        publish_strip_artifacts(inputs, vec![b"first-new".to_vec(), b"second-new".to_vec()])
            .expect("replacement should succeed");

        assert_eq!(fs::read(&first).unwrap(), b"first-new");
        assert_eq!(fs::read(&second).unwrap(), b"second-new");
        assert_ne!(fs::metadata(&first).unwrap().ino(), first_inode);
        assert_ne!(fs::metadata(&second).unwrap().ino(), second_inode);
        assert_eq!(directory.stage_count(), 0);
    }

    #[test]
    fn rolls_back_every_observed_split_boundary_in_both_modes() {
        for suffix in ["_stripped", ""] {
            for fail_after in 1..=3 {
                let directory = TestDirectory::new();
                let paths = [
                    directory.0.join("first.json"),
                    directory.0.join("second.json"),
                    directory.0.join("third.json"),
                ];
                for (index, path) in paths.iter().enumerate() {
                    fs::write(path, format!("original-{index}")).unwrap();
                }
                let original_inodes = paths
                    .iter()
                    .map(|path| fs::metadata(path).unwrap().ino())
                    .collect::<Vec<_>>();
                let refs = paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
                let inputs = prepare(&refs, suffix);
                let error = publish_strip_with_faults(
                    inputs,
                    vec![b"new-0".to_vec(), b"new-1".to_vec(), b"new-2".to_vec()],
                    &StripFaults {
                        fail_after_commits: Some(fail_after),
                        ..StripFaults::default()
                    },
                )
                .expect_err("injected failure should roll back");
                assert_eq!(error, StripPublicationError::PublicationRolledBack);
                for (index, path) in paths.iter().enumerate() {
                    assert_eq!(
                        fs::read(path).unwrap(),
                        format!("original-{index}").as_bytes()
                    );
                    assert_eq!(fs::metadata(path).unwrap().ino(), original_inodes[index]);
                    if !suffix.is_empty() {
                        assert!(
                            !directory
                                .0
                                .join(format!(
                                    "{}_stripped.json",
                                    path.file_stem().unwrap().to_string_lossy()
                                ))
                                .exists()
                        );
                    }
                }
                assert_eq!(directory.stage_count(), 0);
            }
        }
    }

    #[test]
    fn uncertain_entry_does_not_prevent_other_owned_rollbacks() {
        let directory = TestDirectory::new();
        let paths = [
            directory.0.join("first.json"),
            directory.0.join("second.json"),
            directory.0.join("third.json"),
        ];
        for (index, path) in paths.iter().enumerate() {
            fs::write(path, format!("original-{index}")).unwrap();
        }
        let refs = paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let inputs = prepare(&refs, "");
        let error = publish_strip_with_faults(
            inputs,
            vec![b"new-0".to_vec(), b"new-1".to_vec(), b"new-2".to_vec()],
            &StripFaults {
                fail_after_commits: Some(3),
                fail_rollback_at: vec![2],
                ..StripFaults::default()
            },
        )
        .expect_err("rollback uncertainty should be reported");
        assert_eq!(error, StripPublicationError::RollbackUncertain);
        assert_eq!(fs::read(&paths[0]).unwrap(), b"original-0");
        assert_eq!(fs::read(&paths[1]).unwrap(), b"original-1");
        assert_eq!(fs::read(&paths[2]).unwrap(), b"new-2");
    }

    #[test]
    fn link_count_race_restores_all_paths_and_preserves_the_external_alias() {
        let directory = TestDirectory::new();
        let paths = [
            directory.0.join("first.json"),
            directory.0.join("second.json"),
            directory.0.join("third.json"),
        ];
        for (index, path) in paths.iter().enumerate() {
            fs::write(path, format!("original-{index}")).unwrap();
        }
        let original_inodes = paths
            .iter()
            .map(|path| fs::metadata(path).unwrap().ino())
            .collect::<Vec<_>>();
        let refs = paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        let inputs = prepare(&refs, "");

        let error = publish_strip_with_faults(
            inputs,
            vec![b"new-0".to_vec(), b"new-1".to_vec(), b"new-2".to_vec()],
            &StripFaults {
                fail_after_commits: Some(3),
                link_rollback_stage_at: Some(1),
                ..StripFaults::default()
            },
        )
        .expect_err("external link-count race should be uncertain");

        assert_eq!(error, StripPublicationError::RollbackUncertain);
        for (index, path) in paths.iter().enumerate() {
            assert_eq!(
                fs::read(path).unwrap(),
                format!("original-{index}").as_bytes()
            );
            assert_eq!(fs::metadata(path).unwrap().ino(), original_inodes[index]);
        }
        let alias = fs::read_dir(&directory.0)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bangbang-cpu-template-helper.race-link.")
            })
            .expect("external hard link must be preserved");
        assert_eq!(fs::read(alias.path()).unwrap(), b"original-1");
        assert_eq!(fs::metadata(&paths[1]).unwrap().nlink(), 2);
    }

    #[test]
    fn rejects_empty_mode_hardlinks_before_publication() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first.json");
        let alias = directory.0.join("alias.json");
        let second = directory.0.join("second.json");
        fs::write(&first, b"first").unwrap();
        fs::hard_link(&first, &alias).unwrap();
        fs::write(&second, b"second").unwrap();
        let inputs = [first.as_path(), second.as_path()]
            .iter()
            .map(|path| prepare_strip_input(path, "").unwrap().0)
            .collect::<Vec<_>>();
        assert_eq!(
            validate_prepared_strip_inputs(&inputs),
            Err(crate::input::StripInputError::SharedInput)
        );
        assert_eq!(fs::read(&first).unwrap(), b"first");
        assert_eq!(fs::read(&alias).unwrap(), b"first");
    }

    #[test]
    fn every_injected_prestage_failure_preserves_the_complete_batch() {
        for faults in [
            StripFaults {
                force_unsupported_directory: Some(0),
                ..StripFaults::default()
            },
            StripFaults {
                fail_stage_at: Some(0),
                ..StripFaults::default()
            },
            StripFaults {
                fail_write_at: Some(1),
                ..StripFaults::default()
            },
            StripFaults {
                fail_flush_at: Some(1),
                ..StripFaults::default()
            },
            StripFaults {
                fail_file_sync_at: Some(1),
                ..StripFaults::default()
            },
            StripFaults {
                fail_precommit_sync_directory: Some(0),
                ..StripFaults::default()
            },
        ] {
            let directory = TestDirectory::new();
            let first = directory.0.join("first.json");
            let second = directory.0.join("second.json");
            fs::write(&first, b"first-original").unwrap();
            fs::write(&second, b"second-original").unwrap();
            let inputs = prepare(&[&first, &second], "_stripped");

            let error = publish_strip_with_faults(
                inputs,
                vec![b"first-new".to_vec(), b"second-new".to_vec()],
                &faults,
            )
            .expect_err("injected precommit failure should fail");
            assert!(matches!(
                error,
                StripPublicationError::AtomicCommitUnsupported | StripPublicationError::Staging
            ));
            assert_eq!(fs::read(&first).unwrap(), b"first-original");
            assert_eq!(fs::read(&second).unwrap(), b"second-original");
            assert!(!directory.0.join("first_stripped.json").exists());
            assert!(!directory.0.join("second_stripped.json").exists());
            assert_eq!(directory.stage_count(), 0);
        }
    }

    #[test]
    fn commit_failures_are_original_or_confirmed_rollback_outcomes() {
        for fail_at in 0..3 {
            let directory = TestDirectory::new();
            let paths = [
                directory.0.join("first.json"),
                directory.0.join("second.json"),
                directory.0.join("third.json"),
            ];
            for (index, path) in paths.iter().enumerate() {
                fs::write(path, format!("original-{index}")).unwrap();
            }
            let refs = paths.iter().map(PathBuf::as_path).collect::<Vec<_>>();
            let inputs = prepare(&refs, "_stripped");
            let error = publish_strip_with_faults(
                inputs,
                vec![b"new-0".to_vec(), b"new-1".to_vec(), b"new-2".to_vec()],
                &StripFaults {
                    fail_commit_at: Some(fail_at),
                    ..StripFaults::default()
                },
            )
            .expect_err("injected commit should fail");
            assert_eq!(
                error,
                if fail_at == 0 {
                    StripPublicationError::Commit
                } else {
                    StripPublicationError::PublicationRolledBack
                }
            );
            for (index, path) in paths.iter().enumerate() {
                assert_eq!(
                    fs::read(path).unwrap(),
                    format!("original-{index}").as_bytes()
                );
                assert!(
                    !directory
                        .0
                        .join(format!(
                            "{}_stripped.json",
                            path.file_stem().unwrap().to_string_lossy()
                        ))
                        .exists()
                );
            }
            assert_eq!(directory.stage_count(), 0);
        }
    }

    #[test]
    fn racing_winner_is_preserved_while_prior_owned_output_is_restored() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first.json");
        let second = directory.0.join("second.json");
        fs::write(&first, b"first-original").unwrap();
        fs::write(&second, b"second-original").unwrap();
        let inputs = prepare(&[&first, &second], "_stripped");

        let error = publish_strip_with_faults(
            inputs,
            vec![b"first-new".to_vec(), b"second-new".to_vec()],
            &StripFaults {
                replace_output_before_commit: Some(1),
                ..StripFaults::default()
            },
        )
        .expect_err("racing winner should stop publication");
        assert_eq!(error, StripPublicationError::RollbackUncertain);
        assert!(!directory.0.join("first_stripped.json").exists());
        assert_eq!(
            fs::read(directory.0.join("second_stripped.json")).unwrap(),
            b"racing-winner"
        );
        assert_eq!(fs::read(&first).unwrap(), b"first-original");
        assert_eq!(fs::read(&second).unwrap(), b"second-original");
        assert_eq!(directory.stage_count(), 0);
    }

    #[test]
    fn unknown_private_stage_is_preserved_and_reported() {
        let directory = TestDirectory::new();
        let first = directory.0.join("first.json");
        let second = directory.0.join("second.json");
        fs::write(&first, b"first-original").unwrap();
        fs::write(&second, b"second-original").unwrap();
        let inputs = prepare(&[&first, &second], "_stripped");

        let error = publish_strip_with_faults(
            inputs,
            vec![b"first-new".to_vec(), b"second-new".to_vec()],
            &StripFaults {
                replace_stage_before_commit: Some(0),
                ..StripFaults::default()
            },
        )
        .expect_err("unknown stage should be uncertain");
        assert_eq!(error, StripPublicationError::RollbackUncertain);
        assert_eq!(directory.stage_count(), 1);
        assert!(!directory.0.join("first_stripped.json").exists());
        assert!(!directory.0.join("second_stripped.json").exists());
        let private = fs::read_dir(&directory.0)
            .unwrap()
            .filter_map(Result::ok)
            .find(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".bangbang-cpu-template-helper.stage.")
            })
            .expect("unknown stage should remain");
        assert_eq!(fs::read(private.path()).unwrap(), b"racing-replacement");
    }

    #[test]
    fn distinguishes_committed_durability_and_cleanup_uncertainty() {
        let durability_directory = TestDirectory::new();
        let first = durability_directory.0.join("first.json");
        let second = durability_directory.0.join("second.json");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let inputs = prepare(&[&first, &second], "");
        let error = publish_strip_with_faults(
            inputs,
            vec![b"new-first".to_vec(), b"new-second".to_vec()],
            &StripFaults {
                fail_postcommit_sync_directory: Some(0),
                ..StripFaults::default()
            },
        )
        .expect_err("sync uncertainty should be reported");
        assert_eq!(error, StripPublicationError::CommittedDurabilityUncertain);
        assert_eq!(fs::read(&first).unwrap(), b"new-first");
        assert_eq!(fs::read(&second).unwrap(), b"new-second");
        assert_eq!(durability_directory.stage_count(), 2);

        let cleanup_directory = TestDirectory::new();
        let first = cleanup_directory.0.join("first.json");
        let second = cleanup_directory.0.join("second.json");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        let inputs = prepare(&[&first, &second], "");
        let error = publish_strip_with_faults(
            inputs,
            vec![b"new-first".to_vec(), b"new-second".to_vec()],
            &StripFaults {
                fail_cleanup_at: vec![0],
                ..StripFaults::default()
            },
        )
        .expect_err("cleanup uncertainty should be reported");
        assert_eq!(error, StripPublicationError::CommittedCleanupUncertain);
        assert_eq!(fs::read(&first).unwrap(), b"new-first");
        assert_eq!(fs::read(&second).unwrap(), b"new-second");
        assert_eq!(cleanup_directory.stage_count(), 1);
    }

    #[test]
    fn errors_never_retain_paths_values_or_identities() {
        let sensitive = "private-template-value";
        for error in [
            StripPublicationError::ArtifactCountMismatch,
            StripPublicationError::ArtifactTooLarge,
            StripPublicationError::InputChanged,
            StripPublicationError::OutputInspection,
            StripPublicationError::Collision,
            StripPublicationError::AtomicCommitUnsupported,
            StripPublicationError::Staging,
            StripPublicationError::Commit,
            StripPublicationError::PrecommitCleanupUncertain,
            StripPublicationError::PublicationRolledBack,
            StripPublicationError::RollbackUncertain,
            StripPublicationError::CommittedDurabilityUncertain,
            StripPublicationError::CommittedCleanupUncertain,
        ] {
            assert!(!error.to_string().contains(sensitive));
            assert!(!format!("{error:?}").contains(sensitive));
        }
    }
}
