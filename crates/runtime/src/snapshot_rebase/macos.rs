#[cfg(test)]
use std::collections::VecDeque;
use std::ffi::CString;
use std::fs::{File, OpenOptions, Permissions};
use std::io::{self, Seek};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use crate::snapshot_diff_v2_13::{
    NATIVE_V2_DIFF_MAGIC, SnapshotV2DiffMaterializationBaseFile,
    SnapshotV2DiffMaterializationError, apply_snapshot_v2_diff_layer_file_with_cancel,
};
use crate::snapshot_memory_v2::{
    FileFacts, NATIVE_V2_MEMORY_MAGIC, inspect_file, inspect_file_facts,
    verify_snapshot_v2_memory_image_output,
};

use super::*;

const STAGING_PREFIX: &[u8] = b".bangbang-snapshot-rebase-";
const STAGING_RANDOM_BYTES: usize = 16;
const STAGING_CREATE_ATTEMPTS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

struct SplitInputPath {
    parent: PathBuf,
    component: CString,
}

struct AdoptedInput {
    role: SnapshotV2DiffRebaseInput,
    parent: PathBuf,
    directory: File,
    directory_identity: FileIdentity,
    component: CString,
    file: File,
    facts: FileFacts,
    identity: FileIdentity,
}

impl fmt::Debug for AdoptedInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdoptedInput")
            .field("role", &self.role)
            .field("parent", &REDACTED)
            .field("component", &REDACTED)
            .field("identity", &REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaseKind {
    Complete,
    ZeroRoot,
}

struct StagingFile<'directory> {
    directory: &'directory File,
    name: CString,
    file: File,
    result_identity: FileIdentity,
    verified_facts: Option<FileFacts>,
    cleanup_identity: FileIdentity,
    active: bool,
    cleanup_on_drop: bool,
}

impl fmt::Debug for StagingFile<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagingFile")
            .field("name", &REDACTED)
            .field("identity", &REDACTED)
            .field("active", &self.active)
            .finish()
    }
}

impl StagingFile<'_> {
    fn cleanup(&mut self) -> Option<SnapshotV2DiffRebaseCleanup> {
        if !self.active {
            return None;
        }
        self.cleanup_on_drop = false;
        let cleanup = clean_owned_entry(self.directory, &self.name, self.cleanup_identity);
        self.active = false;
        Some(cleanup)
    }

    fn committed(&mut self, displaced_identity: FileIdentity) {
        self.cleanup_identity = displaced_identity;
    }
}

impl Drop for StagingFile<'_> {
    fn drop(&mut self) {
        if self.active && self.cleanup_on_drop {
            let _ = clean_owned_entry(self.directory, &self.name, self.cleanup_identity);
        }
    }
}

struct SystemRebasePolicy<C> {
    is_cancelled: C,
}

impl<C> SystemRebasePolicy<C>
where
    C: FnMut(SnapshotV2DiffRebaseStage) -> bool,
{
    fn checkpoint(
        &mut self,
        stage: SnapshotV2DiffRebaseStage,
    ) -> Result<(), SnapshotV2DiffRebaseError> {
        enter_rebase_stage(stage)
            .map_err(|kind| precommit_error(stage, SnapshotV2DiffRebaseFailure::Io { kind }))?;
        if (self.is_cancelled)(stage) {
            Err(precommit_error(
                stage,
                SnapshotV2DiffRebaseFailure::Cancelled,
            ))
        } else {
            Ok(())
        }
    }
}

