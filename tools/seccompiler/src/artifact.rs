use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{
    AtFlags, FileType, Mode, OFlags, RenameFlags, fstat, fsync, open, openat, renameat_with,
    statat, unlinkat,
};
use rustix::io::Errno;

static PRIVATE_NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) struct Artifact {
    pub(super) name: OsString,
    pub(super) bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PublicationError {
    OutputDirectoryOpen,
    OutputInspection,
    UnsafeOutputType,
    AtomicPublicationUnsupported,
    Staging,
    PrecommitCleanupUncertain,
    PublicationRolledBack,
    RollbackUncertain,
    CommittedDurabilityUncertain,
    CommittedCleanupUncertain,
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OutputDirectoryOpen => "output directory could not be opened safely",
            Self::OutputInspection => "output target could not be inspected safely",
            Self::UnsafeOutputType => "output target is not absent or a regular file",
            Self::AtomicPublicationUnsupported => {
                "output filesystem lacks required atomic rename operations"
            }
            Self::Staging => "complete output artifacts could not be staged",
            Self::PrecommitCleanupUncertain => {
                "output was not committed, but private staging cleanup is uncertain"
            }
            Self::PublicationRolledBack => "output publication failed and was rolled back",
            Self::RollbackUncertain => "output publication failed and rollback is uncertain",
            Self::CommittedDurabilityUncertain => {
                "output was committed, but directory durability is uncertain"
            }
            Self::CommittedCleanupUncertain => {
                "output was committed, but private staging cleanup is uncertain"
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Original {
    Absent,
    Existing(Identity),
}

#[derive(Debug)]
struct StagedArtifact {
    final_name: OsString,
    stage_name: OsString,
    staged_identity: Identity,
    original: Original,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrivateCreationError {
    Operation,
    CleanupUncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PublishOneError {
    NoMutation,
    MutationUncertain,
}

#[derive(Clone, Copy, Debug, Default)]
struct Faults {
    fail_capability_probe: bool,
    fail_after_publications: Option<usize>,
    fail_rollback: bool,
    fail_commit_sync: bool,
    fail_cleanup: bool,
    #[cfg(test)]
    replace_before_publication: Option<usize>,
}

pub(super) fn publish(directory: &Path, artifacts: &[Artifact]) -> Result<(), PublicationError> {
    publish_with_faults(directory, artifacts, Faults::default())
}

fn publish_with_faults(
    directory: &Path,
    artifacts: &[Artifact],
    faults: Faults,
) -> Result<(), PublicationError> {
    let directory_fd = open(
        directory,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| PublicationError::OutputDirectoryOpen)?;

    let originals = inspect_targets(&directory_fd, artifacts)?;
    let reserved_names = artifacts
        .iter()
        .map(|artifact| artifact.name.clone())
        .collect::<Vec<_>>();
    if faults.fail_capability_probe {
        return Err(PublicationError::AtomicPublicationUnsupported);
    }
    probe_rename_capabilities(&directory_fd, &originals, &reserved_names)?;

    let mut staged = Vec::with_capacity(artifacts.len());
    for (artifact, original) in artifacts.iter().zip(originals) {
        match stage_artifact(&directory_fd, artifact, original, &reserved_names) {
            Ok(entry) => staged.push(entry),
            Err(error) => {
                return if cleanup_stages(&directory_fd, &staged).is_ok() {
                    Err(error)
                } else {
                    Err(PublicationError::PrecommitCleanupUncertain)
                };
            }
        }
    }

    if fsync(&directory_fd).is_err() {
        return if cleanup_stages(&directory_fd, &staged).is_ok() && fsync(&directory_fd).is_ok() {
            Err(PublicationError::Staging)
        } else {
            Err(PublicationError::PrecommitCleanupUncertain)
        };
    }

    let mut published = 0;
    while published < staged.len() {
        let Some(entry) = staged.get(published) else {
            return Err(PublicationError::RollbackUncertain);
        };
        #[cfg(test)]
        if faults.replace_before_publication == Some(published) {
            inject_replacement(directory, entry)?;
        }
        match publish_one(&directory_fd, entry) {
            Ok(()) => {}
            Err(PublishOneError::NoMutation) => {
                return rollback_after_failure(
                    &directory_fd,
                    &staged,
                    published,
                    faults.fail_rollback,
                );
            }
            Err(PublishOneError::MutationUncertain) => {
                return Err(PublicationError::RollbackUncertain);
            }
        }
        published += 1;
        if faults.fail_after_publications == Some(published) {
            return rollback_after_failure(&directory_fd, &staged, published, faults.fail_rollback);
        }
    }

    if faults.fail_commit_sync || fsync(&directory_fd).is_err() {
        return Err(PublicationError::CommittedDurabilityUncertain);
    }

    if faults.fail_cleanup || cleanup_committed_stages(&directory_fd, &staged).is_err() {
        return Err(PublicationError::CommittedCleanupUncertain);
    }
    if fsync(&directory_fd).is_err() {
        return Err(PublicationError::CommittedCleanupUncertain);
    }
    Ok(())
}

fn inspect_targets<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    artifacts: &[Artifact],
) -> Result<Vec<Original>, PublicationError> {
    artifacts
        .iter()
        .map(
            |artifact| match stat_identity(directory_fd, &artifact.name) {
                Ok(None) => Ok(Original::Absent),
                Ok(Some((identity, FileType::RegularFile))) => Ok(Original::Existing(identity)),
                Ok(Some(_)) => Err(PublicationError::UnsafeOutputType),
                Err(()) => Err(PublicationError::OutputInspection),
            },
        )
        .collect()
}

fn probe_rename_capabilities<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    originals: &[Original],
    reserved_names: &[OsString],
) -> Result<(), PublicationError> {
    let needs_exchange = originals
        .iter()
        .any(|original| matches!(original, Original::Existing(_)));
    let needs_noreplace = originals
        .iter()
        .any(|original| matches!(original, Original::Absent));
    if !needs_exchange && !needs_noreplace {
        return Ok(());
    }

    let first = create_private(directory_fd, "probe", b"a", reserved_names)
        .map_err(|error| map_private_error(error, PublicationError::Staging))?;
    let second = match create_private(directory_fd, "probe", b"b", reserved_names) {
        Ok(second) => second,
        Err(error) => {
            return if unlink_if_identity(directory_fd, &first.0, first.1).is_ok() {
                Err(map_private_error(error, PublicationError::Staging))
            } else {
                Err(PublicationError::PrecommitCleanupUncertain)
            };
        }
    };

    let supported = (|| {
        if needs_noreplace {
            match renameat_with(
                directory_fd,
                &first.0,
                directory_fd,
                &second.0,
                RenameFlags::NOREPLACE,
            ) {
                Err(error) if error == Errno::EXIST => {}
                _ => return false,
            }
        }
        if needs_exchange {
            if renameat_with(
                directory_fd,
                &first.0,
                directory_fd,
                &second.0,
                RenameFlags::EXCHANGE,
            )
            .is_err()
            {
                return false;
            }
            if identities_at(directory_fd, &first.0, second.1, &second.0, first.1).is_err() {
                return false;
            }
            if renameat_with(
                directory_fd,
                &first.0,
                directory_fd,
                &second.0,
                RenameFlags::EXCHANGE,
            )
            .is_err()
            {
                return false;
            }
            if identities_at(directory_fd, &first.0, first.1, &second.0, second.1).is_err() {
                return false;
            }
        }
        true
    })();

    let first_cleanup = unlink_if_known_identity(directory_fd, &first.0, &[first.1, second.1]);
    let second_cleanup = unlink_if_known_identity(directory_fd, &second.0, &[first.1, second.1]);
    if first_cleanup.is_err() || second_cleanup.is_err() {
        return Err(PublicationError::PrecommitCleanupUncertain);
    }
    if !supported {
        return Err(PublicationError::AtomicPublicationUnsupported);
    }
    Ok(())
}

fn stage_artifact<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    artifact: &Artifact,
    original: Original,
    reserved_names: &[OsString],
) -> Result<StagedArtifact, PublicationError> {
    let (stage_name, staged_identity) =
        create_private(directory_fd, "stage", &artifact.bytes, reserved_names)
            .map_err(|error| map_private_error(error, PublicationError::Staging))?;
    Ok(StagedArtifact {
        final_name: artifact.name.clone(),
        stage_name,
        staged_identity,
        original,
    })
}

fn create_private<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    kind: &str,
    contents: &[u8],
    reserved_names: &[OsString],
) -> Result<(OsString, Identity), PrivateCreationError> {
    for _ in 0..128 {
        let serial = PRIVATE_NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name: OsString =
            format!(".seccompiler-bin.{kind}.{}.{}", std::process::id(), serial).into();
        if reserved_names.contains(&name) {
            continue;
        }
        let descriptor = match openat(
            directory_fd,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(descriptor) => descriptor,
            Err(error) if error == Errno::EXIST => continue,
            Err(_) => return Err(PrivateCreationError::Operation),
        };
        let mut file = File::from(descriptor);
        let write_failed = file.write_all(contents).is_err() || file.sync_all().is_err();
        let metadata = fstat(&file);
        drop(file);
        let metadata = metadata.map_err(|_| PrivateCreationError::CleanupUncertain)?;
        let identity = identity(&metadata);
        if write_failed {
            return if unlink_if_identity(directory_fd, &name, identity).is_ok() {
                Err(PrivateCreationError::Operation)
            } else {
                Err(PrivateCreationError::CleanupUncertain)
            };
        }
        return Ok((name, identity));
    }
    Err(PrivateCreationError::Operation)
}

fn map_private_error(
    error: PrivateCreationError,
    operation_error: PublicationError,
) -> PublicationError {
    match error {
        PrivateCreationError::Operation => operation_error,
        PrivateCreationError::CleanupUncertain => PublicationError::PrecommitCleanupUncertain,
    }
}

fn publish_one<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    entry: &StagedArtifact,
) -> Result<(), PublishOneError> {
    match entry.original {
        Original::Absent => {
            ensure_absent(directory_fd, &entry.final_name)
                .map_err(|_| PublishOneError::NoMutation)?;
            identity_at(directory_fd, &entry.stage_name, entry.staged_identity)
                .map_err(|_| PublishOneError::NoMutation)?;
            renameat_with(
                directory_fd,
                &entry.stage_name,
                directory_fd,
                &entry.final_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|_| PublishOneError::NoMutation)?;
            identity_at(directory_fd, &entry.final_name, entry.staged_identity)
                .map_err(|_| PublishOneError::MutationUncertain)
        }
        Original::Existing(original_identity) => {
            identities_at(
                directory_fd,
                &entry.stage_name,
                entry.staged_identity,
                &entry.final_name,
                original_identity,
            )
            .map_err(|_| PublishOneError::NoMutation)?;
            renameat_with(
                directory_fd,
                &entry.stage_name,
                directory_fd,
                &entry.final_name,
                RenameFlags::EXCHANGE,
            )
            .map_err(|_| PublishOneError::NoMutation)?;
            if identities_at(
                directory_fd,
                &entry.stage_name,
                original_identity,
                &entry.final_name,
                entry.staged_identity,
            )
            .is_ok()
            {
                return Ok(());
            }
            restore_captured_exchange(directory_fd, entry)
        }
    }
}

