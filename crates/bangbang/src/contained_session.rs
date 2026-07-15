use std::fmt;
use std::os::unix::ffi::OsStrExt;
#[cfg(not(target_os = "macos"))]
use std::os::unix::net::UnixStream;
use std::path::Path;

use bangbang_session::{GrantId, SnapshotOutputChild, SocketChild};
#[cfg(not(target_os = "macos"))]
use bangbang_session::{Readiness, TerminalCategory};

const GRANT_REFERENCE_PREFIX: &str = "bangbang-grant:";

/// Stable private bootstrap failure that never includes identity or path data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContainedSessionError;

impl fmt::Display for ContainedSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private launcher session failed")
    }
}

impl std::error::Error for ContainedSessionError {}

/// Stable failure for an explicit contained resource claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GrantClaimError;

impl fmt::Display for GrantClaimError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private resource grant failed")
    }
}

impl std::error::Error for GrantClaimError {}

pub(crate) fn grant_reference_id(reference: &Path) -> Result<Option<GrantId>, GrantClaimError> {
    let reference = reference.as_os_str().as_bytes();
    let Some(id) = reference.strip_prefix(GRANT_REFERENCE_PREFIX.as_bytes()) else {
        return Ok(None);
    };
    let id = std::str::from_utf8(id).map_err(|_| GrantClaimError)?;
    GrantId::parse(id).map(Some).map_err(|_| GrantClaimError)
}

fn socket_directory_reference(
    reference: &Path,
) -> Result<Option<(GrantId, SocketChild)>, GrantClaimError> {
    let reference = reference.as_os_str().as_bytes();
    let Some(value) = reference.strip_prefix(GRANT_REFERENCE_PREFIX.as_bytes()) else {
        return Ok(None);
    };
    let mut components = value.split(|byte| *byte == b'/');
    let id = components.next().ok_or(GrantClaimError)?;
    let child = components.next().ok_or(GrantClaimError)?;
    if components.next().is_some() {
        return Err(GrantClaimError);
    }
    let id = std::str::from_utf8(id).map_err(|_| GrantClaimError)?;
    let child = std::str::from_utf8(child).map_err(|_| GrantClaimError)?;
    Ok(Some((
        GrantId::parse(id).map_err(|_| GrantClaimError)?,
        SocketChild::parse(child).map_err(|_| GrantClaimError)?,
    )))
}

fn snapshot_output_reference(
    reference: &Path,
) -> Result<Option<(GrantId, SnapshotOutputChild)>, GrantClaimError> {
    let reference = reference.as_os_str().as_bytes();
    let Some(value) = reference.strip_prefix(GRANT_REFERENCE_PREFIX.as_bytes()) else {
        return Ok(None);
    };
    let Some(separator) = value.iter().position(|byte| *byte == b'/') else {
        return Err(GrantClaimError);
    };
    let (id, child) = value.split_at(separator);
    let child = child.get(1..).ok_or(GrantClaimError)?;
    if child.contains(&b'/') {
        return Err(GrantClaimError);
    }
    let id = std::str::from_utf8(id).map_err(|_| GrantClaimError)?;
    let child = std::str::from_utf8(child).map_err(|_| GrantClaimError)?;
    Ok(Some((
        GrantId::parse(id).map_err(|_| GrantClaimError)?,
        SnapshotOutputChild::parse(child).map_err(|_| GrantClaimError)?,
    )))
}