pub(super) fn rebase_snapshot_v2_diff_paths_macos<C>(
    paths: &SnapshotV2DiffRebasePaths,
    is_cancelled: C,
) -> Result<SnapshotV2DiffRebaseOutcome, SnapshotV2DiffRebaseError>
where
    C: FnMut(SnapshotV2DiffRebaseStage) -> bool,
{
    let mut policy = SystemRebasePolicy { is_cancelled };
    policy.checkpoint(SnapshotV2DiffRebaseStage::PlatformCheck)?;

    let base = adopt_input(paths.base(), SnapshotV2DiffRebaseInput::Base, &mut policy)?;
    let base_kind = classify_base(&base.file)?;
    let diff = adopt_input(paths.diff(), SnapshotV2DiffRebaseInput::Diff, &mut policy)?;

    policy.checkpoint(SnapshotV2DiffRebaseStage::SourceAliasCheck)?;
    if base.facts.same_object(diff.facts) {
        return Err(precommit_error(
            SnapshotV2DiffRebaseStage::SourceAliasCheck,
            SnapshotV2DiffRebaseFailure::SourceAlias,
        ));
    }

    policy.checkpoint(SnapshotV2DiffRebaseStage::SourceDuplication)?;
    let base_copy = duplicate_source(&base)?;
    let diff_copy = duplicate_source(&diff)?;

    policy.checkpoint(SnapshotV2DiffRebaseStage::StagingCreate)?;
    let mut staging = create_staging(&base.directory)?;
    let materialization_base = match base_kind {
        BaseKind::Complete => SnapshotV2DiffMaterializationBaseFile::Complete(base_copy),
        BaseKind::ZeroRoot => SnapshotV2DiffMaterializationBaseFile::ZeroRoot(base_copy),
    };

    if let Err(mut error) = policy.checkpoint(SnapshotV2DiffRebaseStage::Materialization) {
        error.staging_cleanup = staging.cleanup();
        return Err(error);
    }
    let materialization = apply_snapshot_v2_diff_layer_file_with_cancel(
        materialization_base,
        diff_copy,
        &mut staging.file,
        |_| (policy.is_cancelled)(SnapshotV2DiffRebaseStage::Materialization),
    );
    let binding = match materialization {
        Ok(binding) => binding,
        Err(SnapshotV2DiffMaterializationError::Cancelled { .. }) => {
            return Err(cleanup_precommit_error(
                &mut staging,
                precommit_error(
                    SnapshotV2DiffRebaseStage::Materialization,
                    SnapshotV2DiffRebaseFailure::Cancelled,
                ),
            ));
        }
        Err(source) => {
            return Err(cleanup_precommit_error(
                &mut staging,
                precommit_error(
                    SnapshotV2DiffRebaseStage::Materialization,
                    SnapshotV2DiffRebaseFailure::Materialization { source },
                ),
            ));
        }
    };

    if let Err(error) = sync_and_verify_result(&binding, &mut staging, &mut policy) {
        return Err(cleanup_precommit_error(&mut staging, error));
    }

    if let Err(error) = policy
        .checkpoint(SnapshotV2DiffRebaseStage::SourceStability)
        .and_then(|()| verify_sources(&base, &diff))
    {
        return Err(cleanup_precommit_error(&mut staging, error));
    }
    if let Err(error) = policy
        .checkpoint(SnapshotV2DiffRebaseStage::DirectoryStability)
        .and_then(|()| verify_directories(&base, &diff))
    {
        return Err(cleanup_precommit_error(&mut staging, error));
    }
    if let Err(error) = policy
        .checkpoint(SnapshotV2DiffRebaseStage::EntryStability)
        .and_then(|()| verify_entries(&base, &diff, &staging))
    {
        return Err(cleanup_precommit_error(&mut staging, error));
    }

    // This is the final caller-controlled checkpoint. Every identity check is
    // repeated after it and no callback, allocator, or test action runs between
    // those checks and the exchange syscall.
    if let Err(error) = policy.checkpoint(SnapshotV2DiffRebaseStage::AtomicExchange) {
        return Err(cleanup_precommit_error(&mut staging, error));
    }
    if let Err(error) = verify_sources(&base, &diff)
        .and_then(|()| verify_directories(&base, &diff))
        .and_then(|()| verify_entries(&base, &diff, &staging))
    {
        return Err(cleanup_precommit_error(&mut staging, error));
    }

    atomic_exchange(&base.directory, &staging.name, &base.component).map_err(|kind| {
        cleanup_precommit_error(
            &mut staging,
            precommit_error(
                SnapshotV2DiffRebaseStage::AtomicExchange,
                SnapshotV2DiffRebaseFailure::AtomicExchangeUnavailable { kind },
            ),
        )
    })?;

    // `RENAME_SWAP` is the commit point. From here onward this function cannot
    // return `Err`, attempt a compensating swap, or invoke caller code.
    staging.committed(base.identity);
    Ok(finish_committed_rebase(binding, &base, &diff, &mut staging))
}

