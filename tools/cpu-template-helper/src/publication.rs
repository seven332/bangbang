//! Failure-aware exclusive publication for one helper artifact.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use rustix::fs::{
    AtFlags, Mode, OFlags, RenameFlags, fstat, fsync, open, openat, renameat_with, statat, unlinkat,
};
use rustix::io::Errno;

use crate::CPU_TEMPLATE_DOCUMENT_MAX_BYTES;

static PRIVATE_NAME_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Failure while exclusively publishing one complete helper artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationError {
    InvalidOutputPath,
    ArtifactTooLarge,
    OutputDirectoryOpen,
    OutputInspection,
    Collision,
    Staging,
    AtomicCommitUnsupported,
    Commit,
    PrecommitCleanupUncertain,
    CommittedStateUncertain,
    CommittedDurabilityUncertain,
}

impl fmt::Display for PublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidOutputPath => "output path is invalid",
            Self::ArtifactTooLarge => "output artifact exceeds the size limit",
            Self::OutputDirectoryOpen => "output directory could not be opened safely",
            Self::OutputInspection => "output target could not be inspected safely",
            Self::Collision => "output target already exists",
            Self::Staging => "complete output artifact could not be staged",
            Self::AtomicCommitUnsupported => {
                "output filesystem lacks exclusive atomic rename support"
            }
            Self::Commit => "output artifact could not be committed",
            Self::PrecommitCleanupUncertain => {
                "output was not committed, but private staging cleanup is uncertain"
            }
            Self::CommittedStateUncertain => {
                "output commit occurred, but the published identity is uncertain"
            }
            Self::CommittedDurabilityUncertain => {
                "output was committed, but directory durability is uncertain"
            }
        })
    }
}

impl std::error::Error for PublicationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
}

#[derive(Debug, Default)]
struct Faults {
    stage_collisions: usize,
    fail_write_after: Option<usize>,
    fail_flush: bool,
    fail_file_sync: bool,
    replace_stage_before_commit: bool,
    concurrent_winner: bool,
    fail_commit: bool,
    force_atomic_unsupported: bool,
    replace_final_after_commit: bool,
    fail_directory_sync: bool,
    fail_cleanup: bool,
    fail_cleanup_sync: bool,
}

/// Publish one owner-only artifact only when the final path is absent.
pub fn publish_new_artifact(path: &Path, bytes: &[u8]) -> Result<(), PublicationError> {
    publish_with_faults(path, bytes, &Faults::default())
}

fn publish_with_faults(path: &Path, bytes: &[u8], faults: &Faults) -> Result<(), PublicationError> {
    if bytes.len() > CPU_TEMPLATE_DOCUMENT_MAX_BYTES {
        return Err(PublicationError::ArtifactTooLarge);
    }
    let final_name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or(PublicationError::InvalidOutputPath)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let directory = open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| PublicationError::OutputDirectoryOpen)?;

    match stat_identity(&directory, final_name) {
        Ok(None) => {}
        Ok(Some(_)) => return Err(PublicationError::Collision),
        Err(()) => return Err(PublicationError::OutputInspection),
    }

    let (stage_name, stage_identity) = create_stage(&directory, final_name, faults)?;
    if stage_bytes(&directory, &stage_name, stage_identity, bytes, faults).is_err() {
        return fail_before_commit(
            &directory,
            &stage_name,
            stage_identity,
            PublicationError::Staging,
            faults,
        );
    }

    if faults.replace_stage_before_commit && replace_path_for_test(&directory, &stage_name).is_err()
    {
        return fail_before_commit(
            &directory,
            &stage_name,
            stage_identity,
            PublicationError::OutputInspection,
            faults,
        );
    }
    if identity_at(&directory, &stage_name, stage_identity).is_err() {
        return Err(PublicationError::PrecommitCleanupUncertain);
    }

    if faults.concurrent_winner && create_concurrent_winner(&directory, final_name).is_err() {
        return fail_before_commit(
            &directory,
            &stage_name,
            stage_identity,
            PublicationError::OutputInspection,
            faults,
        );
    }
    if faults.fail_commit {
        return fail_before_commit(
            &directory,
            &stage_name,
            stage_identity,
            PublicationError::Commit,
            faults,
        );
    }
    if faults.force_atomic_unsupported {
        return fail_before_commit(
            &directory,
            &stage_name,
            stage_identity,
            PublicationError::AtomicCommitUnsupported,
            faults,
        );
    }

    if let Err(error) = renameat_with(
        &directory,
        &stage_name,
        &directory,
        final_name,
        RenameFlags::NOREPLACE,
    ) {
        let publication_error = if error == Errno::EXIST {
            PublicationError::Collision
        } else if matches!(error, Errno::NOSYS | Errno::NOTSUP | Errno::INVAL) {
            PublicationError::AtomicCommitUnsupported
        } else {
            PublicationError::Commit
        };
        return fail_before_commit(
            &directory,
            &stage_name,
            stage_identity,
            publication_error,
            faults,
        );
    }

    if faults.replace_final_after_commit && replace_path_for_test(&directory, final_name).is_err() {
        return Err(PublicationError::CommittedStateUncertain);
    }
    if identity_at(&directory, final_name, stage_identity).is_err() {
        return Err(PublicationError::CommittedStateUncertain);
    }
    if faults.fail_directory_sync || fsync(&directory).is_err() {
        return Err(PublicationError::CommittedDurabilityUncertain);
    }
    Ok(())
}