#[cfg(test)]
mod reference_tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::{Path, PathBuf};

    use bangbang_session::{MAX_GRANT_ID_BYTES, MAX_SNAPSHOT_OUTPUT_CHILD_BYTES};

    use super::{
        GrantClaimError, grant_reference_id, snapshot_output_reference, socket_directory_reference,
    };

    #[test]
    fn grant_references_use_one_exact_case_sensitive_bounded_grammar() {
        for ordinary in [
            "ordinary-path",
            "Bangbang-grant:kernel",
            "bangbang-grants:kernel",
            "bangbang-grant",
        ] {
            assert!(
                grant_reference_id(Path::new(ordinary))
                    .expect("ordinary paths should classify")
                    .is_none()
            );
        }

        for invalid in [
            "bangbang-grant:",
            "bangbang-grant:with/slash",
            "bangbang-grant:unicode-☃",
        ] {
            assert_eq!(
                grant_reference_id(Path::new(invalid))
                    .expect_err("reserved malformed references must fail closed"),
                GrantClaimError
            );
        }
        let too_long = format!("bangbang-grant:{}", "a".repeat(MAX_GRANT_ID_BYTES + 1));
        assert_eq!(
            grant_reference_id(Path::new(&too_long))
                .expect_err("overlong references must fail closed"),
            GrantClaimError
        );

        let maximum = "a".repeat(MAX_GRANT_ID_BYTES);
        let reference = format!("bangbang-grant:{maximum}");
        let id = grant_reference_id(Path::new(&reference))
            .expect("maximum reference should classify")
            .expect("maximum reference should contain an ID");
        assert_eq!(id.as_bytes(), maximum.as_bytes());
        assert_eq!(GrantClaimError.to_string(), "private resource grant failed");
        assert!(!format!("{id:?} {GrantClaimError:?}").contains(&maximum));
    }

    #[test]
    fn directory_references_bind_one_exact_id_and_child() {
        let (id, child) =
            socket_directory_reference(Path::new("bangbang-grant:api-directory/api.sock"))
                .expect("reference should classify")
                .expect("reference should be explicit");
        assert_eq!(id.as_bytes(), b"api-directory");
        assert_eq!(child.as_bytes(), b"api.sock");

        for ordinary in ["ordinary/socket", "Bangbang-grant:api/socket"] {
            assert!(
                socket_directory_reference(Path::new(ordinary))
                    .expect("ordinary path should classify")
                    .is_none()
            );
        }
        for malformed in [
            "bangbang-grant:api",
            "bangbang-grant:/socket",
            "bangbang-grant:api/",
            "bangbang-grant:api/.",
            "bangbang-grant:api/../socket",
            "bangbang-grant:api/nested/socket",
            "bangbang-grant:api/with space",
            "bangbang-grant:api/雪",
        ] {
            assert_eq!(
                socket_directory_reference(Path::new(malformed)),
                Err(GrantClaimError)
            );
        }
        let non_utf8 = PathBuf::from(OsString::from_vec(b"bangbang-grant:api/\xff".to_vec()));
        assert_eq!(socket_directory_reference(&non_utf8), Err(GrantClaimError));
    }

    #[test]
    fn snapshot_outputs_bind_one_exact_id_and_utf8_child() {
        for child_value in ["state.snap", "memory image", "雪", r"back\\slash"] {
            let reference = format!("bangbang-grant:output/{child_value}");
            let (id, child) = snapshot_output_reference(Path::new(&reference))
                .expect("reference should classify")
                .expect("reference should be explicit");
            assert_eq!(id.as_bytes(), b"output");
            assert_eq!(child.as_bytes(), child_value.as_bytes());
            assert!(!format!("{id:?} {child:?}").contains(child_value));
        }

        for ordinary in ["ordinary/snapshot", "Bangbang-grant:output/state"] {
            assert!(
                snapshot_output_reference(Path::new(ordinary))
                    .expect("ordinary path should classify")
                    .is_none()
            );
        }
        for malformed in [
            "bangbang-grant:output",
            "bangbang-grant:/state",
            "bangbang-grant:output/",
            "bangbang-grant:output/.",
            "bangbang-grant:output/..",
            "bangbang-grant:output/nested/state",
            "bangbang-grant:output/nul\0name",
        ] {
            assert_eq!(
                snapshot_output_reference(Path::new(malformed)),
                Err(GrantClaimError)
            );
        }
        let overlong = format!(
            "bangbang-grant:output/{}",
            "a".repeat(MAX_SNAPSHOT_OUTPUT_CHILD_BYTES + 1)
        );
        assert_eq!(
            snapshot_output_reference(Path::new(&overlong)),
            Err(GrantClaimError)
        );
        let non_utf8 = PathBuf::from(OsString::from_vec(b"bangbang-grant:output/\xff".to_vec()));
        assert_eq!(snapshot_output_reference(&non_utf8), Err(GrantClaimError));
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::env;
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io::{Read, Write};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::net::{UnixDatagram, UnixStream};
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use bangbang_runtime::boot::BootSourceFiles;
    use bangbang_runtime::snapshot_artifact::{
        SnapshotArtifactKind, SnapshotArtifactOutput, SnapshotStagingOwnership,
        SnapshotStagingTracker, SnapshotStagingTrackingError,
    };
    use bangbang_session::macos::grant_registry::{
        CommittedGrantBatch, DirectoryGrantRegistry, FileGrantRegistry, GrantRegistry,
        GrantedDirectory, StagedGrantBatch,
    };
    use bangbang_session::macos::grant_transport::receive_grant;
    use bangbang_session::macos::runtime::{
        SnapshotStagingKind, SnapshotStagingName, SnapshotStagingOwnershipRecord, WorkerNamespace,
        WorkerSocketNamespace,
    };
    use bangbang_session::macos::{set_cloexec, verify_peer, verify_peer_pid};
    use bangbang_session::{
        Frame, FrameDecoder, GRANT_FD, GrantAccess, GrantId, Message, ObjectIdentity, Readiness,
        ResourceRole, SESSION_ENV_KEY, SESSION_ENV_VALUE, SESSION_FD, SOCKET_BROKER_FD, SessionId,
        SnapshotOutputChild, TerminalCategory, WorkerLifecycle, WorkerPolicy, encode_frame,
    };

    use super::{
        ContainedSessionError, GrantClaimError, SocketChild, grant_reference_id,
        snapshot_output_reference, socket_directory_reference,
    };

    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
    #[cfg(feature = "grant-integration-probe")]
    const GRANT_DELAY_PROBE: &str = "--bangbang-internal-grant-delay-v1";
    #[cfg(feature = "grant-integration-probe")]
    const GRANT_DELAY_READY: &str = "status: grant integration delay ready";

    #[cfg(feature = "grant-integration-probe")]
    fn grant_delay_requested() -> bool {
        let arguments = env::args_os().skip(1).collect::<Vec<_>>();
        match arguments.as_slice() {
            [probe] => probe == OsStr::new(GRANT_DELAY_PROBE),
            [id, _, start, _, start_cpu, _, parent_cpu, _, probe] => {
                id == OsStr::new("--id")
                    && start == OsStr::new("--start-time-us")
                    && start_cpu == OsStr::new("--start-time-cpu-us")
                    && parent_cpu == OsStr::new("--parent-cpu-time-us")
                    && probe == OsStr::new(GRANT_DELAY_PROBE)
            }
            _ => false,
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ControlState {
        Running,
        Cancelled,
        Disconnected,
        ProtocolFailure,
    }

    #[derive(Debug)]
    struct SharedControl {
        state: Mutex<ControlState>,
        closing: AtomicBool,
    }

    impl SharedControl {
        fn new(state: ControlState) -> Self {
            Self {
                state: Mutex::new(state),
                closing: AtomicBool::new(false),
            }
        }

        fn state(&self) -> Result<ControlState, ContainedSessionError> {
            self.state
                .lock()
                .map(|state| *state)
                .map_err(|_| ContainedSessionError)
        }

        fn set(&self, state: ControlState) -> Result<(), ContainedSessionError> {
            let mut current = self.state.lock().map_err(|_| ContainedSessionError)?;
            if *current == ControlState::Running {
                *current = state;
            }
            Ok(())
        }
    }

    /// Shared one-time authority for exact contained resource claims.
    #[derive(Clone)]
    pub(crate) struct GrantAuthority {
        registry: Arc<Mutex<Option<FileGrantRegistry>>>,
    }

    impl std::fmt::Debug for GrantAuthority {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("GrantAuthority")
                .field("registry", &"<redacted>")
                .finish()
        }
    }

    impl GrantAuthority {
        fn new(registry: FileGrantRegistry) -> Self {
            Self {
                registry: Arc::new(Mutex::new(Some(registry))),
            }
        }

        pub(crate) fn claim_read_only_file(
            &self,
            reference: &Path,
            role: ResourceRole,
        ) -> Result<Option<File>, GrantClaimError> {
            self.claim_file(reference, role, GrantAccess::ReadOnly)
        }

        pub(crate) fn claim_file(
            &self,
            reference: &Path,
            role: ResourceRole,
            access: GrantAccess,
        ) -> Result<Option<File>, GrantClaimError> {
            let Some(id) = grant_reference_id(reference)? else {
                return Ok(None);
            };
            let mut registry = self.registry.lock().map_err(|_| GrantClaimError)?;
            let grant = registry
                .as_mut()
                .ok_or(GrantClaimError)?
                .take_file(&id, role, access)
                .map_err(|_| GrantClaimError)?;
            Ok(Some(File::from(grant.into_owned_fd())))
        }

        pub(crate) fn claim_boot_files(
            &self,
            kernel_reference: &Path,
            initrd_reference: Option<&Path>,
        ) -> Result<BootSourceFiles, GrantClaimError> {
            let kernel_id = grant_reference_id(kernel_reference)?;
            let initrd_id = initrd_reference
                .map(grant_reference_id)
                .transpose()?
                .flatten();
            let mut requests = Vec::with_capacity(2);
            if let Some(id) = &kernel_id {
                requests.push((id.clone(), ResourceRole::KernelImage, GrantAccess::ReadOnly));
            }
            if let Some(id) = &initrd_id {
                requests.push((id.clone(), ResourceRole::InitrdImage, GrantAccess::ReadOnly));
            }
            if requests.is_empty() {
                return Ok(BootSourceFiles::default());
            }

            let mut registry = self.registry.lock().map_err(|_| GrantClaimError)?;
            let files = registry
                .as_mut()
                .ok_or(GrantClaimError)?
                .take_files(&requests)
                .map_err(|_| GrantClaimError)?;
            let mut files = files.into_iter();
            let kernel = match kernel_id {
                Some(_) => Some(File::from(
                    files.next().ok_or(GrantClaimError)?.into_owned_fd(),
                )),
                None => None,
            };
            let initrd = match initrd_id {
                Some(_) => Some(File::from(
                    files.next().ok_or(GrantClaimError)?.into_owned_fd(),
                )),
                None => None,
            };
            debug_assert!(files.next().is_none());
            Ok(BootSourceFiles::new(kernel, initrd))
        }

        pub(crate) fn duplicate_exact_files(
            &self,
            requests: &[(GrantId, ResourceRole, GrantAccess)],
        ) -> Result<Vec<File>, GrantClaimError> {
            let registry = self.registry.lock().map_err(|_| GrantClaimError)?;
            registry
                .as_ref()
                .ok_or(GrantClaimError)?
                .duplicate_files(requests)
                .map_err(|_| GrantClaimError)
                .map(|files| {
                    files
                        .into_iter()
                        .map(|file| File::from(file.into_owned_fd()))
                        .collect()
                })
        }

        pub(crate) fn take_exact_files(
            &self,
            requests: &[(GrantId, ResourceRole, GrantAccess)],
        ) -> Result<Vec<File>, GrantClaimError> {
            let mut registry = self.registry.lock().map_err(|_| GrantClaimError)?;
            registry
                .as_mut()
                .ok_or(GrantClaimError)?
                .take_files(requests)
                .map_err(|_| GrantClaimError)
                .map(|files| {
                    files
                        .into_iter()
                        .map(|file| File::from(file.into_owned_fd()))
                        .collect()
                })
        }

        #[cfg(feature = "grant-integration-probe")]
        pub(crate) fn with_registry<T>(
            &self,
            consumer: impl FnOnce(&mut FileGrantRegistry) -> Result<T, ContainedSessionError>,
        ) -> Result<T, ContainedSessionError> {
            let mut registry = self.registry.lock().map_err(|_| ContainedSessionError)?;
            consumer(registry.as_mut().ok_or(ContainedSessionError)?)
        }

        fn invalidate(&self) {
            let mut registry = match self.registry.lock() {
                Ok(registry) => registry,
                Err(error) => error.into_inner(),
            };
            registry.take();
        }
    }

    /// One exact contained socket-directory claim.
    pub(crate) struct ClaimedSocketDirectory {
        pub(crate) directory: GrantedDirectory,
        pub(crate) child: SocketChild,
    }

    /// One retained snapshot-output directory paired with one validated child.
    #[derive(Clone)]
    pub(crate) struct ClaimedSnapshotOutput {
        directory: Rc<GrantedDirectory>,
        child: SnapshotOutputChild,
    }

    /// Durable bridge from runtime staging events into the private session namespace.
    pub(crate) struct SnapshotStagingRecordTracker {
        namespace: WorkerSocketNamespace,
    }

    impl SnapshotStagingRecordTracker {
        pub(crate) const fn new(namespace: WorkerSocketNamespace) -> Self {
            Self { namespace }
        }

        fn record_for(
            ownership: &SnapshotStagingOwnership,
        ) -> Result<SnapshotStagingOwnershipRecord, SnapshotStagingTrackingError> {
            let kind = match ownership.artifact() {
                SnapshotArtifactKind::State => SnapshotStagingKind::State,
                SnapshotArtifactKind::Memory => SnapshotStagingKind::Memory,
            };
            let component = std::str::from_utf8(ownership.component())
                .map_err(|_| SnapshotStagingTrackingError)?;
            let name = SnapshotStagingName::parse(kind, component)
                .map_err(|_| SnapshotStagingTrackingError)?;
            let directory = ownership.directory_identity();
            let file = ownership.file_identity();
            Ok(SnapshotStagingOwnershipRecord::new(
                kind,
                ObjectIdentity {
                    device: directory.device(),
                    inode: directory.inode(),
                },
                name,
                ObjectIdentity {
                    device: file.device(),
                    inode: file.inode(),
                },
            ))
        }
    }

    impl std::fmt::Debug for SnapshotStagingRecordTracker {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("SnapshotStagingRecordTracker")
                .field("namespace", &"<owned>")
                .finish()
        }
    }

    impl SnapshotStagingTracker for SnapshotStagingRecordTracker {
        fn record(
            &self,
            ownership: &SnapshotStagingOwnership,
        ) -> Result<(), SnapshotStagingTrackingError> {
            let record = Self::record_for(ownership)?;
            self.namespace
                .write_snapshot_staging_record(&record)
                .map_err(|_| SnapshotStagingTrackingError)?;
            #[cfg(feature = "grant-integration-probe")]
            crate::grant_integration_probe::hold_after_snapshot_staging_record();
            Ok(())
        }

        fn clear(
            &self,
            ownership: &SnapshotStagingOwnership,
        ) -> Result<(), SnapshotStagingTrackingError> {
            let record = Self::record_for(ownership)?;
            self.namespace
                .clear_snapshot_staging_record(&record)
                .map_err(|_| SnapshotStagingTrackingError)
        }
    }

    impl std::fmt::Debug for ClaimedSnapshotOutput {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ClaimedSnapshotOutput")
                .field("directory", &"<retained>")
                .field("child", &"<redacted>")
                .finish()
        }
    }

    impl ClaimedSnapshotOutput {
        pub(crate) fn artifact_output(
            &self,
            tracker: Arc<dyn SnapshotStagingTracker>,
        ) -> Result<SnapshotArtifactOutput, GrantClaimError> {
            // SAFETY: the retained directory anchor remains live for fcntl;
            // success returns an independently owned close-on-exec descriptor.
            let descriptor =
                unsafe { libc::fcntl(self.directory.anchor_fd(), libc::F_DUPFD_CLOEXEC, 0) };
            if descriptor < 0 {
                return Err(GrantClaimError);
            }
            // SAFETY: descriptor is the fresh duplicate returned above.
            let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
            Ok(SnapshotArtifactOutput::anchored_tracked(
                File::from(descriptor),
                self.child.as_bytes().to_vec(),
                tracker,
            ))
        }
    }

    /// Failure-atomic contained claims for a state/memory output pair.
    #[derive(Debug, Default)]
    pub(crate) struct ClaimedSnapshotOutputs {
        pub(crate) state: Option<ClaimedSnapshotOutput>,
        pub(crate) memory: Option<ClaimedSnapshotOutput>,
    }

    impl std::fmt::Debug for ClaimedSocketDirectory {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ClaimedSocketDirectory")
                .field("directory", &"<owned>")
                .field("child", &"<redacted>")
                .finish()
        }
    }

    /// Main-thread authority for active directory scopes.
    #[derive(Clone)]
    pub(crate) struct DirectoryGrantAuthority {
        registry: Rc<RefCell<Option<DirectoryGrantRegistry>>>,
        retained_snapshot: Rc<RefCell<HashMap<GrantId, Rc<GrantedDirectory>>>>,
    }

    impl std::fmt::Debug for DirectoryGrantAuthority {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("DirectoryGrantAuthority")
                .field("registry", &"<redacted>")
                .finish()
        }
    }

    impl DirectoryGrantAuthority {
        fn new(registry: DirectoryGrantRegistry) -> Self {
            Self {
                registry: Rc::new(RefCell::new(Some(registry))),
                retained_snapshot: Rc::new(RefCell::new(HashMap::new())),
            }
        }

        pub(crate) fn claim_socket_directory(
            &self,
            reference: &Path,
            role: ResourceRole,
        ) -> Result<Option<ClaimedSocketDirectory>, GrantClaimError> {
            let Some((id, child)) = socket_directory_reference(reference)? else {
                return Ok(None);
            };
            let directory = self
                .registry
                .try_borrow_mut()
                .map_err(|_| GrantClaimError)?
                .as_mut()
                .ok_or(GrantClaimError)?
                .take_scoped_directory(&id, role)
                .map_err(|_| GrantClaimError)?;
            Ok(Some(ClaimedSocketDirectory { directory, child }))
        }

        pub(crate) fn claim_snapshot_outputs(
            &self,
            state_reference: &Path,
            memory_reference: &Path,
        ) -> Result<ClaimedSnapshotOutputs, GrantClaimError> {
            let state = snapshot_output_reference(state_reference)?;
            let memory = snapshot_output_reference(memory_reference)?;
            if matches!(
                (&state, &memory),
                (Some((state_id, state_child)), Some((memory_id, memory_child)))
                    if state_id == memory_id && state_child == memory_child
            ) {
                return Err(GrantClaimError);
            }

            let mut distinct = HashSet::with_capacity(2);
            let mut requested = Vec::with_capacity(2);
            for (id, _) in [&state, &memory].into_iter().flatten() {
                if distinct.insert(id.clone()) {
                    requested.push(id.clone());
                }
            }
            if requested.is_empty() {
                return Ok(ClaimedSnapshotOutputs::default());
            }

            let mut retained = self
                .retained_snapshot
                .try_borrow_mut()
                .map_err(|_| GrantClaimError)?;
            let missing = requested
                .iter()
                .filter(|id| !retained.contains_key(*id))
                .cloned()
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                retained
                    .try_reserve(missing.len())
                    .map_err(|_| GrantClaimError)?;
                let requests = missing
                    .iter()
                    .cloned()
                    .map(|id| (id, ResourceRole::SnapshotOutputDirectory))
                    .collect::<Vec<_>>();
                let directories = self
                    .registry
                    .try_borrow_mut()
                    .map_err(|_| GrantClaimError)?
                    .as_mut()
                    .ok_or(GrantClaimError)?
                    .take_scoped_directories(&requests)
                    .map_err(|_| GrantClaimError)?;
                for (id, directory) in missing.into_iter().zip(directories) {
                    let previous = retained.insert(id, Rc::new(directory));
                    debug_assert!(previous.is_none());
                }
            }

            let claim = |reference: Option<(GrantId, SnapshotOutputChild)>| {
                reference
                    .map(|(id, child)| {
                        retained
                            .get(&id)
                            .cloned()
                            .map(|directory| ClaimedSnapshotOutput { directory, child })
                            .ok_or(GrantClaimError)
                    })
                    .transpose()
            };
            Ok(ClaimedSnapshotOutputs {
                state: claim(state)?,
                memory: claim(memory)?,
            })
        }

        #[cfg(feature = "grant-integration-probe")]
        fn with_registry<T>(
            &self,
            consumer: impl FnOnce(&mut DirectoryGrantRegistry) -> Result<T, ContainedSessionError>,
        ) -> Result<T, ContainedSessionError> {
            let mut registry = self
                .registry
                .try_borrow_mut()
                .map_err(|_| ContainedSessionError)?;
            consumer(registry.as_mut().ok_or(ContainedSessionError)?)
        }

        fn invalidate(&self) {
            if let Ok(mut registry) = self.registry.try_borrow_mut() {
                registry.take();
            }
            if let Ok(mut retained) = self.retained_snapshot.try_borrow_mut() {
                retained.clear();
            }
        }
    }

    /// Move-only authenticated launcher broker endpoint.
    pub(crate) struct SocketBrokerEndpoint {
        pub(crate) socket: UnixDatagram,
        pub(crate) session: SessionId,
        pub(crate) launcher_pid: libc::pid_t,
    }

    impl std::fmt::Debug for SocketBrokerEndpoint {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("SocketBrokerEndpoint")
                .field("socket", &"<owned>")
                .field("session", &"<redacted>")
                .field("launcher_pid", &"<redacted>")
                .finish()
        }
    }

    /// Owner-thread one-time authority for the private launcher broker endpoint.
    #[derive(Clone)]
    pub(crate) struct SocketBrokerAuthority {
        endpoint: Rc<RefCell<Option<SocketBrokerEndpoint>>>,
    }

    impl std::fmt::Debug for SocketBrokerAuthority {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("SocketBrokerAuthority")
                .field("endpoint", &"<redacted>")
                .finish()
        }
    }

    impl SocketBrokerAuthority {
        fn new(socket: UnixDatagram, session: SessionId, launcher_pid: libc::pid_t) -> Self {
            Self {
                endpoint: Rc::new(RefCell::new(Some(SocketBrokerEndpoint {
                    socket,
                    session,
                    launcher_pid,
                }))),
            }
        }

        pub(crate) fn take_endpoint(&self) -> Result<SocketBrokerEndpoint, GrantClaimError> {
            self.endpoint
                .try_borrow_mut()
                .map_err(|_| GrantClaimError)?
                .take()
                .ok_or(GrantClaimError)
        }

        fn invalidate(&self) {
            if let Ok(mut endpoint) = self.endpoint.try_borrow_mut() {
                endpoint.take();
            }
        }
    }

    pub(crate) struct ContainedSession {
        stream: UnixStream,
        lifecycle: Arc<Mutex<WorkerLifecycle>>,
        namespace: Arc<Mutex<Option<WorkerNamespace>>>,
        control: Arc<SharedControl>,
        wakeup_reader: Option<UnixStream>,
        wakeup_writer: Option<UnixStream>,
        reader: Option<JoinHandle<()>>,
        grants: GrantAuthority,
        directory_grants: DirectoryGrantAuthority,
        socket_broker: SocketBrokerAuthority,
        socket_namespace: WorkerSocketNamespace,
        policy: WorkerPolicy,
        started: bool,
        closed: bool,
    }

    impl std::fmt::Debug for ContainedSession {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ContainedSession")
                .field("identity", &"<redacted>")
                .field("policy", &self.policy)
                .field("started", &self.started)
                .field("closed", &self.closed)
                .finish()
        }
    }

    impl ContainedSession {
        pub(crate) fn bootstrap() -> Result<Option<Self>, ContainedSessionError> {
            let Some(value) = env::var_os(SESSION_ENV_KEY) else {
                return Ok(None);
            };
            // SAFETY: Bootstrap runs at the first line of process main before
            // any application thread is created; removing the private marker
            // prevents it from leaking to later child processes.
            unsafe { env::remove_var(SESSION_ENV_KEY) };
            if value != OsStr::new(SESSION_ENV_VALUE) {
                return Err(ContainedSessionError);
            }
            set_cloexec(SESSION_FD).map_err(|_| ContainedSessionError)?;
            set_cloexec(GRANT_FD).map_err(|_| ContainedSessionError)?;
            set_cloexec(SOCKET_BROKER_FD).map_err(|_| ContainedSessionError)?;
            // SAFETY: The validated private bootstrap contract transfers the
            // fixed descriptor exactly once into this process object.
            let owned = unsafe { OwnedFd::from_raw_fd(SESSION_FD) };
            let mut stream = UnixStream::from(owned);
            // SAFETY: The same validated bootstrap contract transfers fixed
            // grant descriptor 4 exactly once into this process object.
            let grant_owned = unsafe { OwnedFd::from_raw_fd(GRANT_FD) };
            let grant_socket = UnixDatagram::from(grant_owned);
            // SAFETY: The private bootstrap contract transfers fixed broker fd 5 once.
            let broker_owned = unsafe { OwnedFd::from_raw_fd(SOCKET_BROKER_FD) };
            let broker_socket = UnixDatagram::from(broker_owned);
            stream
                .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
                .map_err(|_| ContainedSessionError)?;
            // SAFETY: `getppid` has no pointer or ownership contract.
            let parent = unsafe { libc::getppid() };
            verify_peer(stream.as_raw_fd(), parent).map_err(|_| ContainedSessionError)?;
            verify_peer_pid(grant_socket.as_raw_fd(), parent).map_err(|_| ContainedSessionError)?;
            verify_peer_pid(broker_socket.as_raw_fd(), parent)
                .map_err(|_| ContainedSessionError)?;

            let mut decoder = FrameDecoder::default();
            let mut lifecycle = WorkerLifecycle::new();
            let hello = lifecycle.hello().map_err(|_| ContainedSessionError)?;
            write_frame(&mut stream, hello)?;
            let start = read_frame(&mut stream, &mut decoder, handshake_deadline()?)?;
            let policy = match lifecycle
                .receive(start)
                .map_err(|_| ContainedSessionError)?
            {
                Message::Start(policy) => policy,
                _ => return Err(ContainedSessionError),
            };
            install_worker_policy(policy, parent)?;
            let session = lifecycle.session().ok_or(ContainedSessionError)?;
            let namespace = WorkerNamespace::create(session).map_err(|_| ContainedSessionError)?;
            namespace.enter().map_err(|_| ContainedSessionError)?;
            let identity = namespace.identity();
            let socket_namespace = namespace
                .socket_namespace()
                .map_err(|_| ContainedSessionError)?;
            let prepared = lifecycle
                .prepared(identity.device, identity.inode)
                .map_err(|_| ContainedSessionError)?;
            write_frame(&mut stream, prepared)?;

            #[cfg(feature = "grant-integration-probe")]
            let receive_grants = if grant_delay_requested() {
                println!("{GRANT_DELAY_READY}");
                std::io::stdout()
                    .flush()
                    .map_err(|_| ContainedSessionError)?;
                false
            } else {
                true
            };
            #[cfg(not(feature = "grant-integration-probe"))]
            let receive_grants = true;

            let grant_outcome = receive_grant_batch(
                &mut stream,
                &mut decoder,
                &mut lifecycle,
                &grant_socket,
                session,
                handshake_deadline()?,
                receive_grants,
            )?;
            drop(grant_socket);
            let (mut grants, cancelled) = match grant_outcome {
                GrantPhaseOutcome::Committed(committed) => {
                    let accepted = lifecycle
                        .grants_accepted(
                            committed.batch,
                            committed.grant_count,
                            committed.final_sequence,
                        )
                        .map_err(|_| ContainedSessionError)?;
                    write_frame(&mut stream, accepted)?;
                    (committed.registry, false)
                }
                GrantPhaseOutcome::Cancelled => (GrantRegistry::default(), true),
            };
            let started = if cancelled {
                false
            } else {
                let next = read_frame(&mut stream, &mut decoder, handshake_deadline()?)?;
                let next = lifecycle.receive(next).map_err(|_| ContainedSessionError)?;
                match next {
                    Message::Proceed => {
                        verify_peer(stream.as_raw_fd(), parent)
                            .map_err(|_| ContainedSessionError)?;
                        let starting = lifecycle.starting().map_err(|_| ContainedSessionError)?;
                        write_frame(&mut stream, starting)?;
                        true
                    }
                    Message::Cancel(_) => false,
                    _ => return Err(ContainedSessionError),
                }
            };
            stream
                .set_read_timeout(None)
                .map_err(|_| ContainedSessionError)?;
            stream
                .set_write_timeout(None)
                .map_err(|_| ContainedSessionError)?;

            let initial_state = if started {
                ControlState::Running
            } else {
                ControlState::Cancelled
            };
            let control = Arc::new(SharedControl::new(initial_state));
            let file_grants = GrantAuthority::new(grants.take_file_registry());
            let directory_grants = DirectoryGrantAuthority::new(grants.take_directory_registry());
            let socket_broker = SocketBrokerAuthority::new(broker_socket, session, parent);
            let lifecycle = Arc::new(Mutex::new(lifecycle));
            let namespace = Arc::new(Mutex::new(Some(namespace)));
            let (wakeup_reader, mut wakeup_writer) =
                UnixStream::pair().map_err(|_| ContainedSessionError)?;
            let reader = if started {
                let reader_stream = stream.try_clone().map_err(|_| ContainedSessionError)?;
                let reader_wakeup = wakeup_writer
                    .try_clone()
                    .map_err(|_| ContainedSessionError)?;
                Some(spawn_reader(
                    reader_stream,
                    decoder,
                    Arc::clone(&lifecycle),
                    Arc::clone(&namespace),
                    Arc::clone(&control),
                    file_grants.clone(),
                    reader_wakeup,
                )?)
            } else {
                wakeup_writer
                    .write_all(&[1])
                    .map_err(|_| ContainedSessionError)?;
                None
            };
            Ok(Some(Self {
                stream,
                lifecycle,
                namespace,
                control,
                wakeup_reader: Some(wakeup_reader),
                wakeup_writer: Some(wakeup_writer),
                reader,
                grants: file_grants,
                directory_grants,
                socket_broker,
                socket_namespace,
                policy,
                started,
                closed: false,
            }))
        }

        pub(crate) fn take_wakeup_pair(
            &mut self,
        ) -> Result<(UnixStream, UnixStream), ContainedSessionError> {
            Ok((
                self.wakeup_reader.take().ok_or(ContainedSessionError)?,
                self.wakeup_writer.take().ok_or(ContainedSessionError)?,
            ))
        }

        pub(crate) fn shutdown_requested(&self) -> Result<bool, ContainedSessionError> {
            match self.control.state()? {
                ControlState::Running => Ok(false),
                ControlState::Cancelled => Ok(true),
                ControlState::Disconnected | ControlState::ProtocolFailure => {
                    Err(ContainedSessionError)
                }
            }
        }

        pub(crate) fn was_cancelled(&self) -> bool {
            self.control.state().ok() == Some(ControlState::Cancelled)
        }

        pub(crate) fn grant_authority(&self) -> Option<GrantAuthority> {
            self.started.then(|| self.grants.clone())
        }

        pub(crate) fn directory_grant_authority(&self) -> Option<DirectoryGrantAuthority> {
            self.started.then(|| self.directory_grants.clone())
        }

        pub(crate) fn socket_broker_authority(&self) -> Option<SocketBrokerAuthority> {
            self.started.then(|| self.socket_broker.clone())
        }

        pub(crate) fn socket_namespace(
            &self,
        ) -> Result<Option<WorkerSocketNamespace>, ContainedSessionError> {
            if !self.started {
                return Ok(None);
            }
            self.socket_namespace
                .try_clone()
                .map(Some)
                .map_err(|_| ContainedSessionError)
        }

        #[cfg(feature = "grant-integration-probe")]
        pub(crate) fn verify_launch_policy(
            &self,
            no_file: u64,
            file_size: Option<u64>,
            daemonized: bool,
        ) -> Result<(), ContainedSessionError> {
            if self.policy.no_file() != no_file
                || self.policy.file_size() != file_size
                || self.policy.is_daemonized() != daemonized
                || [
                    "BANGBANG_POLICY_SECRET",
                    "BANGBANG_ORDINARY_AMBIENT",
                    "DYLD_INSERT_LIBRARIES",
                    "DYLD_LIBRARY_PATH",
                    "RUST_LOG",
                    SESSION_ENV_KEY,
                ]
                .into_iter()
                .any(|name| env::var_os(name).is_some())
            {
                return Err(ContainedSessionError);
            }
            verify_installed_limit(libc::RLIMIT_NOFILE, no_file)?;
            if let Some(file_size) = file_size {
                verify_installed_limit(libc::RLIMIT_FSIZE, file_size)?;
            }
            self.namespace
                .lock()
                .map_err(|_| ContainedSessionError)?
                .as_ref()
                .ok_or(ContainedSessionError)?
                .verify_current_directory()
                .map_err(|_| ContainedSessionError)
        }

        #[cfg(feature = "grant-integration-probe")]
        pub(crate) fn with_directory_grants<T>(
            &mut self,
            consumer: impl FnOnce(&mut DirectoryGrantRegistry) -> Result<T, ContainedSessionError>,
        ) -> Result<T, ContainedSessionError> {
            self.directory_grants.with_registry(consumer)
        }

        pub(crate) fn send_ready(
            &mut self,
            readiness: Readiness,
        ) -> Result<(), ContainedSessionError> {
            if self.shutdown_requested()? {
                return Err(ContainedSessionError);
            }
            let frame = self
                .lifecycle
                .lock()
                .map_err(|_| ContainedSessionError)?
                .ready(readiness)
                .map_err(|_| ContainedSessionError)?;
            write_frame(&mut self.stream, frame)
        }

        pub(crate) fn finish(
            &mut self,
            category: TerminalCategory,
            exit_code: u8,
        ) -> Result<(), ContainedSessionError> {
            if self.closed {
                return Ok(());
            }
            let terminal_result = if self.started {
                let frame = self
                    .lifecycle
                    .lock()
                    .map_err(|_| ContainedSessionError)?
                    .terminal(category, exit_code)
                    .map_err(|_| ContainedSessionError)?;
                write_frame(&mut self.stream, frame)
            } else {
                Ok(())
            };
            self.close();
            terminal_result
        }

        fn close(&mut self) {
            if self.closed {
                return;
            }
            self.closed = true;
            self.control.closing.store(true, Ordering::Release);
            self.grants.invalidate();
            self.directory_grants.invalidate();
            self.socket_broker.invalidate();
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
            if let Some(reader) = self.reader.take() {
                let _ = reader.join();
            }
            cleanup_namespace(&self.namespace);
        }
    }

    fn install_worker_policy(
        policy: WorkerPolicy,
        parent: libc::pid_t,
    ) -> Result<(), ContainedSessionError> {
        // SAFETY: Credential and session getters take no retained pointers.
        let (uid, effective_uid, gid, effective_gid, session, parent_session) = unsafe {
            (
                libc::getuid(),
                libc::geteuid(),
                libc::getgid(),
                libc::getegid(),
                libc::getsid(0),
                libc::getsid(parent),
            )
        };
        if uid != policy.uid()
            || effective_uid != policy.uid()
            || gid != policy.gid()
            || effective_gid != policy.gid()
            || session < 0
            || parent_session < 0
            || session != parent_session
            || (policy.is_daemonized() && parent_session != parent)
        {
            return Err(ContainedSessionError);
        }
        if policy.is_daemonized() {
            verify_daemon_standard_streams()?;
        }
        if let Some(file_size) = policy.file_size() {
            install_resource_limit(libc::RLIMIT_FSIZE, file_size)?;
        }
        install_resource_limit(libc::RLIMIT_NOFILE, policy.no_file())
    }

    fn verify_daemon_standard_streams() -> Result<(), ContainedSessionError> {
        let mut expected_device = None;
        for descriptor in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            let mut stat = MaybeUninit::<libc::stat>::uninit();
            // SAFETY: `stat` is writable for one result and the descriptor is borrowed.
            if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
                return Err(ContainedSessionError);
            }
            // SAFETY: Successful `fstat` initialized the complete result.
            let stat = unsafe { stat.assume_init() };
            if stat.st_mode & libc::S_IFMT != libc::S_IFCHR {
                return Err(ContainedSessionError);
            }
            match expected_device {
                None => expected_device = Some(stat.st_rdev),
                Some(device) if device == stat.st_rdev => {}
                Some(_) => return Err(ContainedSessionError),
            }
        }
        Ok(())
    }

    fn install_resource_limit(
        resource: libc::c_int,
        requested: u64,
    ) -> Result<(), ContainedSessionError> {
        let mut inherited = MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: `inherited` is writable for one rlimit result.
        if unsafe { libc::getrlimit(resource, inherited.as_mut_ptr()) } != 0 {
            return Err(ContainedSessionError);
        }
        // SAFETY: Successful `getrlimit` initialized the complete result.
        let inherited = unsafe { inherited.assume_init() };
        let exact = exact_resource_limit(inherited, requested)?;
        // SAFETY: `exact` is a fully initialized limit for a fixed supported resource.
        if unsafe { libc::setrlimit(resource, &raw const exact) } != 0 {
            return Err(ContainedSessionError);
        }
        let mut installed = MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: `installed` is writable for one rlimit result.
        if unsafe { libc::getrlimit(resource, installed.as_mut_ptr()) } != 0 {
            return Err(ContainedSessionError);
        }
        // SAFETY: Successful `getrlimit` initialized the complete result.
        let installed = unsafe { installed.assume_init() };
        if installed.rlim_cur != exact.rlim_cur || installed.rlim_max != exact.rlim_max {
            return Err(ContainedSessionError);
        }
        Ok(())
    }

    #[cfg(feature = "grant-integration-probe")]
    fn verify_installed_limit(
        resource: libc::c_int,
        expected: u64,
    ) -> Result<(), ContainedSessionError> {
        let expected = libc::rlim_t::try_from(expected).map_err(|_| ContainedSessionError)?;
        let mut installed = MaybeUninit::<libc::rlimit>::uninit();
        // SAFETY: `installed` is writable for one rlimit result.
        if unsafe { libc::getrlimit(resource, installed.as_mut_ptr()) } != 0 {
            return Err(ContainedSessionError);
        }
        // SAFETY: Successful `getrlimit` initialized the complete result.
        let installed = unsafe { installed.assume_init() };
        if installed.rlim_cur == expected && installed.rlim_max == expected {
            Ok(())
        } else {
            Err(ContainedSessionError)
        }
    }

    fn exact_resource_limit(
        inherited: libc::rlimit,
        requested: u64,
    ) -> Result<libc::rlimit, ContainedSessionError> {
        let requested = libc::rlim_t::try_from(requested).map_err(|_| ContainedSessionError)?;
        if requested > inherited.rlim_max {
            return Err(ContainedSessionError);
        }
        Ok(libc::rlimit {
            rlim_cur: requested,
            rlim_max: requested,
        })
    }

    impl Drop for ContainedSession {
        fn drop(&mut self) {
            self.close();
        }
    }

    enum GrantPhaseOutcome {
        Committed(CommittedGrantBatch),
        Cancelled,
    }

    fn receive_grant_batch(
        stream: &mut UnixStream,
        decoder: &mut FrameDecoder,
        lifecycle: &mut WorkerLifecycle,
        grant_socket: &UnixDatagram,
        session: bangbang_session::SessionId,
        deadline: Instant,
        receive_grants: bool,
    ) -> Result<GrantPhaseOutcome, ContainedSessionError> {
        let mut staged = StagedGrantBatch::new(session);
        loop {
            if let Some(frame) = decoder.next_frame().map_err(|_| ContainedSessionError)? {
                return receive_grant_control(lifecycle, frame);
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(ContainedSessionError)?;
            let millis = remaining.as_millis().max(1);
            let timeout = i32::try_from(millis).unwrap_or(i32::MAX);
            let mut descriptors = [
                libc::pollfd {
                    fd: stream.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: grant_socket.as_raw_fd(),
                    events: if receive_grants { libc::POLLIN } else { 0 },
                    revents: 0,
                },
            ];
            // SAFETY: descriptors is writable for its exact element count and
            // both descriptors remain owned during the synchronous poll.
            let result = unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    libc::nfds_t::try_from(descriptors.len()).map_err(|_| ContainedSessionError)?,
                    timeout,
                )
            };
            if result < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(ContainedSessionError);
            }
            if result == 0 {
                return Err(ContainedSessionError);
            }
            if descriptors
                .first()
                .is_some_and(|descriptor| descriptor.revents & libc::POLLIN != 0)
            {
                let frame = read_frame(stream, decoder, deadline)?;
                return receive_grant_control(lifecycle, frame);
            }
            if descriptors
                .get(1)
                .is_some_and(|descriptor| descriptor.revents & libc::POLLIN != 0)
            {
                let received = receive_grant(grant_socket).map_err(|_| ContainedSessionError)?;
                if let Some(committed) =
                    staged.accept(received).map_err(|_| ContainedSessionError)?
                {
                    return Ok(GrantPhaseOutcome::Committed(committed));
                }
            }
            let invalid = libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
            if descriptors
                .iter()
                .any(|descriptor| descriptor.revents & invalid != 0)
            {
                return Err(ContainedSessionError);
            }
        }
    }

    fn receive_grant_control(
        lifecycle: &mut WorkerLifecycle,
        frame: Frame,
    ) -> Result<GrantPhaseOutcome, ContainedSessionError> {
        match lifecycle
            .receive(frame)
            .map_err(|_| ContainedSessionError)?
        {
            Message::Cancel(_) => Ok(GrantPhaseOutcome::Cancelled),
            _ => Err(ContainedSessionError),
        }
    }

    fn spawn_reader(
        mut stream: UnixStream,
        mut decoder: FrameDecoder,
        lifecycle: Arc<Mutex<WorkerLifecycle>>,
        namespace: Arc<Mutex<Option<WorkerNamespace>>>,
        control: Arc<SharedControl>,
        grants: GrantAuthority,
        mut wakeup: UnixStream,
    ) -> Result<JoinHandle<()>, ContainedSessionError> {
        thread::Builder::new()
            .name("bangbang-session-control".to_string())
            .spawn(move || {
                let state = reader_loop(&mut stream, &mut decoder, &lifecycle);
                if !control.closing.load(Ordering::Acquire) {
                    grants.invalidate();
                    if state == ControlState::Disconnected {
                        cleanup_namespace(&namespace);
                    }
                    let _ = control.set(state);
                    let _ = wakeup.write_all(&[1]);
                }
            })
            .map_err(|_| ContainedSessionError)
    }

    fn reader_loop(
        stream: &mut UnixStream,
        decoder: &mut FrameDecoder,
        lifecycle: &Mutex<WorkerLifecycle>,
    ) -> ControlState {
        loop {
            match decoder.next_frame() {
                Ok(Some(frame)) => {
                    let message = lifecycle
                        .lock()
                        .map_err(|_| ())
                        .and_then(|mut lifecycle| lifecycle.receive(frame).map_err(|_| ()));
                    return match message {
                        Ok(Message::Cancel(_)) => ControlState::Cancelled,
                        _ => ControlState::ProtocolFailure,
                    };
                }
                Ok(None) => {}
                Err(_) => return ControlState::ProtocolFailure,
            }

            let mut bytes = [0_u8; 4096];
            match stream.read(&mut bytes) {
                Ok(0) => {
                    return if decoder.finish().is_ok() {
                        ControlState::Disconnected
                    } else {
                        ControlState::ProtocolFailure
                    };
                }
                Ok(length) => {
                    let Some(bytes) = bytes.get(..length) else {
                        return ControlState::ProtocolFailure;
                    };
                    if decoder.push(bytes).is_err() {
                        return ControlState::ProtocolFailure;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return ControlState::Disconnected,
            }
        }
    }

    fn read_frame(
        stream: &mut UnixStream,
        decoder: &mut FrameDecoder,
        deadline: Instant,
    ) -> Result<Frame, ContainedSessionError> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(ContainedSessionError)?;
            if let Some(frame) = decoder.next_frame().map_err(|_| ContainedSessionError)? {
                return Ok(frame);
            }
            stream
                .set_read_timeout(Some(remaining))
                .map_err(|_| ContainedSessionError)?;
            let mut bytes = [0_u8; 4096];
            let length = match stream.read(&mut bytes) {
                Ok(length) => length,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err(ContainedSessionError),
            };
            if length == 0 {
                return Err(ContainedSessionError);
            }
            decoder
                .push(bytes.get(..length).ok_or(ContainedSessionError)?)
                .map_err(|_| ContainedSessionError)?;
        }
    }

    fn handshake_deadline() -> Result<Instant, ContainedSessionError> {
        Instant::now()
            .checked_add(HANDSHAKE_TIMEOUT)
            .ok_or(ContainedSessionError)
    }

    fn write_frame(stream: &mut UnixStream, frame: Frame) -> Result<(), ContainedSessionError> {
        let encoded = encode_frame(frame).map_err(|_| ContainedSessionError)?;
        stream
            .write_all(&encoded)
            .map_err(|_| ContainedSessionError)
    }

    fn cleanup_namespace(namespace: &Mutex<Option<WorkerNamespace>>) {
        if let Ok(mut namespace) = namespace.lock()
            && let Some(namespace) = namespace.as_mut()
        {
            let _ = namespace.cleanup();
        }
    }

    #[cfg(test)]
    mod tests {
        use std::fs::OpenOptions;
        use std::io::Read as _;
        use std::mem::MaybeUninit;
        use std::os::fd::{AsRawFd, OwnedFd};
        use std::path::Path;
        use std::sync::{Arc, Barrier};
        use std::thread;

        use bangbang_session::macos::grant_registry::{GrantRegistry, StagedGrantBatch};
        use bangbang_session::macos::grant_transport::ReceivedGrant;
        use bangbang_session::{
            BatchId, GrantAccess, GrantFrame, GrantId, GrantObjectKind, GrantRecord,
            ObjectIdentity, ResourceRole, SessionId,
        };

        use super::{GrantAuthority, GrantClaimError, exact_resource_limit};

        #[test]
        fn exact_limit_never_raises_the_inherited_hard_limit() {
            let inherited = libc::rlimit {
                rlim_cur: 1024,
                rlim_max: 4096,
            };
            let exact =
                exact_resource_limit(inherited, 2048).expect("lower hard limit should validate");
            assert_eq!(exact.rlim_cur, 2048);
            assert_eq!(exact.rlim_max, 2048);
            assert!(exact_resource_limit(inherited, 4097).is_err());
        }

        fn received(
            session: SessionId,
            batch: BatchId,
            sequence: u64,
            record: GrantRecord,
            descriptor: Option<OwnedFd>,
        ) -> ReceivedGrant {
            ReceivedGrant {
                frame: GrantFrame {
                    session,
                    batch,
                    sequence,
                    descriptor_count: record.descriptor_count(),
                    record,
                },
                descriptor,
            }
        }

        fn file_record(
            id: &str,
            role: ResourceRole,
            access: GrantAccess,
            path: &Path,
        ) -> (GrantRecord, OwnedFd) {
            let mut options = OpenOptions::new();
            match access {
                GrantAccess::ReadOnly | GrantAccess::CreateChildren => {
                    options.read(true);
                }
                GrantAccess::WriteOnly => {
                    options.write(true);
                }
                GrantAccess::ReadWrite => {
                    options.read(true).write(true);
                }
            }
            let file = options.open(path).expect("grant fixture should open");
            let descriptor: OwnedFd = file.into();
            let mut stat = MaybeUninit::<libc::stat>::uninit();
            assert_eq!(
                // SAFETY: stat points to writable storage and descriptor is live.
                unsafe { libc::fstat(descriptor.as_raw_fd(), stat.as_mut_ptr()) },
                0
            );
            // SAFETY: successful fstat initialized the complete structure.
            let stat = unsafe { stat.assume_init() };
            // SAFETY: F_GETFL only inspects the live descriptor.
            let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFL) };
            assert!(flags >= 0);
            (
                GrantRecord::Descriptor {
                    id: GrantId::parse(id).expect("grant ID should parse"),
                    role,
                    access,
                    kind: GrantObjectKind::RegularFile,
                    identity: ObjectIdentity {
                        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
                        inode: stat.st_ino,
                    },
                    status_flags: u32::try_from(flags).expect("status flags should fit"),
                },
                descriptor,
            )
        }

        fn file_registry() -> GrantRegistry {
            let session = SessionId::from_bytes([31; 32]);
            let batch = BatchId::from_bytes([32; 16]);
            let mut staged = StagedGrantBatch::new(session);
            staged
                .accept(received(
                    session,
                    batch,
                    0,
                    GrantRecord::Begin {
                        grant_count: 8,
                        record_count: 10,
                        bookmark_bytes: 0,
                    },
                    None,
                ))
                .expect("begin should stage");
            for (sequence, (id, role, path)) in [
                (
                    "kernel",
                    ResourceRole::KernelImage,
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
                ),
                (
                    "initrd",
                    ResourceRole::InitrdImage,
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api_server.rs"),
                ),
                (
                    "metadata",
                    ResourceRole::StartupMetadata,
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
                ),
                (
                    "drive-ro",
                    ResourceRole::DriveBacking,
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/contained_session.rs"),
                ),
                (
                    "drive-rw",
                    ResourceRole::DriveBacking,
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/vmm.rs"),
                ),
                (
                    "logger",
                    ResourceRole::LoggerSink,
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/grant_integration_probe.rs"),
                ),
                (
                    "metrics",
                    ResourceRole::MetricsSink,
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/periodic_metrics.rs"),
                ),
                (
                    "serial",
                    ResourceRole::SerialSink,
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/test_support.rs"),
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let access = match id {
                    "drive-rw" => GrantAccess::ReadWrite,
                    "logger" | "metrics" | "serial" => GrantAccess::WriteOnly,
                    _ => GrantAccess::ReadOnly,
                };
                let (record, descriptor) = file_record(id, role, access, &path);
                staged
                    .accept(received(
                        session,
                        batch,
                        u64::try_from(sequence + 1).expect("sequence should fit"),
                        record,
                        Some(descriptor),
                    ))
                    .expect("descriptor should stage");
            }
            staged
                .accept(received(
                    session,
                    batch,
                    9,
                    GrantRecord::Commit {
                        grant_count: 8,
                        record_count: 10,
                        bookmark_bytes: 0,
                    },
                    None,
                ))
                .expect("commit should validate")
                .expect("commit should return registry")
                .registry
        }

        #[test]
        fn file_authority_is_send_sync_and_claims_fail_closed_atomically() {
            fn assert_send_sync<T: Send + Sync>() {}
            assert_send_sync::<GrantAuthority>();

            let mut registry = file_registry();
            let authority = GrantAuthority::new(registry.take_file_registry());
            assert!(registry.is_empty());
            let wrong_pair = authority.claim_boot_files(
                Path::new("bangbang-grant:kernel"),
                Some(Path::new("bangbang-grant:metadata")),
            );
            assert_eq!(
                wrong_pair.expect_err("wrong role should fail"),
                GrantClaimError
            );
            let duplicate_pair = authority.claim_boot_files(
                Path::new("bangbang-grant:kernel"),
                Some(Path::new("bangbang-grant:kernel")),
            );
            assert_eq!(
                duplicate_pair.expect_err("duplicate IDs should fail"),
                GrantClaimError
            );

            let mut kernel = authority
                .claim_read_only_file(
                    Path::new("bangbang-grant:kernel"),
                    ResourceRole::KernelImage,
                )
                .expect("kernel claim should validate")
                .expect("kernel reference should claim a file");
            let mut kernel_contents = String::new();
            kernel
                .read_to_string(&mut kernel_contents)
                .expect("claimed kernel fixture should read");
            assert!(kernel_contents.contains("name = \"bangbang\""));

            assert!(
                authority
                    .claim_read_only_file(
                        Path::new("bangbang-grant:kernel"),
                        ResourceRole::KernelImage,
                    )
                    .is_err()
            );
            assert!(
                authority
                    .claim_read_only_file(
                        Path::new("bangbang-grant:"),
                        ResourceRole::StartupMetadata,
                    )
                    .is_err()
            );
            assert!(
                authority
                    .claim_read_only_file(
                        Path::new("ordinary-path"),
                        ResourceRole::StartupMetadata,
                    )
                    .expect("ordinary path should not claim")
                    .is_none()
            );
            assert!(
                authority
                    .claim_file(
                        Path::new("bangbang-grant:drive-rw"),
                        ResourceRole::DriveBacking,
                        GrantAccess::ReadOnly,
                    )
                    .is_err()
            );
            assert!(
                authority
                    .claim_file(
                        Path::new("bangbang-grant:drive-rw"),
                        ResourceRole::DriveBacking,
                        GrantAccess::ReadWrite,
                    )
                    .expect("exact read-write drive claim should validate")
                    .is_some()
            );
            assert!(
                authority
                    .claim_file(
                        Path::new("bangbang-grant:logger"),
                        ResourceRole::LoggerSink,
                        GrantAccess::ReadOnly,
                    )
                    .is_err()
            );
            assert!(
                authority
                    .claim_file(
                        Path::new("bangbang-grant:logger"),
                        ResourceRole::MetricsSink,
                        GrantAccess::WriteOnly,
                    )
                    .is_err()
            );
            assert!(
                authority
                    .claim_file(
                        Path::new("bangbang-grant:logger"),
                        ResourceRole::LoggerSink,
                        GrantAccess::WriteOnly,
                    )
                    .expect("output mismatch should preserve exact grant")
                    .is_some()
            );
            assert!(
                authority
                    .claim_file(
                        Path::new("bangbang-grant:metrics"),
                        ResourceRole::MetricsSink,
                        GrantAccess::WriteOnly,
                    )
                    .expect("metrics output should claim")
                    .is_some()
            );
            assert!(
                authority
                    .claim_file(
                        Path::new("bangbang-grant:serial"),
                        ResourceRole::SerialSink,
                        GrantAccess::WriteOnly,
                    )
                    .expect("serial output should claim")
                    .is_some()
            );
            assert!(
                authority
                    .claim_file(
                        Path::new("bangbang-grant:drive-ro"),
                        ResourceRole::PmemBacking,
                        GrantAccess::ReadOnly,
                    )
                    .is_err()
            );
            assert!(
                authority
                    .claim_file(
                        Path::new("bangbang-grant:drive-ro"),
                        ResourceRole::DriveBacking,
                        GrantAccess::ReadOnly,
                    )
                    .expect("wrong-role failure should preserve drive grant")
                    .is_some()
            );

            let mut mixed_registry = file_registry();
            let mixed_authority = GrantAuthority::new(mixed_registry.take_file_registry());
            let kernel_only = mixed_authority
                .claim_boot_files(
                    Path::new("bangbang-grant:kernel"),
                    Some(Path::new("ordinary-initrd")),
                )
                .expect("provided kernel with ordinary initrd should claim");
            assert!(!kernel_only.is_empty());
            assert!(
                mixed_authority
                    .claim_read_only_file(
                        Path::new("bangbang-grant:initrd"),
                        ResourceRole::InitrdImage,
                    )
                    .expect("unclaimed initrd should remain available")
                    .is_some()
            );

            let mut reverse_mixed_registry = file_registry();
            let reverse_mixed_authority =
                GrantAuthority::new(reverse_mixed_registry.take_file_registry());
            let initrd_only = reverse_mixed_authority
                .claim_boot_files(
                    Path::new("ordinary-kernel"),
                    Some(Path::new("bangbang-grant:initrd")),
                )
                .expect("ordinary kernel with provided initrd should claim");
            assert!(!initrd_only.is_empty());
            assert!(
                reverse_mixed_authority
                    .claim_read_only_file(
                        Path::new("bangbang-grant:kernel"),
                        ResourceRole::KernelImage,
                    )
                    .expect("unclaimed kernel should remain available")
                    .is_some()
            );

            authority.invalidate();
            assert!(
                authority
                    .claim_read_only_file(
                        Path::new("bangbang-grant:metadata"),
                        ResourceRole::StartupMetadata,
                    )
                    .is_err()
            );
            assert!(
                authority
                    .claim_read_only_file(
                        Path::new("ordinary-path"),
                        ResourceRole::StartupMetadata,
                    )
                    .expect("ordinary path remains outside grant authority")
                    .is_none()
            );
        }

        #[test]
        fn file_authority_invalidation_serializes_with_a_claim() {
            let mut registry = file_registry();
            let authority = GrantAuthority::new(registry.take_file_registry());
            let barrier = Arc::new(Barrier::new(3));

            let claiming_authority = authority.clone();
            let claiming_barrier = Arc::clone(&barrier);
            let claim = thread::spawn(move || {
                claiming_barrier.wait();
                claiming_authority.claim_read_only_file(
                    Path::new("bangbang-grant:kernel"),
                    ResourceRole::KernelImage,
                )
            });
            let invalidating_authority = authority.clone();
            let invalidating_barrier = Arc::clone(&barrier);
            let invalidate = thread::spawn(move || {
                invalidating_barrier.wait();
                invalidating_authority.invalidate();
            });
            barrier.wait();

            match claim.join().expect("claim thread should join") {
                Ok(Some(_)) | Err(GrantClaimError) => {}
                Ok(None) => panic!("an explicit reference must not become an ordinary path"),
            }
            invalidate.join().expect("invalidation thread should join");
            assert!(
                authority
                    .claim_read_only_file(
                        Path::new("bangbang-grant:metadata"),
                        ResourceRole::StartupMetadata,
                    )
                    .is_err()
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::path::Path;

    use bangbang_runtime::boot::BootSourceFiles;
    use bangbang_session::{GrantAccess, ResourceRole};

    use super::{
        ContainedSessionError, GrantClaimError, Readiness, TerminalCategory, UnixStream,
        grant_reference_id, socket_directory_reference,
    };

    #[derive(Debug, Clone)]
    pub(crate) struct GrantAuthority;

    #[derive(Debug, Clone)]
    pub(crate) struct DirectoryGrantAuthority;

    #[derive(Debug)]
    pub(crate) struct ClaimedSocketDirectory;

    impl GrantAuthority {
        pub(crate) fn claim_read_only_file(
            &self,
            reference: &Path,
            role: ResourceRole,
        ) -> Result<Option<std::fs::File>, GrantClaimError> {
            self.claim_file(reference, role, GrantAccess::ReadOnly)
        }

        pub(crate) fn claim_file(
            &self,
            reference: &Path,
            _role: ResourceRole,
            _access: GrantAccess,
        ) -> Result<Option<std::fs::File>, GrantClaimError> {
            match grant_reference_id(reference)? {
                Some(_) => Err(GrantClaimError),
                None => Ok(None),
            }
        }

        pub(crate) fn claim_boot_files(
            &self,
            kernel_reference: &Path,
            initrd_reference: Option<&Path>,
        ) -> Result<BootSourceFiles, GrantClaimError> {
            let kernel = grant_reference_id(kernel_reference)?;
            let initrd = initrd_reference
                .map(grant_reference_id)
                .transpose()?
                .flatten();
            if kernel.is_some() || initrd.is_some() {
                Err(GrantClaimError)
            } else {
                Ok(BootSourceFiles::default())
            }
        }
    }

    impl DirectoryGrantAuthority {
        pub(crate) fn claim_socket_directory(
            &self,
            reference: &Path,
            _role: ResourceRole,
        ) -> Result<Option<ClaimedSocketDirectory>, GrantClaimError> {
            match socket_directory_reference(reference)? {
                Some(_) => Err(GrantClaimError),
                None => Ok(None),
            }
        }
    }

    #[derive(Debug)]
    pub(crate) struct ContainedSession;

    impl ContainedSession {
        pub(crate) fn bootstrap() -> Result<Option<Self>, ContainedSessionError> {
            Ok(None)
        }

        pub(crate) fn take_wakeup_pair(
            &mut self,
        ) -> Result<(UnixStream, UnixStream), ContainedSessionError> {
            Err(ContainedSessionError)
        }

        pub(crate) fn shutdown_requested(&self) -> Result<bool, ContainedSessionError> {
            Ok(false)
        }

        pub(crate) fn was_cancelled(&self) -> bool {
            false
        }

        pub(crate) fn grant_authority(&self) -> Option<GrantAuthority> {
            None
        }

        pub(crate) fn directory_grant_authority(&self) -> Option<DirectoryGrantAuthority> {
            None
        }

        pub(crate) fn send_ready(
            &mut self,
            _readiness: Readiness,
        ) -> Result<(), ContainedSessionError> {
            Err(ContainedSessionError)
        }

        pub(crate) fn finish(
            &mut self,
            _category: TerminalCategory,
            _exit_code: u8,
        ) -> Result<(), ContainedSessionError> {
            Err(ContainedSessionError)
        }
    }
}

pub(crate) use platform::{
    ClaimedSocketDirectory, ContainedSession, DirectoryGrantAuthority, GrantAuthority,
};
#[cfg(target_os = "macos")]
pub(crate) use platform::{
    SnapshotStagingRecordTracker, SocketBrokerAuthority, SocketBrokerEndpoint,
};
