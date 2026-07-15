use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::SeekFrom;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Component;

use super::*;

const MEMORY_STAGING_PREFIX: &[u8] = b".bangbang-snapshot-memory-";
const STATE_STAGING_PREFIX: &[u8] = b".bangbang-snapshot-state-";
const STAGING_RANDOM_BYTES: usize = 16;
const STAGING_CREATE_ATTEMPTS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

impl FileIdentity {
    fn cleanup_identity(self) -> SnapshotArtifactIdentity {
        SnapshotArtifactIdentity::new(
            u64::from(u32::from_ne_bytes(self.device.to_ne_bytes())),
            self.inode,
        )
    }
}

#[derive(Debug)]
struct SplitFinalPath {
    parent: PathBuf,
    component: CString,
}

#[derive(Debug)]
struct OpenedFinalPath {
    directory: File,
    directory_identity: FileIdentity,
    component: CString,
    tracker: Option<Arc<dyn SnapshotStagingTracker>>,
}

struct StagingFile<'directory> {
    destination: &'directory OpenedFinalPath,
    artifact: SnapshotArtifactKind,
    name: CString,
    file: File,
    identity: FileIdentity,
    active: bool,
    cleanup_on_drop: bool,
    ownership: Option<SnapshotStagingOwnership>,
}

impl fmt::Debug for StagingFile<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StagingFile")
            .field("artifact", &self.artifact)
            .field("name", &REDACTED)
            .field("active", &self.active)
            .finish()
    }
}

impl StagingFile<'_> {
    fn publish(
        &mut self,
        check_stage: SnapshotPublicationStage,
        publish_stage: SnapshotPublicationStage,
        visibility: SnapshotArtifactVisibility,
    ) -> Result<(), SnapshotPublicationError> {
        enter_publication_stage(check_stage).map_err(|kind| {
            publication_error(
                check_stage,
                visibility,
                SnapshotPublicationFailure::Io(kind),
            )
        })?;
        let current = entry_identity(&self.destination.directory, &self.name).map_err(|kind| {
            publication_error(
                check_stage,
                visibility,
                SnapshotPublicationFailure::Io(kind),
            )
        })?;
        if current != Some(self.identity) {
            return Err(publication_error(
                check_stage,
                visibility,
                SnapshotPublicationFailure::StagingChanged {
                    artifact: self.artifact,
                },
            ));
        }

        enter_publication_stage(publish_stage).map_err(|kind| {
            publication_error(
                publish_stage,
                visibility,
                SnapshotPublicationFailure::Io(kind),
            )
        })?;
        exclusive_rename(
            &self.destination.directory,
            &self.name,
            &self.destination.component,
        )
        .map_err(|kind| {
            let failure = if kind == io::ErrorKind::AlreadyExists {
                SnapshotPublicationFailure::FinalAlreadyExists {
                    artifact: self.artifact,
                }
            } else {
                SnapshotPublicationFailure::Io(kind)
            };
            publication_error(publish_stage, visibility, failure)
        })?;
        self.active = false;
        self.cleanup_on_drop = false;
        let _ = self.clear_ownership();
        Ok(())
    }

    fn record_ownership(&mut self) -> Result<(), SnapshotStagingTrackingError> {
        let Some(tracker) = &self.destination.tracker else {
            return Ok(());
        };
        let ownership = SnapshotStagingOwnership::new(
            self.artifact,
            self.destination.directory_identity.cleanup_identity(),
            self.name.as_bytes().to_vec(),
            self.identity.cleanup_identity(),
        );
        tracker.record(&ownership)?;
        self.ownership = Some(ownership);
        Ok(())
    }

    fn clear_ownership(&mut self) -> Result<(), SnapshotStagingTrackingError> {
        let Some(ownership) = self.ownership.as_ref() else {
            return Ok(());
        };
        let tracker = self
            .destination
            .tracker
            .as_ref()
            .ok_or(SnapshotStagingTrackingError)?;
        tracker.clear(ownership)?;
        self.ownership = None;
        Ok(())
    }

    fn finish_cleanup(&mut self, cleanup: SnapshotStagingCleanup) -> SnapshotStagingCleanup {
        if matches!(
            cleanup,
            SnapshotStagingCleanup::Removed
                | SnapshotStagingCleanup::AlreadyAbsent
                | SnapshotStagingCleanup::ChangedRefused
        ) {
            self.active = false;
            if self.clear_ownership().is_err() {
                return SnapshotStagingCleanup::Failed(io::ErrorKind::PermissionDenied);
            }
        }
        cleanup
    }

    fn cleanup(&mut self) -> Option<SnapshotStagingCleanup> {
        if !self.active {
            return None;
        }
        self.cleanup_on_drop = false;
        let stage = match self.artifact {
            SnapshotArtifactKind::State => SnapshotPublicationStage::StateStagingCleanup,
            SnapshotArtifactKind::Memory => SnapshotPublicationStage::MemoryStagingCleanup,
        };
        if let Err(kind) = enter_publication_stage(stage) {
            return Some(SnapshotStagingCleanup::Failed(kind));
        }
        let cleanup = clean_staging_entry(&self.destination.directory, &self.name, self.identity);
        Some(self.finish_cleanup(cleanup))
    }
}

impl Drop for StagingFile<'_> {
    fn drop(&mut self) {
        if self.active && self.cleanup_on_drop {
            let cleanup =
                clean_staging_entry(&self.destination.directory, &self.name, self.identity);
            let _ = self.finish_cleanup(cleanup);
        }
    }
}