fn adopt_input<C>(
    path: &Path,
    role: SnapshotV2DiffRebaseInput,
    policy: &mut SystemRebasePolicy<C>,
) -> Result<AdoptedInput, SnapshotV2DiffRebaseError>
where
    C: FnMut(SnapshotV2DiffRebaseStage) -> bool,
{
    let (path_stage, directory_stage, file_stage, validation_stage) = match role {
        SnapshotV2DiffRebaseInput::Base => (
            SnapshotV2DiffRebaseStage::BasePathValidation,
            SnapshotV2DiffRebaseStage::BaseDirectoryOpen,
            SnapshotV2DiffRebaseStage::BaseFileOpen,
            SnapshotV2DiffRebaseStage::BaseValidation,
        ),
        SnapshotV2DiffRebaseInput::Diff => (
            SnapshotV2DiffRebaseStage::DiffPathValidation,
            SnapshotV2DiffRebaseStage::DiffDirectoryOpen,
            SnapshotV2DiffRebaseStage::DiffFileOpen,
            SnapshotV2DiffRebaseStage::DiffValidation,
        ),
    };

    policy.checkpoint(path_stage)?;
    let split = split_input_path(path).ok_or_else(|| {
        precommit_error(
            path_stage,
            SnapshotV2DiffRebaseFailure::InvalidPath { input: role },
        )
    })?;

    policy.checkpoint(directory_stage)?;
    let directory = open_directory(&split.parent).map_err(|kind| {
        precommit_error(directory_stage, SnapshotV2DiffRebaseFailure::Io { kind })
    })?;
    let directory_identity = file_identity(&directory).map_err(|kind| {
        precommit_error(directory_stage, SnapshotV2DiffRebaseFailure::Io { kind })
    })?;

    policy.checkpoint(file_stage)?;
    let file = open_source(&directory, &split.component)
        .map_err(|kind| precommit_error(file_stage, SnapshotV2DiffRebaseFailure::Io { kind }))?;
    let identity = file_identity(&file)
        .map_err(|kind| precommit_error(file_stage, SnapshotV2DiffRebaseFailure::Io { kind }))?;

    policy.checkpoint(validation_stage)?;
    let facts = inspect_file(&file).map_err(|source| {
        precommit_error(
            validation_stage,
            SnapshotV2DiffRebaseFailure::Source {
                input: role,
                source,
            },
        )
    })?;
    if !facts.same_identity(identity.device, identity.inode) {
        return Err(precommit_error(
            validation_stage,
            SnapshotV2DiffRebaseFailure::SourceChanged { input: role },
        ));
    }
    if entry_identity(&directory, &split.component).map_err(|kind| {
        precommit_error(validation_stage, SnapshotV2DiffRebaseFailure::Io { kind })
    })? != Some(identity)
    {
        return Err(precommit_error(
            validation_stage,
            SnapshotV2DiffRebaseFailure::EntryChanged { input: role },
        ));
    }
    let after = inspect_file(&file).map_err(|source| {
        precommit_error(
            validation_stage,
            SnapshotV2DiffRebaseFailure::Source {
                input: role,
                source,
            },
        )
    })?;
    if after != facts {
        return Err(precommit_error(
            validation_stage,
            SnapshotV2DiffRebaseFailure::SourceChanged { input: role },
        ));
    }

    Ok(AdoptedInput {
        role,
        parent: split.parent,
        directory,
        directory_identity,
        component: split.component,
        file,
        facts,
        identity,
    })
}

fn classify_base(file: &File) -> Result<BaseKind, SnapshotV2DiffRebaseError> {
    let stage = SnapshotV2DiffRebaseStage::BaseValidation;
    let mut magic = [0_u8; 8];
    if read_exact_at(file, &mut magic, 0).is_err() {
        return Err(precommit_error(
            stage,
            SnapshotV2DiffRebaseFailure::InvalidBaseKind,
        ));
    }
    if magic == NATIVE_V2_MEMORY_MAGIC {
        Ok(BaseKind::Complete)
    } else if magic == NATIVE_V2_DIFF_MAGIC {
        Ok(BaseKind::ZeroRoot)
    } else {
        Err(precommit_error(
            stage,
            SnapshotV2DiffRebaseFailure::InvalidBaseKind,
        ))
    }
}

fn duplicate_source(input: &AdoptedInput) -> Result<File, SnapshotV2DiffRebaseError> {
    let stage = SnapshotV2DiffRebaseStage::SourceDuplication;
    let copy = input.file.try_clone().map_err(|source| {
        precommit_error(
            stage,
            SnapshotV2DiffRebaseFailure::Io {
                kind: source.kind(),
            },
        )
    })?;
    let facts = inspect_file(&copy).map_err(|source| {
        precommit_error(
            stage,
            SnapshotV2DiffRebaseFailure::Source {
                input: input.role,
                source,
            },
        )
    })?;
    if facts != input.facts {
        return Err(precommit_error(
            stage,
            SnapshotV2DiffRebaseFailure::SourceChanged { input: input.role },
        ));
    }
    Ok(copy)
}

fn create_staging(directory: &File) -> Result<StagingFile<'_>, SnapshotV2DiffRebaseError> {
    let stage = SnapshotV2DiffRebaseStage::StagingCreate;
    for _ in 0..STAGING_CREATE_ATTEMPTS {
        let name = staging_name()?;
        match open_staging(directory, &name) {
            Ok(file) => {
                let identity = match file_identity(&file) {
                    Ok(identity) => identity,
                    Err(kind) => {
                        let mut error =
                            precommit_error(stage, SnapshotV2DiffRebaseFailure::Io { kind });
                        error.staging_cleanup = Some(SnapshotV2DiffRebaseCleanup::Failed(kind));
                        return Err(error);
                    }
                };
                let mut staging = StagingFile {
                    directory,
                    name,
                    file,
                    result_identity: identity,
                    verified_facts: None,
                    cleanup_identity: identity,
                    active: true,
                    cleanup_on_drop: true,
                };
                if let Err(source) = staging.file.set_permissions(Permissions::from_mode(0o600)) {
                    let error = precommit_error(
                        stage,
                        SnapshotV2DiffRebaseFailure::Io {
                            kind: source.kind(),
                        },
                    );
                    return Err(cleanup_precommit_error(&mut staging, error));
                }
                let permissions = staging.file.metadata().map_err(|source| {
                    cleanup_precommit_error(
                        &mut staging,
                        precommit_error(
                            stage,
                            SnapshotV2DiffRebaseFailure::Io {
                                kind: source.kind(),
                            },
                        ),
                    )
                })?;
                let facts = inspect_file_facts(&staging.file).map_err(|source| {
                    cleanup_precommit_error(
                        &mut staging,
                        precommit_error(
                            stage,
                            SnapshotV2DiffRebaseFailure::StagingValidation { source },
                        ),
                    )
                })?;
                let position = staging.file.stream_position().map_err(|source| {
                    cleanup_precommit_error(
                        &mut staging,
                        precommit_error(
                            stage,
                            SnapshotV2DiffRebaseFailure::Io {
                                kind: source.kind(),
                            },
                        ),
                    )
                })?;
                if permissions.mode() & 0o7777 != 0o600
                    || !facts.is_regular()
                    || !facts.is_read_write()
                    || !facts.is_close_on_exec()
                    || facts.is_append()
                    || facts.length() != 0
                    || position != 0
                {
                    let error = precommit_error(
                        stage,
                        SnapshotV2DiffRebaseFailure::Io {
                            kind: io::ErrorKind::InvalidData,
                        },
                    );
                    return Err(cleanup_precommit_error(&mut staging, error));
                }
                return Ok(staging);
            }
            Err(io::ErrorKind::AlreadyExists) => {}
            Err(kind) => {
                return Err(precommit_error(
                    stage,
                    SnapshotV2DiffRebaseFailure::Io { kind },
                ));
            }
        }
    }
    Err(precommit_error(
        stage,
        SnapshotV2DiffRebaseFailure::Io {
            kind: io::ErrorKind::AlreadyExists,
        },
    ))
}