fn restore_captured_exchange<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    entry: &StagedArtifact,
) -> Result<(), PublishOneError> {
    identity_at(directory_fd, &entry.final_name, entry.staged_identity)
        .map_err(|_| PublishOneError::MutationUncertain)?;
    let captured_identity = match stat_identity(directory_fd, &entry.stage_name) {
        Ok(Some((identity, _))) => identity,
        _ => return Err(PublishOneError::MutationUncertain),
    };
    renameat_with(
        directory_fd,
        &entry.stage_name,
        directory_fd,
        &entry.final_name,
        RenameFlags::EXCHANGE,
    )
    .map_err(|_| PublishOneError::MutationUncertain)?;
    identities_at(
        directory_fd,
        &entry.stage_name,
        entry.staged_identity,
        &entry.final_name,
        captured_identity,
    )
    .map_err(|_| PublishOneError::MutationUncertain)?;
    Err(PublishOneError::NoMutation)
}

fn rollback_after_failure<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    entries: &[StagedArtifact],
    published: usize,
    fail_rollback: bool,
) -> Result<(), PublicationError> {
    if fail_rollback && published != 0 {
        return Err(PublicationError::RollbackUncertain);
    }
    for entry in entries.iter().take(published).rev() {
        if rollback_one(directory_fd, entry).is_err() {
            return Err(PublicationError::RollbackUncertain);
        }
    }
    if cleanup_stages(directory_fd, entries).is_err() {
        return Err(PublicationError::PrecommitCleanupUncertain);
    }
    if fsync(directory_fd).is_err() {
        return Err(PublicationError::PrecommitCleanupUncertain);
    }
    Err(PublicationError::PublicationRolledBack)
}