pub(super) fn publish_snapshot_artifacts_macos_with<E, F>(
    outputs: &SnapshotArtifactOutputs,
    producer: F,
) -> Result<SnapshotPublicationOutcome, SnapshotPublicationTransactionError<E>>
where
    F: FnOnce(SnapshotMemoryStagingWriter) -> Result<SnapshotCommitRecord, E>,
{
    let state = open_artifact_output(
        outputs.state(),
        SnapshotArtifactKind::State,
        SnapshotPublicationStage::StatePathValidation,
        SnapshotPublicationStage::StateDirectoryOpen,
    )?;
    let memory_path = open_artifact_output(
        outputs.memory(),
        SnapshotArtifactKind::Memory,
        SnapshotPublicationStage::MemoryPathValidation,
        SnapshotPublicationStage::MemoryDirectoryOpen,
    )?;

    enter_publication_stage(SnapshotPublicationStage::AliasCheck).map_err(|kind| {
        publication_error(
            SnapshotPublicationStage::AliasCheck,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(kind),
        )
    })?;
    if state.directory_identity == memory_path.directory_identity
        && state.component.as_bytes() == memory_path.component.as_bytes()
    {
        return Err(publication_error(
            SnapshotPublicationStage::AliasCheck,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::SameArtifact,
        )
        .into());
    }

    preflight_absent(
        &state,
        SnapshotArtifactKind::State,
        SnapshotPublicationStage::StateFinalPreflight,
    )?;
    preflight_absent(
        &memory_path,
        SnapshotArtifactKind::Memory,
        SnapshotPublicationStage::MemoryFinalPreflight,
    )?;

    let mut memory_staging = create_staging(
        &memory_path,
        SnapshotArtifactKind::Memory,
        SnapshotPublicationStage::MemoryStagingCreate,
    )?;
    let mut state_staging = match create_staging(
        &state,
        SnapshotArtifactKind::State,
        SnapshotPublicationStage::StateStagingCreate,
    ) {
        Ok(staging) => staging,
        Err(mut error) => {
            error.memory_cleanup = memory_staging.cleanup();
            return Err(error.into());
        }
    };

    match publish_prepared_with(
        &memory_path,
        &state,
        &mut memory_staging,
        &mut state_staging,
        producer,
    ) {
        Ok(outcome) => Ok(outcome),
        Err(SnapshotPublicationTransactionError::Publication(mut error)) => {
            error.memory_cleanup = memory_staging.cleanup();
            error.state_cleanup = state_staging.cleanup();
            Err(SnapshotPublicationTransactionError::Publication(error))
        }
        Err(SnapshotPublicationTransactionError::Producer(mut error)) => {
            error.memory_cleanup = memory_staging.cleanup();
            error.state_cleanup = state_staging.cleanup();
            Err(SnapshotPublicationTransactionError::Producer(error))
        }
    }
}

fn open_artifact_output(
    output: &SnapshotArtifactOutput,
    artifact: SnapshotArtifactKind,
    validation_stage: SnapshotPublicationStage,
    directory_stage: SnapshotPublicationStage,
) -> Result<OpenedFinalPath, SnapshotPublicationError> {
    stage_io(
        validation_stage,
        SnapshotArtifactVisibility::NoFinalArtifact,
    )?;
    match &output.location {
        SnapshotArtifactOutputLocation::Path(path) => {
            let split = split_final_path(path, artifact).map_err(|failure| {
                publication_error(
                    validation_stage,
                    SnapshotArtifactVisibility::NoFinalArtifact,
                    failure,
                )
            })?;
            open_final_path(split, directory_stage)
        }
        SnapshotArtifactOutputLocation::Anchored {
            directory,
            child,
            tracker,
        } => {
            let component = validate_supplied_component(child, artifact).map_err(|failure| {
                publication_error(
                    validation_stage,
                    SnapshotArtifactVisibility::NoFinalArtifact,
                    failure,
                )
            })?;
            open_anchored_final(directory, component, tracker.clone(), directory_stage)
        }
    }
}

fn validate_supplied_component(
    component: &[u8],
    artifact: SnapshotArtifactKind,
) -> Result<CString, SnapshotPublicationFailure> {
    if component.is_empty() || component == b"." || component == b".." || component.contains(&b'/')
    {
        return Err(SnapshotPublicationFailure::InvalidFinalPath { artifact });
    }
    CString::new(component).map_err(|_| SnapshotPublicationFailure::InvalidFinalPath { artifact })
}

fn open_anchored_final(
    supplied: &File,
    component: CString,
    tracker: Option<Arc<dyn SnapshotStagingTracker>>,
    stage: SnapshotPublicationStage,
) -> Result<OpenedFinalPath, SnapshotPublicationError> {
    stage_io(stage, SnapshotArtifactVisibility::NoFinalArtifact)?;
    let directory = supplied.try_clone().map_err(|source| {
        publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(source.kind()),
        )
    })?;
    let metadata = directory.metadata().map_err(|source| {
        publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(source.kind()),
        )
    })?;
    if !metadata.file_type().is_dir() {
        return Err(publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(io::ErrorKind::InvalidInput),
        ));
    }
    // SAFETY: the directory remains live and the fixed dot component is NUL-terminated.
    if unsafe {
        libc::faccessat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::W_OK | libc::X_OK,
            libc::AT_EACCESS,
        )
    } != 0
    {
        return Err(publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(io::Error::last_os_error().kind()),
        ));
    }
    let directory_identity = file_identity(&directory).map_err(|kind| {
        publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(kind),
        )
    })?;
    Ok(OpenedFinalPath {
        directory,
        directory_identity,
        component,
        tracker,
    })
}