fn sync_and_verify_result<C>(
    binding: &SnapshotV2MemoryBinding,
    staging: &mut StagingFile<'_>,
    policy: &mut SystemRebasePolicy<C>,
) -> Result<(), SnapshotV2DiffRebaseError>
where
    C: FnMut(SnapshotV2DiffRebaseStage) -> bool,
{
    let stage = SnapshotV2DiffRebaseStage::ResultFileSync;
    policy.checkpoint(stage)?;
    staging.file.sync_all().map_err(|source| {
        precommit_error(
            stage,
            SnapshotV2DiffRebaseFailure::Io {
                kind: source.kind(),
            },
        )
    })?;
    verify_snapshot_v2_memory_image_output(binding, &mut staging.file).map_err(|source| {
        precommit_error(
            stage,
            SnapshotV2DiffRebaseFailure::ResultVerification { source },
        )
    })?;
    if file_identity(&staging.file)
        .map_err(|kind| precommit_error(stage, SnapshotV2DiffRebaseFailure::Io { kind }))?
        != staging.result_identity
    {
        return Err(precommit_error(
            stage,
            SnapshotV2DiffRebaseFailure::StagingChanged,
        ));
    }
    staging.verified_facts = Some(inspect_file_facts(&staging.file).map_err(|source| {
        precommit_error(
            stage,
            SnapshotV2DiffRebaseFailure::StagingValidation { source },
        )
    })?);
    Ok(())
}

fn verify_sources(
    base: &AdoptedInput,
    diff: &AdoptedInput,
) -> Result<(), SnapshotV2DiffRebaseError> {
    for input in [base, diff] {
        if inspect_file(&input.file).ok() != Some(input.facts) {
            return Err(precommit_error(
                SnapshotV2DiffRebaseStage::SourceStability,
                SnapshotV2DiffRebaseFailure::SourceChanged { input: input.role },
            ));
        }
    }
    Ok(())
}

fn verify_directories(
    base: &AdoptedInput,
    diff: &AdoptedInput,
) -> Result<(), SnapshotV2DiffRebaseError> {
    for input in [base, diff] {
        let retained = file_identity(&input.directory).ok();
        let reopened = open_directory(&input.parent)
            .ok()
            .and_then(|directory| file_identity(&directory).ok());
        if retained != Some(input.directory_identity) || reopened != Some(input.directory_identity)
        {
            return Err(precommit_error(
                SnapshotV2DiffRebaseStage::DirectoryStability,
                SnapshotV2DiffRebaseFailure::DirectoryChanged { input: input.role },
            ));
        }
    }
    Ok(())
}

fn verify_entries(
    base: &AdoptedInput,
    diff: &AdoptedInput,
    staging: &StagingFile<'_>,
) -> Result<(), SnapshotV2DiffRebaseError> {
    if base.identity == diff.identity {
        return Err(precommit_error(
            SnapshotV2DiffRebaseStage::EntryStability,
            SnapshotV2DiffRebaseFailure::SourceAlias,
        ));
    }
    if staging.result_identity == base.identity || staging.result_identity == diff.identity {
        return Err(precommit_error(
            SnapshotV2DiffRebaseStage::EntryStability,
            SnapshotV2DiffRebaseFailure::StagingChanged,
        ));
    }
    for input in [base, diff] {
        if entry_identity(&input.directory, &input.component)
            .ok()
            .flatten()
            != Some(input.identity)
        {
            return Err(precommit_error(
                SnapshotV2DiffRebaseStage::EntryStability,
                SnapshotV2DiffRebaseFailure::EntryChanged { input: input.role },
            ));
        }
    }
    if entry_identity(staging.directory, &staging.name)
        .ok()
        .flatten()
        != Some(staging.result_identity)
    {
        return Err(precommit_error(
            SnapshotV2DiffRebaseStage::EntryStability,
            SnapshotV2DiffRebaseFailure::StagingChanged,
        ));
    }
    if staging.verified_facts.is_none()
        || inspect_file_facts(&staging.file).ok() != staging.verified_facts
    {
        return Err(precommit_error(
            SnapshotV2DiffRebaseStage::EntryStability,
            SnapshotV2DiffRebaseFailure::StagingChanged,
        ));
    }
    Ok(())
}