#[cfg(test)]
fn inject_replacement(directory: &Path, entry: &StagedArtifact) -> Result<(), PublicationError> {
    let replacement = directory.join(format!(
        ".seccompiler-bin.race.{}.{}",
        std::process::id(),
        PRIVATE_NAME_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&replacement, b"racing-replacement")
        .map_err(|_| PublicationError::OutputInspection)?;
    std::fs::rename(&replacement, directory.join(&entry.final_name))
        .map_err(|_| PublicationError::OutputInspection)
}

fn rollback_one<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    entry: &StagedArtifact,
) -> Result<(), ()> {
    identity_at(directory_fd, &entry.final_name, entry.staged_identity)?;
    match entry.original {
        Original::Absent => {
            ensure_absent(directory_fd, &entry.stage_name)?;
            renameat_with(
                directory_fd,
                &entry.final_name,
                directory_fd,
                &entry.stage_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|_| ())?;
            identity_at(directory_fd, &entry.stage_name, entry.staged_identity)?;
            ensure_absent(directory_fd, &entry.final_name)
        }
        Original::Existing(original_identity) => {
            identity_at(directory_fd, &entry.stage_name, original_identity)?;
            renameat_with(
                directory_fd,
                &entry.stage_name,
                directory_fd,
                &entry.final_name,
                RenameFlags::EXCHANGE,
            )
            .map_err(|_| ())?;
            identities_at(
                directory_fd,
                &entry.stage_name,
                entry.staged_identity,
                &entry.final_name,
                original_identity,
            )
        }
    }
}