fn create_stage<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    final_name: &OsStr,
    faults: &Faults,
) -> Result<(OsString, Identity), PublicationError> {
    for attempt in 0..128 {
        let serial = PRIVATE_NAME_COUNTER.fetch_add(1, Ordering::Relaxed);
        let name: OsString = format!(
            ".bangbang-cpu-template-helper.stage.{}.{}",
            std::process::id(),
            serial
        )
        .into();
        if name == final_name {
            continue;
        }
        if attempt < faults.stage_collisions {
            create_stage_collision_for_test(directory, &name)?;
        }
        let descriptor = match openat(
            directory,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::RUSR | Mode::WUSR,
        ) {
            Ok(descriptor) => descriptor,
            Err(error) if error == Errno::EXIST => continue,
            Err(_) => return Err(PublicationError::Staging),
        };
        let metadata =
            fstat(&descriptor).map_err(|_| PublicationError::PrecommitCleanupUncertain)?;
        return Ok((name, identity(&metadata)));
    }
    Err(PublicationError::Staging)
}

fn create_stage_collision_for_test<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    stage_name: &OsStr,
) -> Result<(), PublicationError> {
    let descriptor = openat(
        directory,
        stage_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|_| PublicationError::Staging)?;
    let mut file = File::from(descriptor);
    file.write_all(b"occupied-stage")
        .map_err(|_| PublicationError::Staging)
}

fn stage_bytes<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    stage_name: &OsStr,
    stage_identity: Identity,
    bytes: &[u8],
    faults: &Faults,
) -> Result<(), ()> {
    let descriptor = openat(
        directory,
        stage_name,
        OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| ())?;
    if identity(&fstat(&descriptor).map_err(|_| ())?) != stage_identity {
        return Err(());
    }
    let mut file = File::from(descriptor);
    if let Some(limit) = faults.fail_write_after {
        let prefix = bytes.get(..limit.min(bytes.len())).ok_or(())?;
        file.write_all(prefix).map_err(|_| ())?;
        return Err(());
    }
    file.write_all(bytes).map_err(|_| ())?;
    if faults.fail_flush || file.flush().is_err() {
        return Err(());
    }
    if faults.fail_file_sync || file.sync_all().is_err() {
        return Err(());
    }
    Ok(())
}

fn fail_before_commit<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    stage_name: &OsStr,
    stage_identity: Identity,
    error: PublicationError,
    faults: &Faults,
) -> Result<(), PublicationError> {
    if cleanup_stage(directory, stage_name, stage_identity, faults).is_ok() {
        Err(error)
    } else {
        Err(PublicationError::PrecommitCleanupUncertain)
    }
}