fn cleanup_precommit_error(
    staging: &mut StagingFile<'_>,
    mut error: SnapshotV2DiffRebaseError,
) -> SnapshotV2DiffRebaseError {
    error.staging_cleanup = staging.cleanup();
    error
}

fn finish_committed_rebase(
    binding: SnapshotV2MemoryBinding,
    base: &AdoptedInput,
    diff: &AdoptedInput,
    staging: &mut StagingFile<'_>,
) -> SnapshotV2DiffRebaseOutcome {
    let mut first_uncertainty = None;

    if let Err(kind) = enter_rebase_stage(SnapshotV2DiffRebaseStage::CommitVerification) {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotV2DiffRebaseStage::CommitVerification,
            SnapshotV2DiffRebaseCommitFailure::Io(kind),
        );
    }
    for input in [base, diff] {
        let retained = file_identity(&input.directory).ok();
        let reopened = open_directory(&input.parent)
            .ok()
            .and_then(|directory| file_identity(&directory).ok());
        if retained != Some(input.directory_identity) || reopened != Some(input.directory_identity)
        {
            record_uncertainty(
                &mut first_uncertainty,
                SnapshotV2DiffRebaseStage::CommitVerification,
                SnapshotV2DiffRebaseCommitFailure::DirectoryChanged { input: input.role },
            );
        }
    }
    if entry_identity(&base.directory, &base.component)
        .ok()
        .flatten()
        != Some(staging.result_identity)
    {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotV2DiffRebaseStage::CommitVerification,
            SnapshotV2DiffRebaseCommitFailure::BaseEntryChanged,
        );
    }
    if entry_identity(staging.directory, &staging.name)
        .ok()
        .flatten()
        != Some(base.identity)
    {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotV2DiffRebaseStage::CommitVerification,
            SnapshotV2DiffRebaseCommitFailure::DisplacedEntryChanged,
        );
    }
    if inspect_file(&diff.file).ok() != Some(diff.facts)
        || entry_identity(&diff.directory, &diff.component)
            .ok()
            .flatten()
            != Some(diff.identity)
    {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotV2DiffRebaseStage::CommitVerification,
            SnapshotV2DiffRebaseCommitFailure::DiffChanged,
        );
    }
    if file_identity(&base.file).ok() != Some(base.identity) {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotV2DiffRebaseStage::CommitVerification,
            SnapshotV2DiffRebaseCommitFailure::BaseSourceChanged,
        );
    }

    let cleanup = match enter_rebase_stage(SnapshotV2DiffRebaseStage::DisplacedCleanup) {
        Ok(()) => staging
            .cleanup()
            .unwrap_or(SnapshotV2DiffRebaseCleanup::AlreadyAbsent),
        Err(kind) => {
            staging.cleanup_on_drop = false;
            staging.active = false;
            SnapshotV2DiffRebaseCleanup::Failed(kind)
        }
    };
    if matches!(
        cleanup,
        SnapshotV2DiffRebaseCleanup::ChangedRefused | SnapshotV2DiffRebaseCleanup::Failed(_)
    ) {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotV2DiffRebaseStage::DisplacedCleanup,
            SnapshotV2DiffRebaseCommitFailure::Cleanup,
        );
    }

    let directory_sync = match enter_rebase_stage(SnapshotV2DiffRebaseStage::BaseDirectorySync) {
        Ok(()) => base.directory.sync_all().err().map(|source| source.kind()),
        Err(kind) => Some(kind),
    };
    if let Some(kind) = directory_sync {
        record_uncertainty(
            &mut first_uncertainty,
            SnapshotV2DiffRebaseStage::BaseDirectorySync,
            SnapshotV2DiffRebaseCommitFailure::Io(kind),
        );
    }

    let _ = enter_rebase_stage(SnapshotV2DiffRebaseStage::Complete);
    let commit = match first_uncertainty {
        None => SnapshotV2DiffRebaseCommit::Durable,
        Some((stage, failure)) => SnapshotV2DiffRebaseCommit::Uncertain {
            stage,
            failure,
            cleanup,
            directory_sync,
        },
    };
    SnapshotV2DiffRebaseOutcome { binding, commit }
}

fn record_uncertainty(
    first: &mut Option<(SnapshotV2DiffRebaseStage, SnapshotV2DiffRebaseCommitFailure)>,
    stage: SnapshotV2DiffRebaseStage,
    failure: SnapshotV2DiffRebaseCommitFailure,
) {
    if first.is_none() {
        *first = Some((stage, failure));
    }
}