fn cleanup_stages<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    entries: &[StagedArtifact],
) -> Result<(), ()> {
    let mut failed = false;
    for entry in entries {
        if unlink_if_identity(directory_fd, &entry.stage_name, entry.staged_identity).is_err() {
            failed = true;
        }
    }
    if failed { Err(()) } else { Ok(()) }
}

fn cleanup_committed_stages<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    entries: &[StagedArtifact],
) -> Result<(), ()> {
    let mut failed = false;
    for entry in entries {
        match entry.original {
            Original::Absent => {
                if ensure_absent(directory_fd, &entry.stage_name).is_err() {
                    failed = true;
                }
            }
            Original::Existing(identity) => {
                if unlink_if_identity(directory_fd, &entry.stage_name, identity).is_err() {
                    failed = true;
                }
            }
        }
    }
    if failed { Err(()) } else { Ok(()) }
}

fn unlink_if_identity<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    name: &OsStr,
    expected: Identity,
) -> Result<(), ()> {
    match stat_identity(directory_fd, name)? {
        None => Ok(()),
        Some((actual, _)) if actual == expected => {
            unlinkat(directory_fd, name, AtFlags::empty()).map_err(|_| ())
        }
        Some(_) => Err(()),
    }
}

fn unlink_if_known_identity<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    name: &OsStr,
    expected: &[Identity],
) -> Result<(), ()> {
    match stat_identity(directory_fd, name)? {
        None => Ok(()),
        Some((actual, _)) if expected.contains(&actual) => {
            unlinkat(directory_fd, name, AtFlags::empty()).map_err(|_| ())
        }
        Some(_) => Err(()),
    }
}

fn ensure_absent<Fd: std::os::fd::AsFd>(directory_fd: &Fd, name: &OsStr) -> Result<(), ()> {
    match stat_identity(directory_fd, name)? {
        None => Ok(()),
        Some(_) => Err(()),
    }
}

fn identity_at<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    name: &OsStr,
    expected: Identity,
) -> Result<(), ()> {
    match stat_identity(directory_fd, name)? {
        Some((actual, _)) if actual == expected => Ok(()),
        _ => Err(()),
    }
}