fn cleanup_stage<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    stage_name: &OsStr,
    stage_identity: Identity,
    faults: &Faults,
) -> Result<(), ()> {
    if faults.fail_cleanup {
        return Err(());
    }
    match stat_identity(directory, stage_name)? {
        Some(identity) if identity == stage_identity => {
            unlinkat(directory, stage_name, AtFlags::empty()).map_err(|_| ())?;
        }
        None => {}
        Some(_) => return Err(()),
    }
    if faults.fail_cleanup_sync || fsync(directory).is_err() {
        return Err(());
    }
    Ok(())
}

fn create_concurrent_winner<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    final_name: &OsStr,
) -> Result<(), ()> {
    let descriptor = openat(
        directory,
        final_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|_| ())?;
    let mut file = File::from(descriptor);
    file.write_all(b"concurrent-winner").map_err(|_| ())?;
    file.sync_all().map_err(|_| ())
}

fn replace_path_for_test<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    stage_name: &OsStr,
) -> Result<(), ()> {
    unlinkat(directory, stage_name, AtFlags::empty()).map_err(|_| ())?;
    let descriptor = openat(
        directory,
        stage_name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::RUSR | Mode::WUSR,
    )
    .map_err(|_| ())?;
    let mut file = File::from(descriptor);
    file.write_all(b"identity-replacement").map_err(|_| ())?;
    file.sync_all().map_err(|_| ())
}

fn identity_at<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    name: &OsStr,
    expected: Identity,
) -> Result<(), ()> {
    match stat_identity(directory, name)? {
        Some(actual) if actual == expected => Ok(()),
        Some(_) | None => Err(()),
    }
}