fn split_input_path(path: &Path) -> Option<SplitInputPath> {
    let raw_path = path.as_os_str().as_bytes();
    if raw_path.contains(&0) {
        return None;
    }
    let raw_component = raw_path.rsplit(|byte| *byte == b'/').next()?;
    if raw_component.is_empty() || raw_component == b"." || raw_component == b".." {
        return None;
    }
    let Component::Normal(component) = path.components().next_back()? else {
        return None;
    };
    if component.as_bytes() != raw_component {
        return None;
    }
    let component = CString::new(raw_component).ok()?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    };
    Some(SplitInputPath { parent, component })
}

fn open_directory(path: &Path) -> Result<File, io::ErrorKind> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(path)
        .map_err(|source| source.kind())
}

fn open_source(directory: &File, component: &CString) -> Result<File, io::ErrorKind> {
    // SAFETY: `directory` is live and `component` is one NUL-terminated final
    // component. `O_NOFOLLOW` rejects a final symlink, and success returns a
    // fresh descriptor owned by this function.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error().kind())
    } else {
        // SAFETY: successful `openat` returned a fresh owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn open_staging(directory: &File, name: &CString) -> Result<File, io::ErrorKind> {
    // SAFETY: `directory` is live and `name` is a generated NUL-terminated
    // component. The flags create one private, exclusive regular file and a
    // successful call returns a fresh owned descriptor.
    #[cfg(test)]
    let mode: libc::mode_t =
        REBASE_TEST_HOOK.with(|hook| hook.borrow().staging_mode.unwrap_or(0o600));
    #[cfg(not(test))]
    let mode: libc::mode_t = 0o600;
    // SAFETY: the descriptor, component, and flag invariants are stated above;
    // `mode_t` is explicitly promoted for the variadic mode argument.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            libc::c_uint::from(mode),
        )
    };
    if descriptor < 0 {
        Err(io::Error::last_os_error().kind())
    } else {
        // SAFETY: successful `openat` returned a fresh owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

fn staging_name() -> Result<CString, SnapshotV2DiffRebaseError> {
    let stage = SnapshotV2DiffRebaseStage::StagingCreate;
    let mut random = [0_u8; STAGING_RANDOM_BYTES];
    fill_staging_random(&mut random)
        .map_err(|()| precommit_error(stage, SnapshotV2DiffRebaseFailure::RandomnessUnavailable))?;
    let mut bytes = Vec::with_capacity(STAGING_PREFIX.len() + STAGING_RANDOM_BYTES * 2);
    bytes.extend_from_slice(STAGING_PREFIX);
    for byte in random {
        bytes.push(hex_digit(byte >> 4));
        bytes.push(hex_digit(byte & 0x0f));
    }
    CString::new(bytes)
        .map_err(|_| precommit_error(stage, SnapshotV2DiffRebaseFailure::RandomnessUnavailable))
}

fn fill_staging_random(destination: &mut [u8; STAGING_RANDOM_BYTES]) -> Result<(), ()> {
    #[cfg(test)]
    if REBASE_TEST_HOOK.with(|hook| hook.borrow().random_failure) {
        return Err(());
    }
    #[cfg(test)]
    if let Some(random) = REBASE_TEST_HOOK.with(|hook| hook.borrow_mut().random_names.pop_front()) {
        *destination = random;
        return Ok(());
    }
    getrandom::fill(destination).map_err(|_| ())
}

const fn hex_digit(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + (nibble - 10),
    }
}

fn file_identity(file: &File) -> Result<FileIdentity, io::ErrorKind> {
    let metadata = file.metadata().map_err(|source| source.kind())?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

fn entry_identity(directory: &File, name: &CString) -> Result<Option<FileIdentity>, io::ErrorKind> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: all descriptors and pointers are live for the call, and
    // `AT_SYMLINK_NOFOLLOW` observes the entry rather than a symlink target.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // SAFETY: successful `fstatat` initialized the complete structure.
        let stat = unsafe { stat.assume_init() };
        return Ok(Some(FileIdentity {
            device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
            inode: stat.st_ino,
        }));
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(error.kind())
    }
}

fn atomic_exchange(
    directory: &File,
    staging: &CString,
    base: &CString,
) -> Result<(), io::ErrorKind> {
    #[cfg(test)]
    if REBASE_TEST_HOOK.with(|hook| hook.borrow().exchange_failure) {
        return Err(io::ErrorKind::Other);
    }
    // SAFETY: both names are retained NUL-terminated single components in the
    // same retained directory. `RENAME_SWAP` performs one atomic exchange.
    let result = unsafe {
        libc::renameatx_np(
            directory.as_raw_fd(),
            staging.as_ptr(),
            directory.as_raw_fd(),
            base.as_ptr(),
            libc::RENAME_SWAP,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().kind())
    }
}