fn publish_prepared_with<E, F>(
    memory_path: &OpenedFinalPath,
    state_path: &OpenedFinalPath,
    memory_staging: &mut StagingFile<'_>,
    state_staging: &mut StagingFile<'_>,
    producer: F,
) -> Result<SnapshotPublicationOutcome, SnapshotPublicationTransactionError<E>>
where
    F: FnOnce(SnapshotMemoryStagingWriter) -> Result<SnapshotCommitRecord, E>,
{
    stage_io(
        SnapshotPublicationStage::MemoryWrite,
        SnapshotArtifactVisibility::NoFinalArtifact,
    )?;
    let writer_file = memory_staging.file.try_clone().map_err(|source| {
        publication_error(
            SnapshotPublicationStage::MemoryWrite,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(source.kind()),
        )
    })?;
    let writer_closed = Arc::new(AtomicBool::new(false));
    let writer = SnapshotMemoryStagingWriter::new(writer_file, Arc::clone(&writer_closed));
    let record = producer(writer).map_err(|source| {
        SnapshotPublicationTransactionError::Producer(SnapshotPublicationProducerError::new(source))
    })?;

    stage_io(
        SnapshotPublicationStage::MemoryWriterClose,
        SnapshotArtifactVisibility::NoFinalArtifact,
    )?;
    if !writer_closed.load(Ordering::Acquire) {
        return Err(publication_error(
            SnapshotPublicationStage::MemoryWriterClose,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::StagingWriterRetained,
        )
        .into());
    }

    stage_io(
        SnapshotPublicationStage::MemoryWriteVerify,
        SnapshotArtifactVisibility::NoFinalArtifact,
    )?;
    verify_snapshot_memory_image_output(record.memory_binding(), &mut memory_staging.file)
        .map_err(|source| {
            publication_error(
                SnapshotPublicationStage::MemoryWriteVerify,
                SnapshotArtifactVisibility::NoFinalArtifact,
                SnapshotPublicationFailure::MemoryVerify(source),
            )
        })?;

    stage_io(
        SnapshotPublicationStage::StateEncode,
        SnapshotArtifactVisibility::NoFinalArtifact,
    )?;
    let state_bytes = encode_snapshot_commit_envelope(&record).map_err(|source| {
        publication_error(
            SnapshotPublicationStage::StateEncode,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Commit(source),
        )
    })?;

    stage_io(
        SnapshotPublicationStage::StateWrite,
        SnapshotArtifactVisibility::NoFinalArtifact,
    )?;
    state_staging
        .file
        .write_all(&state_bytes)
        .map_err(|source| {
            publication_error(
                SnapshotPublicationStage::StateWrite,
                SnapshotArtifactVisibility::NoFinalArtifact,
                SnapshotPublicationFailure::Io(source.kind()),
            )
        })?;
    verify_state_staging(&mut state_staging.file, state_bytes.len())?;

    sync_file(
        &memory_staging.file,
        SnapshotPublicationStage::MemoryFileSync,
        SnapshotArtifactVisibility::NoFinalArtifact,
    )?;
    sync_file(
        &state_staging.file,
        SnapshotPublicationStage::StateFileSync,
        SnapshotArtifactVisibility::NoFinalArtifact,
    )?;

    memory_staging.publish(
        SnapshotPublicationStage::MemoryPublishCheck,
        SnapshotPublicationStage::MemoryPublish,
        SnapshotArtifactVisibility::NoFinalArtifact,
    )?;
    sync_directory(
        &memory_path.directory,
        SnapshotPublicationStage::MemoryDirectorySync,
        SnapshotArtifactVisibility::MemoryOrphanVisible,
    )?;

    state_staging.publish(
        SnapshotPublicationStage::StatePublishCheck,
        SnapshotPublicationStage::StatePublish,
        SnapshotArtifactVisibility::MemoryOrphanVisible,
    )?;

    let durability = match enter_publication_stage(SnapshotPublicationStage::StateDirectorySync) {
        Ok(()) => match state_path.directory.sync_all() {
            Ok(()) => SnapshotCommitDurability::Durable,
            Err(source) => SnapshotCommitDurability::Uncertain {
                kind: source.kind(),
            },
        },
        Err(kind) => SnapshotCommitDurability::Uncertain { kind },
    };
    Ok(SnapshotPublicationOutcome { record, durability })
}

fn split_final_path(
    path: &Path,
    artifact: SnapshotArtifactKind,
) -> Result<SplitFinalPath, SnapshotPublicationFailure> {
    let raw_path = path.as_os_str().as_bytes();
    if raw_path.contains(&0) {
        return Err(SnapshotPublicationFailure::InvalidFinalPath { artifact });
    }
    let Some(raw_component) = raw_path.rsplit(|byte| *byte == b'/').next() else {
        return Err(SnapshotPublicationFailure::InvalidFinalPath { artifact });
    };
    if raw_component.is_empty() || raw_component == b"." || raw_component == b".." {
        return Err(SnapshotPublicationFailure::InvalidFinalPath { artifact });
    }
    let Some(component) = path.components().next_back() else {
        return Err(SnapshotPublicationFailure::InvalidFinalPath { artifact });
    };
    let Component::Normal(component) = component else {
        return Err(SnapshotPublicationFailure::InvalidFinalPath { artifact });
    };
    if component.as_bytes() != raw_component {
        return Err(SnapshotPublicationFailure::InvalidFinalPath { artifact });
    }
    let component = CString::new(raw_component)
        .map_err(|_| SnapshotPublicationFailure::InvalidFinalPath { artifact })?;
    if component.as_bytes().is_empty() {
        return Err(SnapshotPublicationFailure::InvalidFinalPath { artifact });
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        parent.to_path_buf()
    };
    Ok(SplitFinalPath { parent, component })
}