fn identities_at<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    first_name: &OsStr,
    first_identity: Identity,
    second_name: &OsStr,
    second_identity: Identity,
) -> Result<(), ()> {
    identity_at(directory_fd, first_name, first_identity)?;
    identity_at(directory_fd, second_name, second_identity)
}

fn stat_identity<Fd: std::os::fd::AsFd>(
    directory_fd: &Fd,
    name: &OsStr,
) -> Result<Option<(Identity, FileType)>, ()> {
    match statat(directory_fd, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => Ok(Some((
            identity(&metadata),
            FileType::from_raw_mode(metadata.st_mode),
        ))),
        Err(error) if error == Errno::NOENT => Ok(None),
        Err(_) => Err(()),
    }
}

fn identity(metadata: &rustix::fs::Stat) -> Identity {
    Identity {
        #[cfg(target_os = "linux")]
        device: metadata.st_dev,
        #[cfg(target_os = "macos")]
        device: metadata.st_dev as u64,
        inode: metadata.st_ino,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )]

    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[derive(Debug)]
    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should follow the epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "bangbang-seccompiler-artifact-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should be created");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn artifact(name: &str, bytes: &[u8]) -> Artifact {
        Artifact {
            name: name.into(),
            bytes: bytes.to_vec(),
        }
    }

    #[test]
    fn publishes_absent_and_existing_regular_files() {
        let directory = TestDirectory::new();
        fs::write(directory.0.join("existing"), b"old").expect("fixture should be written");
        let artifacts = [artifact("absent", b"new-a"), artifact("existing", b"new-b")];

        publish(&directory.0, &artifacts).expect("publication should succeed");

        assert_eq!(fs::read(directory.0.join("absent")).unwrap(), b"new-a");
        assert_eq!(fs::read(directory.0.join("existing")).unwrap(), b"new-b");
        assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
    }

    #[test]
    fn rolls_back_every_injected_split_publication_boundary() {
        for fail_after in 1..=3 {
            let directory = TestDirectory::new();
            fs::write(directory.0.join("api.bpf"), b"old-api").unwrap();
            let artifacts = [
                artifact("vmm.bpf", b"new-vmm"),
                artifact("api.bpf", b"new-api"),
                artifact("vcpu.bpf", b"new-vcpu"),
            ];
            let error = publish_with_faults(
                &directory.0,
                &artifacts,
                Faults {
                    fail_after_publications: Some(fail_after),
                    ..Faults::default()
                },
            )
            .expect_err("injected publication should fail");
            assert_eq!(error, PublicationError::PublicationRolledBack);
            assert!(!directory.0.join("vmm.bpf").exists());
            assert_eq!(fs::read(directory.0.join("api.bpf")).unwrap(), b"old-api");
            assert!(!directory.0.join("vcpu.bpf").exists());
            assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        }
    }

    #[test]
    fn reports_post_commit_uncertainty_without_claiming_rollback() {
        let directory = TestDirectory::new();
        let artifacts = [artifact("output", b"complete")];
        let error = publish_with_faults(
            &directory.0,
            &artifacts,
            Faults {
                fail_commit_sync: true,
                ..Faults::default()
            },
        )
        .expect_err("sync fault should be reported");
        assert_eq!(error, PublicationError::CommittedDurabilityUncertain);
        assert_eq!(fs::read(directory.0.join("output")).unwrap(), b"complete");
    }

    #[test]
    fn preserves_replacements_that_arrive_after_preflight() {
        for starts_existing in [false, true] {
            let directory = TestDirectory::new();
            if starts_existing {
                fs::write(directory.0.join("output"), b"preflight-original").unwrap();
            }
            let error = publish_with_faults(
                &directory.0,
                &[artifact("output", b"our-output")],
                Faults {
                    replace_before_publication: Some(0),
                    ..Faults::default()
                },
            )
            .expect_err("racing replacement should stop publication");
            assert_eq!(error, PublicationError::PublicationRolledBack);
            assert_eq!(
                fs::read(directory.0.join("output")).unwrap(),
                b"racing-replacement"
            );
            assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        }
    }

    #[test]
    fn restores_an_existing_replacement_captured_by_exchange() {
        let directory = TestDirectory::new();
        let final_name = OsString::from("output");
        fs::write(directory.0.join(&final_name), b"preflight-original").unwrap();
        let directory_fd = open(
            &directory.0,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .unwrap();
        let original = stat_identity(&directory_fd, &final_name)
            .unwrap()
            .unwrap()
            .0;
        let reserved = [final_name.clone()];
        let (stage_name, staged_identity) =
            create_private(&directory_fd, "stage", b"our-output", &reserved).unwrap();

        let replacement = directory.0.join("replacement-stage");
        fs::write(&replacement, b"racing-replacement").unwrap();
        fs::rename(&replacement, directory.0.join(&final_name)).unwrap();
        renameat_with(
            &directory_fd,
            &stage_name,
            &directory_fd,
            &final_name,
            RenameFlags::EXCHANGE,
        )
        .unwrap();

        let entry = StagedArtifact {
            final_name: final_name.clone(),
            stage_name: stage_name.clone(),
            staged_identity,
            original: Original::Existing(original),
        };
        assert_eq!(
            restore_captured_exchange(&directory_fd, &entry),
            Err(PublishOneError::NoMutation)
        );
        assert_eq!(
            fs::read(directory.0.join(&final_name)).unwrap(),
            b"racing-replacement"
        );
        assert_eq!(
            fs::read(directory.0.join(&stage_name)).unwrap(),
            b"our-output"
        );
        unlink_if_identity(&directory_fd, &stage_name, staged_identity).unwrap();
    }

    #[test]
    fn distinguishes_capability_rollback_and_cleanup_uncertainty() {
        let capability_directory = TestDirectory::new();
        let capability_error = publish_with_faults(
            &capability_directory.0,
            &[artifact("output", b"new")],
            Faults {
                fail_capability_probe: true,
                ..Faults::default()
            },
        )
        .expect_err("capability fault should fail before mutation");
        assert_eq!(
            capability_error,
            PublicationError::AtomicPublicationUnsupported
        );
        assert_eq!(fs::read_dir(&capability_directory.0).unwrap().count(), 0);

        let rollback_directory = TestDirectory::new();
        fs::write(rollback_directory.0.join("output"), b"old").unwrap();
        let rollback_error = publish_with_faults(
            &rollback_directory.0,
            &[artifact("output", b"new")],
            Faults {
                fail_after_publications: Some(1),
                fail_rollback: true,
                ..Faults::default()
            },
        )
        .expect_err("rollback fault should report uncertainty");
        assert_eq!(rollback_error, PublicationError::RollbackUncertain);
        assert_eq!(
            fs::read(rollback_directory.0.join("output")).unwrap(),
            b"new"
        );

        let cleanup_directory = TestDirectory::new();
        fs::write(cleanup_directory.0.join("output"), b"old").unwrap();
        let cleanup_error = publish_with_faults(
            &cleanup_directory.0,
            &[artifact("output", b"new")],
            Faults {
                fail_cleanup: true,
                ..Faults::default()
            },
        )
        .expect_err("cleanup fault should report a committed output");
        assert_eq!(cleanup_error, PublicationError::CommittedCleanupUncertain);
        assert_eq!(
            fs::read(cleanup_directory.0.join("output")).unwrap(),
            b"new"
        );
    }

    #[test]
    fn publication_errors_do_not_retain_caller_values() {
        let sensitive = "private-output-value";
        for error in [
            PublicationError::OutputDirectoryOpen,
            PublicationError::OutputInspection,
            PublicationError::UnsafeOutputType,
            PublicationError::AtomicPublicationUnsupported,
            PublicationError::Staging,
            PublicationError::PrecommitCleanupUncertain,
            PublicationError::PublicationRolledBack,
            PublicationError::RollbackUncertain,
            PublicationError::CommittedDurabilityUncertain,
            PublicationError::CommittedCleanupUncertain,
        ] {
            assert!(!error.to_string().contains(sensitive));
            assert!(!format!("{error:?}").contains(sensitive));
        }
    }

    #[test]
    fn rejects_non_regular_output_targets_without_mutation() {
        let directory = TestDirectory::new();
        fs::create_dir(directory.0.join("output")).unwrap();
        let error = publish(&directory.0, &[artifact("output", b"new")])
            .expect_err("directory target must be rejected");
        assert_eq!(error, PublicationError::UnsafeOutputType);
        assert!(directory.0.join("output").is_dir());
    }
}