fn clean_owned_entry(
    directory: &File,
    name: &CString,
    expected: FileIdentity,
) -> SnapshotV2DiffRebaseCleanup {
    match entry_identity(directory, name) {
        Ok(None) => SnapshotV2DiffRebaseCleanup::AlreadyAbsent,
        Ok(Some(actual)) if actual != expected => SnapshotV2DiffRebaseCleanup::ChangedRefused,
        Ok(Some(_)) => {
            // SAFETY: the immediately preceding identity check matched the
            // retained expected inode. Darwin has no identity-conditioned
            // unlink, so the documented trusted-directory boundary applies.
            let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
            if result == 0 {
                SnapshotV2DiffRebaseCleanup::Removed
            } else {
                let kind = io::Error::last_os_error().kind();
                if kind == io::ErrorKind::NotFound {
                    SnapshotV2DiffRebaseCleanup::AlreadyAbsent
                } else {
                    SnapshotV2DiffRebaseCleanup::Failed(kind)
                }
            }
        }
        Err(kind) => SnapshotV2DiffRebaseCleanup::Failed(kind),
    }
}

fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        let count = file.read_at(bytes, offset)?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "descriptor read made no progress",
            ));
        }
        offset = offset
            .checked_add(u64::try_from(count).map_err(|_| io::ErrorKind::InvalidData)?)
            .ok_or(io::ErrorKind::InvalidData)?;
        bytes = bytes.get_mut(count..).ok_or(io::ErrorKind::InvalidData)?;
    }
    Ok(())
}

#[cfg(test)]
#[derive(Debug)]
enum RebaseTestAction {
    ReplacePath {
        stage: SnapshotV2DiffRebaseStage,
        path: PathBuf,
        moved: PathBuf,
        replacement: Vec<u8>,
    },
    MovePath {
        stage: SnapshotV2DiffRebaseStage,
        path: PathBuf,
        moved: PathBuf,
    },
    ReplaceParent {
        stage: SnapshotV2DiffRebaseStage,
        parent: PathBuf,
        moved: PathBuf,
    },
    ReplaceStaging {
        stage: SnapshotV2DiffRebaseStage,
        directory: PathBuf,
        moved: PathBuf,
        replacement: Vec<u8>,
    },
    MoveStaging {
        stage: SnapshotV2DiffRebaseStage,
        directory: PathBuf,
        moved: PathBuf,
    },
    CorruptStaging {
        stage: SnapshotV2DiffRebaseStage,
        directory: PathBuf,
        bytes: Vec<u8>,
    },
}

#[cfg(test)]
impl RebaseTestAction {
    fn stage(&self) -> SnapshotV2DiffRebaseStage {
        match self {
            Self::ReplacePath { stage, .. }
            | Self::MovePath { stage, .. }
            | Self::ReplaceParent { stage, .. }
            | Self::ReplaceStaging { stage, .. }
            | Self::MoveStaging { stage, .. }
            | Self::CorruptStaging { stage, .. } => *stage,
        }
    }

    fn perform(self) -> Result<(), io::ErrorKind> {
        match self {
            Self::ReplacePath {
                path,
                moved,
                replacement,
                ..
            } => {
                std::fs::rename(&path, moved).map_err(|source| source.kind())?;
                std::fs::write(path, replacement).map_err(|source| source.kind())
            }
            Self::MovePath { path, moved, .. } => {
                std::fs::rename(path, moved).map_err(|source| source.kind())
            }
            Self::ReplaceParent { parent, moved, .. } => {
                std::fs::rename(&parent, moved).map_err(|source| source.kind())?;
                std::fs::create_dir(parent).map_err(|source| source.kind())
            }
            Self::ReplaceStaging {
                directory,
                moved,
                replacement,
                ..
            } => {
                let staging = find_staging_path(&directory)?;
                std::fs::rename(&staging, moved).map_err(|source| source.kind())?;
                std::fs::write(staging, replacement).map_err(|source| source.kind())
            }
            Self::MoveStaging {
                directory, moved, ..
            } => std::fs::rename(find_staging_path(&directory)?, moved)
                .map_err(|source| source.kind()),
            Self::CorruptStaging {
                directory, bytes, ..
            } => std::fs::write(find_staging_path(&directory)?, bytes)
                .map_err(|source| source.kind()),
        }
    }
}

#[cfg(test)]
fn find_staging_path(directory: &Path) -> Result<PathBuf, io::ErrorKind> {
    std::fs::read_dir(directory)
        .map_err(|source| source.kind())?
        .filter_map(Result::ok)
        .find(|entry| entry.file_name().as_bytes().starts_with(STAGING_PREFIX))
        .map(|entry| entry.path())
        .ok_or(io::ErrorKind::NotFound)
}

#[cfg(test)]
#[derive(Debug, Default)]
struct RebaseTestHook {
    failures: Vec<SnapshotV2DiffRebaseStage>,
    action: Option<RebaseTestAction>,
    random_names: VecDeque<[u8; STAGING_RANDOM_BYTES]>,
    random_failure: bool,
    exchange_failure: bool,
    staging_mode: Option<libc::mode_t>,
    order: Vec<SnapshotV2DiffRebaseStage>,
}

#[cfg(test)]
thread_local! {
    static REBASE_TEST_HOOK: std::cell::RefCell<RebaseTestHook> =
        std::cell::RefCell::new(RebaseTestHook::default());
}