fn open_final_path(
    split: SplitFinalPath,
    stage: SnapshotPublicationStage,
) -> Result<OpenedFinalPath, SnapshotPublicationError> {
    stage_io(stage, SnapshotArtifactVisibility::NoFinalArtifact)?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(split.parent)
        .map_err(|source| {
            publication_error(
                stage,
                SnapshotArtifactVisibility::NoFinalArtifact,
                SnapshotPublicationFailure::Io(source.kind()),
            )
        })?;
    let directory_identity = file_identity(&directory).map_err(|kind| {
        publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(kind),
        )
    })?;
    Ok(OpenedFinalPath {
        directory,
        directory_identity,
        component: split.component,
        tracker: None,
    })
}

fn preflight_absent(
    path: &OpenedFinalPath,
    artifact: SnapshotArtifactKind,
    stage: SnapshotPublicationStage,
) -> Result<(), SnapshotPublicationError> {
    stage_io(stage, SnapshotArtifactVisibility::NoFinalArtifact)?;
    match entry_identity(&path.directory, &path.component) {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::FinalAlreadyExists { artifact },
        )),
        Err(kind) => Err(publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(kind),
        )),
    }
}

fn create_staging<'directory>(
    destination: &'directory OpenedFinalPath,
    artifact: SnapshotArtifactKind,
    stage: SnapshotPublicationStage,
) -> Result<StagingFile<'directory>, SnapshotPublicationError> {
    stage_io(stage, SnapshotArtifactVisibility::NoFinalArtifact)?;
    for _ in 0..STAGING_CREATE_ATTEMPTS {
        let name = staging_name(artifact).map_err(|failure| {
            publication_error(stage, SnapshotArtifactVisibility::NoFinalArtifact, failure)
        })?;
        match open_staging(&destination.directory, &name) {
            Ok(file) => {
                let identity = match file_identity(&file) {
                    Ok(identity) => identity,
                    Err(kind) => {
                        let mut error = publication_error(
                            stage,
                            SnapshotArtifactVisibility::NoFinalArtifact,
                            SnapshotPublicationFailure::Io(kind),
                        );
                        set_staging_cleanup(
                            &mut error,
                            artifact,
                            SnapshotStagingCleanup::Failed(kind),
                        );
                        return Err(error);
                    }
                };
                let mut staging = StagingFile {
                    destination,
                    artifact,
                    name,
                    file,
                    identity,
                    active: true,
                    cleanup_on_drop: true,
                    ownership: None,
                };
                if let Err(source) = staging
                    .file
                    .set_permissions(std::fs::Permissions::from_mode(0o600))
                {
                    let kind = source.kind();
                    let cleanup = staging.cleanup();
                    let mut error = publication_error(
                        stage,
                        SnapshotArtifactVisibility::NoFinalArtifact,
                        SnapshotPublicationFailure::Io(kind),
                    );
                    if let Some(cleanup) = cleanup {
                        set_staging_cleanup(&mut error, artifact, cleanup);
                    }
                    return Err(error);
                }
                if staging.record_ownership().is_err() {
                    let cleanup = staging.cleanup();
                    let mut error = publication_error(
                        stage,
                        SnapshotArtifactVisibility::NoFinalArtifact,
                        SnapshotPublicationFailure::Io(io::ErrorKind::PermissionDenied),
                    );
                    if let Some(cleanup) = cleanup {
                        set_staging_cleanup(&mut error, artifact, cleanup);
                    }
                    return Err(error);
                }
                return Ok(staging);
            }
            Err(io::ErrorKind::AlreadyExists) => {}
            Err(kind) => {
                return Err(publication_error(
                    stage,
                    SnapshotArtifactVisibility::NoFinalArtifact,
                    SnapshotPublicationFailure::Io(kind),
                ));
            }
        }
    }
    Err(publication_error(
        stage,
        SnapshotArtifactVisibility::NoFinalArtifact,
        SnapshotPublicationFailure::Io(io::ErrorKind::AlreadyExists),
    ))
}

fn set_staging_cleanup(
    error: &mut SnapshotPublicationError,
    artifact: SnapshotArtifactKind,
    cleanup: SnapshotStagingCleanup,
) {
    match artifact {
        SnapshotArtifactKind::State => error.state_cleanup = Some(cleanup),
        SnapshotArtifactKind::Memory => error.memory_cleanup = Some(cleanup),
    }
}

fn staging_name(artifact: SnapshotArtifactKind) -> Result<CString, SnapshotPublicationFailure> {
    let mut random = [0_u8; STAGING_RANDOM_BYTES];
    fill_staging_random(&mut random, artifact)?;
    let prefix = match artifact {
        SnapshotArtifactKind::State => STATE_STAGING_PREFIX,
        SnapshotArtifactKind::Memory => MEMORY_STAGING_PREFIX,
    };
    let mut bytes = Vec::with_capacity(prefix.len() + STAGING_RANDOM_BYTES * 2);
    bytes.extend_from_slice(prefix);
    for byte in random {
        bytes.push(hex_digit(byte >> 4));
        bytes.push(hex_digit(byte & 0x0f));
    }
    CString::new(bytes).map_err(|_| SnapshotPublicationFailure::RandomnessUnavailable { artifact })
}

fn fill_staging_random(
    destination: &mut [u8; STAGING_RANDOM_BYTES],
    artifact: SnapshotArtifactKind,
) -> Result<(), SnapshotPublicationFailure> {
    #[cfg(test)]
    if PUBLICATION_TEST_HOOK.with(|hook| hook.borrow().random_failure) {
        return Err(SnapshotPublicationFailure::RandomnessUnavailable { artifact });
    }
    #[cfg(test)]
    if let Some(random) =
        PUBLICATION_TEST_HOOK.with(|hook| hook.borrow_mut().random_names.pop_front())
    {
        *destination = random;
        return Ok(());
    }
    getrandom::fill(destination)
        .map_err(|_| SnapshotPublicationFailure::RandomnessUnavailable { artifact })
}