fn stat_identity<Fd: std::os::fd::AsFd>(
    directory: &Fd,
    name: &OsStr,
) -> Result<Option<Identity>, ()> {
    match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => Ok(Some(identity(&metadata))),
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
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock should follow epoch")
                .as_nanos();
            let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-cpu-template-publication-{}-{nonce}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("test directory should be created");
            Self(path)
        }

        fn stage_count(&self) -> usize {
            fs::read_dir(&self.0)
                .expect("test directory should be readable")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".bangbang-cpu-template-helper.stage.")
                })
                .count()
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn publishes_complete_absent_artifact_and_rejects_existing_types() {
        let directory = TestDirectory::new();
        let output = directory.0.join("cpu.json");
        publish_new_artifact(&output, b"complete").expect("absent output should publish");
        assert_eq!(fs::read(&output).unwrap(), b"complete");
        assert_eq!(directory.stage_count(), 0);
        assert_eq!(
            publish_new_artifact(&output, b"replacement"),
            Err(PublicationError::Collision)
        );
        assert_eq!(fs::read(&output).unwrap(), b"complete");

        let target = directory.0.join("target");
        fs::write(&target, b"target-bytes").unwrap();
        let link = directory.0.join("link");
        symlink(&target, &link).unwrap();
        assert_eq!(
            publish_new_artifact(&link, b"replacement"),
            Err(PublicationError::Collision)
        );
        assert_eq!(fs::read(&target).unwrap(), b"target-bytes");

        let child_directory = directory.0.join("child");
        fs::create_dir(&child_directory).unwrap();
        assert_eq!(
            publish_new_artifact(&child_directory, b"replacement"),
            Err(PublicationError::Collision)
        );
    }

    #[test]
    fn concurrent_winner_is_preserved_and_stage_is_cleaned() {
        let directory = TestDirectory::new();
        let output = directory.0.join("cpu.json");
        let error = publish_with_faults(
            &output,
            b"ours",
            &Faults {
                concurrent_winner: true,
                ..Faults::default()
            },
        )
        .expect_err("concurrent winner should win");
        assert_eq!(error, PublicationError::Collision);
        assert_eq!(fs::read(&output).unwrap(), b"concurrent-winner");
        assert_eq!(directory.stage_count(), 0);
    }

    #[test]
    fn occupied_private_stage_names_are_skipped_without_removing_them() {
        let directory = TestDirectory::new();
        let output = directory.0.join("cpu.json");
        publish_with_faults(
            &output,
            b"complete",
            &Faults {
                stage_collisions: 2,
                ..Faults::default()
            },
        )
        .expect("fresh stage should be selected after occupied names");
        assert_eq!(fs::read(&output).unwrap(), b"complete");
        assert_eq!(directory.stage_count(), 2);
        for entry in fs::read_dir(&directory.0).unwrap().filter_map(Result::ok) {
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".bangbang-cpu-template-helper.stage.")
            {
                assert_eq!(fs::read(entry.path()).unwrap(), b"occupied-stage");
            }
        }
    }

    #[test]
    fn every_injected_precommit_io_failure_leaves_no_final() {
        for faults in [
            Faults {
                fail_write_after: Some(2),
                ..Faults::default()
            },
            Faults {
                fail_flush: true,
                ..Faults::default()
            },
            Faults {
                fail_file_sync: true,
                ..Faults::default()
            },
            Faults {
                fail_commit: true,
                ..Faults::default()
            },
            Faults {
                force_atomic_unsupported: true,
                ..Faults::default()
            },
        ] {
            let directory = TestDirectory::new();
            let output = directory.0.join("cpu.json");
            let error = publish_with_faults(&output, b"complete", &faults)
                .expect_err("injected operation should fail");
            assert!(matches!(
                error,
                PublicationError::Staging
                    | PublicationError::Commit
                    | PublicationError::AtomicCommitUnsupported
            ));
            assert!(!output.exists());
            assert_eq!(directory.stage_count(), 0);
        }
    }

    #[test]
    fn postcommit_directory_sync_reports_uncertain_with_complete_final() {
        let directory = TestDirectory::new();
        let output = directory.0.join("cpu.json");
        assert_eq!(
            publish_with_faults(
                &output,
                b"complete",
                &Faults {
                    fail_directory_sync: true,
                    ..Faults::default()
                }
            ),
            Err(PublicationError::CommittedDurabilityUncertain)
        );
        assert_eq!(fs::read(&output).unwrap(), b"complete");
        assert_eq!(directory.stage_count(), 0);

        let replaced_directory = TestDirectory::new();
        let replaced_output = replaced_directory.0.join("cpu.json");
        assert_eq!(
            publish_with_faults(
                &replaced_output,
                b"complete",
                &Faults {
                    replace_final_after_commit: true,
                    ..Faults::default()
                }
            ),
            Err(PublicationError::CommittedStateUncertain)
        );
        assert_eq!(fs::read(&replaced_output).unwrap(), b"identity-replacement");
        assert_eq!(replaced_directory.stage_count(), 0);
    }

    #[test]
    fn identity_change_and_cleanup_failures_are_explicitly_uncertain() {
        for faults in [
            Faults {
                replace_stage_before_commit: true,
                ..Faults::default()
            },
            Faults {
                fail_write_after: Some(1),
                fail_cleanup: true,
                ..Faults::default()
            },
            Faults {
                fail_write_after: Some(1),
                fail_cleanup_sync: true,
                ..Faults::default()
            },
        ] {
            let directory = TestDirectory::new();
            let output = directory.0.join("cpu.json");
            assert_eq!(
                publish_with_faults(&output, b"complete", &faults),
                Err(PublicationError::PrecommitCleanupUncertain)
            );
            assert!(!output.exists());
        }
    }

    #[test]
    fn rejects_oversized_artifact_before_path_access() {
        let private_path = Path::new("/definitely/private/unreachable/output");
        assert_eq!(
            publish_new_artifact(private_path, &vec![0; CPU_TEMPLATE_DOCUMENT_MAX_BYTES + 1]),
            Err(PublicationError::ArtifactTooLarge)
        );
    }
}