fn enter_rebase_stage(stage: SnapshotV2DiffRebaseStage) -> Result<(), io::ErrorKind> {
    #[cfg(test)]
    {
        let action = REBASE_TEST_HOOK.with(|hook| {
            let mut hook = hook.borrow_mut();
            hook.order.push(stage);
            if hook.failures.contains(&stage) {
                return Err(io::ErrorKind::Other);
            }
            if hook.action.as_ref().map(RebaseTestAction::stage) == Some(stage) {
                Ok(hook.action.take())
            } else {
                Ok(None)
            }
        })?;
        action.map_or(Ok(()), RebaseTestAction::perform)
    }
    #[cfg(not(test))]
    {
        let _ = stage;
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn with_rebase_failures<T>(
    failures: Vec<SnapshotV2DiffRebaseStage>,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotV2DiffRebaseStage>) {
    REBASE_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = RebaseTestHook {
            failures,
            ..RebaseTestHook::default()
        };
    });
    let result = operation();
    let order = REBASE_TEST_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        let order = std::mem::take(&mut hook.order);
        *hook = RebaseTestHook::default();
        order
    });
    (result, order)
}

#[cfg(test)]
pub(super) fn with_staging_random_failure<T>(operation: impl FnOnce() -> T) -> T {
    REBASE_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = RebaseTestHook {
            random_failure: true,
            ..RebaseTestHook::default()
        };
    });
    let result = operation();
    REBASE_TEST_HOOK.with(|hook| *hook.borrow_mut() = RebaseTestHook::default());
    result
}

#[cfg(test)]
pub(super) fn with_staging_random_names<T>(
    names: Vec<[u8; STAGING_RANDOM_BYTES]>,
    operation: impl FnOnce() -> T,
) -> T {
    REBASE_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = RebaseTestHook {
            random_names: names.into(),
            ..RebaseTestHook::default()
        };
    });
    let result = operation();
    REBASE_TEST_HOOK.with(|hook| *hook.borrow_mut() = RebaseTestHook::default());
    result
}

#[cfg(test)]
pub(super) fn with_exchange_failure<T>(operation: impl FnOnce() -> T) -> T {
    REBASE_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = RebaseTestHook {
            exchange_failure: true,
            ..RebaseTestHook::default()
        };
    });
    let result = operation();
    REBASE_TEST_HOOK.with(|hook| *hook.borrow_mut() = RebaseTestHook::default());
    result
}

#[cfg(test)]
pub(super) fn with_path_replacement<T>(
    stage: SnapshotV2DiffRebaseStage,
    path: PathBuf,
    moved: PathBuf,
    replacement: Vec<u8>,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotV2DiffRebaseStage>) {
    with_rebase_action(
        RebaseTestAction::ReplacePath {
            stage,
            path,
            moved,
            replacement,
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_path_removal<T>(
    stage: SnapshotV2DiffRebaseStage,
    path: PathBuf,
    moved: PathBuf,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotV2DiffRebaseStage>) {
    with_rebase_action(RebaseTestAction::MovePath { stage, path, moved }, operation)
}

#[cfg(test)]
pub(super) fn with_parent_replacement<T>(
    stage: SnapshotV2DiffRebaseStage,
    parent: PathBuf,
    moved: PathBuf,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotV2DiffRebaseStage>) {
    with_rebase_action(
        RebaseTestAction::ReplaceParent {
            stage,
            parent,
            moved,
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_staging_replacement<T>(
    stage: SnapshotV2DiffRebaseStage,
    directory: PathBuf,
    moved: PathBuf,
    replacement: Vec<u8>,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotV2DiffRebaseStage>) {
    with_rebase_action(
        RebaseTestAction::ReplaceStaging {
            stage,
            directory,
            moved,
            replacement,
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_staging_removal<T>(
    stage: SnapshotV2DiffRebaseStage,
    directory: PathBuf,
    moved: PathBuf,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotV2DiffRebaseStage>) {
    with_rebase_action(
        RebaseTestAction::MoveStaging {
            stage,
            directory,
            moved,
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_staging_corruption<T>(
    stage: SnapshotV2DiffRebaseStage,
    directory: PathBuf,
    bytes: Vec<u8>,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotV2DiffRebaseStage>) {
    with_rebase_action(
        RebaseTestAction::CorruptStaging {
            stage,
            directory,
            bytes,
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_staging_mode<T>(mode: libc::mode_t, operation: impl FnOnce() -> T) -> T {
    REBASE_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = RebaseTestHook {
            staging_mode: Some(mode),
            ..RebaseTestHook::default()
        };
    });
    let result = operation();
    REBASE_TEST_HOOK.with(|hook| *hook.borrow_mut() = RebaseTestHook::default());
    result
}

#[cfg(test)]
fn with_rebase_action<T>(
    action: RebaseTestAction,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotV2DiffRebaseStage>) {
    REBASE_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = RebaseTestHook {
            action: Some(action),
            ..RebaseTestHook::default()
        };
    });
    let result = operation();
    let order = REBASE_TEST_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        let order = std::mem::take(&mut hook.order);
        *hook = RebaseTestHook::default();
        order
    });
    (result, order)
}