const fn hex_digit(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'a' + (nibble - 10),
    }
}

fn open_staging(directory: &File, name: &CString) -> Result<File, io::ErrorKind> {
    // SAFETY: `directory` is a live directory descriptor, `name` is a
    // NUL-terminated single generated component, and the returned owned
    // descriptor is checked before conversion to `File`.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().kind());
    }
    // SAFETY: `openat` returned a fresh owned descriptor on success.
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn verify_state_staging(
    file: &mut File,
    expected_length: usize,
) -> Result<(), SnapshotPublicationError> {
    let stage = SnapshotPublicationStage::StateWriteVerify;
    stage_io(stage, SnapshotArtifactVisibility::NoFinalArtifact)?;
    let expected = u64::try_from(expected_length).map_err(|_| {
        publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(io::ErrorKind::InvalidData),
        )
    })?;
    let position = file.stream_position().map_err(|source| {
        publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(source.kind()),
        )
    })?;
    let length = file.metadata().map_err(|source| {
        publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(source.kind()),
        )
    })?;
    if position != expected || length.len() != expected {
        return Err(publication_error(
            stage,
            SnapshotArtifactVisibility::NoFinalArtifact,
            SnapshotPublicationFailure::Io(io::ErrorKind::InvalidData),
        ));
    }
    Ok(())
}

fn sync_file(
    file: &File,
    stage: SnapshotPublicationStage,
    visibility: SnapshotArtifactVisibility,
) -> Result<(), SnapshotPublicationError> {
    stage_io(stage, visibility)?;
    file.sync_all().map_err(|source| {
        publication_error(
            stage,
            visibility,
            SnapshotPublicationFailure::Io(source.kind()),
        )
    })
}

fn sync_directory(
    directory: &File,
    stage: SnapshotPublicationStage,
    visibility: SnapshotArtifactVisibility,
) -> Result<(), SnapshotPublicationError> {
    stage_io(stage, visibility)?;
    directory.sync_all().map_err(|source| {
        publication_error(
            stage,
            visibility,
            SnapshotPublicationFailure::Io(source.kind()),
        )
    })
}

fn stage_io(
    stage: SnapshotPublicationStage,
    visibility: SnapshotArtifactVisibility,
) -> Result<(), SnapshotPublicationError> {
    enter_publication_stage(stage)
        .map_err(|kind| publication_error(stage, visibility, SnapshotPublicationFailure::Io(kind)))
}

fn file_identity(file: &File) -> Result<FileIdentity, io::ErrorKind> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` points to writable storage and `file` owns a live
    // descriptor for the duration of the call.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error().kind());
    }
    // SAFETY: successful `fstat` initialized the complete structure.
    let stat = unsafe { stat.assume_init() };
    Ok(FileIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    })
}

fn entry_stat(directory: &File, name: &CString) -> Result<Option<libc::stat>, io::ErrorKind> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `directory` is live, `name` is NUL-terminated, and `stat`
    // points to writable storage. `AT_SYMLINK_NOFOLLOW` inspects the final
    // entry itself rather than following it.
    let result = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        // SAFETY: successful `fstatat` initialized the structure.
        return Ok(Some(unsafe { stat.assume_init() }));
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(error.kind())
    }
}

fn entry_identity(directory: &File, name: &CString) -> Result<Option<FileIdentity>, io::ErrorKind> {
    entry_stat(directory, name).map(|stat| {
        stat.map(|stat| FileIdentity {
            device: stat.st_dev,
            inode: stat.st_ino,
        })
    })
}

fn exclusive_rename(
    directory: &File,
    source: &CString,
    destination: &CString,
) -> Result<(), io::ErrorKind> {
    // SAFETY: both names are NUL-terminated single components and both
    // descriptors refer to the retained destination directory. The
    // exclusive flag prevents replacement of an existing target entry.
    let result = unsafe {
        libc::renameatx_np(
            directory.as_raw_fd(),
            source.as_ptr(),
            directory.as_raw_fd(),
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().kind())
    }
}

fn clean_staging_entry(
    directory: &File,
    name: &CString,
    expected: FileIdentity,
) -> SnapshotStagingCleanup {
    match entry_identity(directory, name) {
        Ok(None) => SnapshotStagingCleanup::AlreadyAbsent,
        Ok(Some(actual)) if actual != expected => SnapshotStagingCleanup::ChangedRefused,
        Ok(Some(_)) => {
            // SAFETY: `directory` is live and `name` is a NUL-terminated
            // generated component. The immediately preceding identity
            // check is best-effort; Darwin has no conditional unlink by
            // expected inode, so trusted directory authority is required.
            let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
            if result == 0 {
                SnapshotStagingCleanup::Removed
            } else {
                let kind = io::Error::last_os_error().kind();
                if kind == io::ErrorKind::NotFound {
                    SnapshotStagingCleanup::AlreadyAbsent
                } else {
                    SnapshotStagingCleanup::Failed(kind)
                }
            }
        }
        Err(kind) => SnapshotStagingCleanup::Failed(kind),
    }
}

pub(super) fn load_snapshot_artifacts_macos(
    paths: &SnapshotArtifactPaths,
) -> Result<LoadedSnapshotArtifacts, SnapshotArtifactLoadError> {
    let state_split =
        split_final_path(paths.state(), SnapshotArtifactKind::State).map_err(|_| {
            load_error(
                SnapshotArtifactLoadStage::StatePathValidation,
                SnapshotArtifactLoadFailure::InvalidFinalPath {
                    artifact: SnapshotArtifactKind::State,
                },
            )
        })?;
    let state_directory = open_load_directory(
        &state_split.parent,
        SnapshotArtifactLoadStage::StateDirectoryOpen,
    )?;
    let (state_file, _) = open_regular_final(
        &state_directory,
        &state_split.component,
        SnapshotArtifactKind::State,
        SnapshotArtifactLoadStage::StateOpen,
        SnapshotArtifactLoadStage::StateTypeCheck,
    )?;
    let prepared = prepare_snapshot_state_file_macos(state_file)?;

    let memory_split =
        split_final_path(paths.memory(), SnapshotArtifactKind::Memory).map_err(|_| {
            load_error(
                SnapshotArtifactLoadStage::MemoryPathValidation,
                SnapshotArtifactLoadFailure::InvalidFinalPath {
                    artifact: SnapshotArtifactKind::Memory,
                },
            )
        })?;
    let memory_directory = open_load_directory(
        &memory_split.parent,
        SnapshotArtifactLoadStage::MemoryDirectoryOpen,
    )?;
    let (memory_file, _) = open_regular_final(
        &memory_directory,
        &memory_split.component,
        SnapshotArtifactKind::Memory,
        SnapshotArtifactLoadStage::MemoryOpen,
        SnapshotArtifactLoadStage::MemoryTypeCheck,
    )?;
    load_prepared_snapshot_memory_file_macos(prepared, memory_file)
}

pub(super) fn prepare_snapshot_state_file_macos(
    mut state_file: File,
) -> Result<PreparedSnapshotState, SnapshotArtifactLoadError> {
    let state_length = supplied_regular_length(
        &state_file,
        SnapshotArtifactKind::State,
        SnapshotArtifactLoadStage::StateTypeCheck,
    )?;
    let maximum = u64::try_from(NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES).map_err(|_| {
        load_error(
            SnapshotArtifactLoadStage::StateSizeCheck,
            SnapshotArtifactLoadFailure::LengthOverflow,
        )
    })?;
    if state_length > maximum {
        return Err(load_error(
            SnapshotArtifactLoadStage::StateSizeCheck,
            SnapshotArtifactLoadFailure::StateTooLarge {
                length: state_length,
                maximum: NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES,
            },
        ));
    }
    let reserve = usize::try_from(state_length).map_err(|_| {
        load_error(
            SnapshotArtifactLoadStage::StateSizeCheck,
            SnapshotArtifactLoadFailure::LengthOverflow,
        )
    })?;
    let mut state_bytes = Vec::new();
    state_bytes.try_reserve_exact(reserve).map_err(|source| {
        load_error(
            SnapshotArtifactLoadStage::StateRead,
            SnapshotArtifactLoadFailure::AllocationFailed { source },
        )
    })?;
    let read_limit = maximum.checked_add(1).ok_or_else(|| {
        load_error(
            SnapshotArtifactLoadStage::StateSizeCheck,
            SnapshotArtifactLoadFailure::LengthOverflow,
        )
    })?;
    state_file.seek(SeekFrom::Start(0)).map_err(|source| {
        load_error(
            SnapshotArtifactLoadStage::StateRead,
            SnapshotArtifactLoadFailure::Io(source.kind()),
        )
    })?;
    Read::by_ref(&mut state_file)
        .take(read_limit)
        .read_to_end(&mut state_bytes)
        .map_err(|source| {
            load_error(
                SnapshotArtifactLoadStage::StateRead,
                SnapshotArtifactLoadFailure::Io(source.kind()),
            )
        })?;
    if state_bytes.len() > NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES {
        return Err(load_error(
            SnapshotArtifactLoadStage::StateSizeCheck,
            SnapshotArtifactLoadFailure::StateTooLarge {
                length: u64::try_from(state_bytes.len()).unwrap_or(u64::MAX),
                maximum: NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES,
            },
        ));
    }
    let record = decode_snapshot_commit_envelope(&state_bytes).map_err(|source| {
        load_error(
            SnapshotArtifactLoadStage::StateDecode,
            SnapshotArtifactLoadFailure::Commit(source),
        )
    })?;
    Ok(PreparedSnapshotState { record })
}

pub(super) fn prepare_snapshot_state_path_macos(
    path: &Path,
) -> Result<PreparedSnapshotState, SnapshotArtifactLoadError> {
    let split = split_final_path(path, SnapshotArtifactKind::State).map_err(|_| {
        load_error(
            SnapshotArtifactLoadStage::StatePathValidation,
            SnapshotArtifactLoadFailure::InvalidFinalPath {
                artifact: SnapshotArtifactKind::State,
            },
        )
    })?;
    let directory =
        open_load_directory(&split.parent, SnapshotArtifactLoadStage::StateDirectoryOpen)?;
    let (file, _) = open_regular_final(
        &directory,
        &split.component,
        SnapshotArtifactKind::State,
        SnapshotArtifactLoadStage::StateOpen,
        SnapshotArtifactLoadStage::StateTypeCheck,
    )?;
    prepare_snapshot_state_file_macos(file)
}

pub(super) fn load_prepared_snapshot_memory_file_macos(
    prepared: PreparedSnapshotState,
    mut memory_file: File,
) -> Result<LoadedSnapshotArtifacts, SnapshotArtifactLoadError> {
    supplied_regular_length(
        &memory_file,
        SnapshotArtifactKind::Memory,
        SnapshotArtifactLoadStage::MemoryTypeCheck,
    )?;
    memory_file.seek(SeekFrom::Start(0)).map_err(|source| {
        load_error(
            SnapshotArtifactLoadStage::MemoryLoad,
            SnapshotArtifactLoadFailure::Io(source.kind()),
        )
    })?;
    let record = prepared.record;
    let memory = load_snapshot_memory_image(record.memory_binding(), &mut memory_file).map_err(
        |source| {
            load_error(
                SnapshotArtifactLoadStage::MemoryLoad,
                SnapshotArtifactLoadFailure::Memory(source),
            )
        },
    )?;
    Ok(LoadedSnapshotArtifacts { record, memory })
}

pub(super) fn load_prepared_snapshot_memory_path_macos(
    prepared: PreparedSnapshotState,
    path: &Path,
) -> Result<LoadedSnapshotArtifacts, SnapshotArtifactLoadError> {
    let split = split_final_path(path, SnapshotArtifactKind::Memory).map_err(|_| {
        load_error(
            SnapshotArtifactLoadStage::MemoryPathValidation,
            SnapshotArtifactLoadFailure::InvalidFinalPath {
                artifact: SnapshotArtifactKind::Memory,
            },
        )
    })?;
    let directory = open_load_directory(
        &split.parent,
        SnapshotArtifactLoadStage::MemoryDirectoryOpen,
    )?;
    let (file, _) = open_regular_final(
        &directory,
        &split.component,
        SnapshotArtifactKind::Memory,
        SnapshotArtifactLoadStage::MemoryOpen,
        SnapshotArtifactLoadStage::MemoryTypeCheck,
    )?;
    load_prepared_snapshot_memory_file_macos(prepared, file)
}

fn supplied_regular_length(
    file: &File,
    artifact: SnapshotArtifactKind,
    stage: SnapshotArtifactLoadStage,
) -> Result<u64, SnapshotArtifactLoadError> {
    let metadata = file
        .metadata()
        .map_err(|source| load_error(stage, SnapshotArtifactLoadFailure::Io(source.kind())))?;
    if !metadata.file_type().is_file() {
        return Err(load_error(
            stage,
            SnapshotArtifactLoadFailure::NotRegularFile { artifact },
        ));
    }
    Ok(metadata.len())
}

fn open_load_directory(
    parent: &Path,
    stage: SnapshotArtifactLoadStage,
) -> Result<File, SnapshotArtifactLoadError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY)
        .open(parent)
        .map_err(|source| load_error(stage, SnapshotArtifactLoadFailure::Io(source.kind())))
}

fn open_regular_final(
    directory: &File,
    component: &CString,
    artifact: SnapshotArtifactKind,
    open_stage: SnapshotArtifactLoadStage,
    type_stage: SnapshotArtifactLoadStage,
) -> Result<(File, u64), SnapshotArtifactLoadError> {
    // SAFETY: `directory` is live and `component` is a NUL-terminated
    // single component. Nonblocking and no-follow prevent special-file
    // hangs and final-symlink traversal before the descriptor type check.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            component.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(load_error(
            open_stage,
            SnapshotArtifactLoadFailure::Io(io::Error::last_os_error().kind()),
        ));
    }
    // SAFETY: successful `openat` returned a fresh owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is writable and `file` owns a live descriptor.
    if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(load_error(
            type_stage,
            SnapshotArtifactLoadFailure::Io(io::Error::last_os_error().kind()),
        ));
    }
    // SAFETY: successful `fstat` initialized the structure.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(load_error(
            type_stage,
            SnapshotArtifactLoadFailure::NotRegularFile { artifact },
        ));
    }
    let length = u64::try_from(stat.st_size)
        .map_err(|_| load_error(type_stage, SnapshotArtifactLoadFailure::LengthOverflow))?;
    Ok((file, length))
}

#[cfg(test)]
#[derive(Debug)]
enum PublicationTestAction {
    CreateFinal {
        stage: SnapshotPublicationStage,
        path: PathBuf,
    },
    ReplaceStaging {
        stage: SnapshotPublicationStage,
        directory: PathBuf,
        artifact: SnapshotArtifactKind,
    },
    RemoveStaging {
        stage: SnapshotPublicationStage,
        directory: PathBuf,
        artifact: SnapshotArtifactKind,
    },
    ReplaceParent {
        stage: SnapshotPublicationStage,
        parent: PathBuf,
        moved: PathBuf,
    },
}

#[cfg(test)]
impl PublicationTestAction {
    fn stage(&self) -> SnapshotPublicationStage {
        match self {
            Self::CreateFinal { stage, .. }
            | Self::ReplaceStaging { stage, .. }
            | Self::RemoveStaging { stage, .. }
            | Self::ReplaceParent { stage, .. } => *stage,
        }
    }

    fn perform(self) -> Result<(), io::ErrorKind> {
        match self {
            Self::CreateFinal { path, .. } => {
                std::fs::write(path, b"concurrent-final").map_err(|error| error.kind())
            }
            Self::ReplaceStaging {
                directory,
                artifact,
                ..
            } => alter_staging_entry(&directory, artifact, true),
            Self::RemoveStaging {
                directory,
                artifact,
                ..
            } => alter_staging_entry(&directory, artifact, false),
            Self::ReplaceParent { parent, moved, .. } => {
                std::fs::rename(&parent, moved).map_err(|error| error.kind())?;
                std::fs::create_dir(parent).map_err(|error| error.kind())
            }
        }
    }
}

#[cfg(test)]
fn alter_staging_entry(
    directory: &Path,
    artifact: SnapshotArtifactKind,
    replace: bool,
) -> Result<(), io::ErrorKind> {
    let prefix = match artifact {
        SnapshotArtifactKind::State => STATE_STAGING_PREFIX,
        SnapshotArtifactKind::Memory => MEMORY_STAGING_PREFIX,
    };
    let entries = std::fs::read_dir(directory).map_err(|error| error.kind())?;
    for entry in entries {
        let entry = entry.map_err(|error| error.kind())?;
        if entry.file_name().as_bytes().starts_with(prefix) {
            let path = entry.path();
            std::fs::remove_file(&path).map_err(|error| error.kind())?;
            if replace {
                return std::fs::write(path, b"replacement-staging").map_err(|error| error.kind());
            }
            return Ok(());
        }
    }
    Err(io::ErrorKind::NotFound)
}

#[cfg(test)]
#[derive(Debug, Default)]
struct PublicationTestHook {
    failures: Vec<SnapshotPublicationStage>,
    action: Option<PublicationTestAction>,
    random_names: std::collections::VecDeque<[u8; STAGING_RANDOM_BYTES]>,
    random_failure: bool,
    order: Vec<SnapshotPublicationStage>,
}

#[cfg(test)]
thread_local! {
    static PUBLICATION_TEST_HOOK: std::cell::RefCell<PublicationTestHook> =
        std::cell::RefCell::new(PublicationTestHook::default());
}

fn enter_publication_stage(stage: SnapshotPublicationStage) -> Result<(), io::ErrorKind> {
    #[cfg(test)]
    {
        let action = PUBLICATION_TEST_HOOK.with(|hook| {
            let mut hook = hook.borrow_mut();
            hook.order.push(stage);
            if hook.failures.contains(&stage) {
                return Err(io::ErrorKind::Other);
            }
            if hook.action.as_ref().map(PublicationTestAction::stage) == Some(stage) {
                Ok(hook.action.take())
            } else {
                Ok(None)
            }
        })?;
        if let Some(action) = action {
            action.perform()?;
        }
        Ok(())
    }
    #[cfg(not(test))]
    {
        let _ = stage;
        Ok(())
    }
}

#[cfg(test)]
pub(super) fn with_publication_failure<T>(
    stage: SnapshotPublicationStage,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotPublicationStage>) {
    PUBLICATION_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = PublicationTestHook {
            failures: vec![stage],
            action: None,
            random_names: std::collections::VecDeque::new(),
            random_failure: false,
            order: Vec::new(),
        };
    });
    let result = operation();
    let order = PUBLICATION_TEST_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        let order = std::mem::take(&mut hook.order);
        hook.failures.clear();
        hook.action = None;
        order
    });
    (result, order)
}

#[cfg(test)]
pub(super) fn with_publication_failures<T>(
    failures: Vec<SnapshotPublicationStage>,
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotPublicationStage>) {
    PUBLICATION_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = PublicationTestHook {
            failures,
            action: None,
            random_names: std::collections::VecDeque::new(),
            random_failure: false,
            order: Vec::new(),
        };
    });
    let result = operation();
    let order = PUBLICATION_TEST_HOOK.with(|hook| {
        let mut hook = hook.borrow_mut();
        let order = std::mem::take(&mut hook.order);
        hook.failures.clear();
        order
    });
    (result, order)
}

#[cfg(test)]
pub(super) fn with_final_collision<T>(
    stage: SnapshotPublicationStage,
    path: PathBuf,
    operation: impl FnOnce() -> T,
) -> T {
    with_publication_action(
        PublicationTestAction::CreateFinal { stage, path },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_staging_replacement<T>(
    stage: SnapshotPublicationStage,
    directory: PathBuf,
    artifact: SnapshotArtifactKind,
    operation: impl FnOnce() -> T,
) -> T {
    with_publication_action(
        PublicationTestAction::ReplaceStaging {
            stage,
            directory,
            artifact,
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_staging_removal<T>(
    stage: SnapshotPublicationStage,
    directory: PathBuf,
    artifact: SnapshotArtifactKind,
    operation: impl FnOnce() -> T,
) -> T {
    with_publication_action(
        PublicationTestAction::RemoveStaging {
            stage,
            directory,
            artifact,
        },
        operation,
    )
}

#[cfg(test)]
pub(super) fn with_parent_replacement<T>(
    stage: SnapshotPublicationStage,
    parent: PathBuf,
    moved: PathBuf,
    operation: impl FnOnce() -> T,
) -> T {
    with_publication_action(
        PublicationTestAction::ReplaceParent {
            stage,
            parent,
            moved,
        },
        operation,
    )
}

#[cfg(test)]
fn with_publication_action<T>(action: PublicationTestAction, operation: impl FnOnce() -> T) -> T {
    PUBLICATION_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = PublicationTestHook {
            failures: Vec::new(),
            action: Some(action),
            random_names: std::collections::VecDeque::new(),
            random_failure: false,
            order: Vec::new(),
        };
    });
    let result = operation();
    PUBLICATION_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = PublicationTestHook::default();
    });
    result
}

#[cfg(test)]
pub(super) fn with_staging_random_names<T>(
    random_names: Vec<[u8; STAGING_RANDOM_BYTES]>,
    operation: impl FnOnce() -> T,
) -> T {
    PUBLICATION_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = PublicationTestHook {
            failures: Vec::new(),
            action: None,
            random_names: random_names.into(),
            random_failure: false,
            order: Vec::new(),
        };
    });
    let result = operation();
    PUBLICATION_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = PublicationTestHook::default();
    });
    result
}

#[cfg(test)]
pub(super) fn with_staging_random_failure<T>(operation: impl FnOnce() -> T) -> T {
    PUBLICATION_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = PublicationTestHook {
            failures: Vec::new(),
            action: None,
            random_names: std::collections::VecDeque::new(),
            random_failure: true,
            order: Vec::new(),
        };
    });
    let result = operation();
    PUBLICATION_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = PublicationTestHook::default();
    });
    result
}

#[cfg(test)]
pub(super) fn with_publication_trace<T>(
    operation: impl FnOnce() -> T,
) -> (T, Vec<SnapshotPublicationStage>) {
    PUBLICATION_TEST_HOOK.with(|hook| {
        *hook.borrow_mut() = PublicationTestHook::default();
    });
    let result = operation();
    let order = PUBLICATION_TEST_HOOK.with(|hook| std::mem::take(&mut hook.borrow_mut().order));
    (result, order)
}
