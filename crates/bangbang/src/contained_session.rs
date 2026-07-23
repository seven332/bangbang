use std::fmt;
use std::os::unix::ffi::OsStrExt;
#[cfg(not(target_os = "macos"))]
use std::os::unix::net::UnixStream;
use std::path::Path;

use bangbang_session::{
    GrantId, SessionId, SnapshotOutputChild, SocketChild, VmnetAuthority, WorkerPolicy,
};
#[cfg(not(target_os = "macos"))]
use bangbang_session::{Readiness, TerminalCategory};

const GRANT_REFERENCE_PREFIX: &str = "bangbang-grant:";

fn started_vmnet_session_authority(
    policy: WorkerPolicy,
    session: Option<SessionId>,
    started: bool,
) -> Result<(SessionId, VmnetAuthority), ContainedSessionError> {
    let session = session.filter(|session| !session.is_pre_session());
    match (started, session) {
        (true, Some(session)) => Ok((session, policy.vmnet_authority())),
        (false, _) | (true, None) => Err(ContainedSessionError),
    }
}

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

pub(crate) fn socket_directory_reference(
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
    use bangbang_session::{SessionId, VmnetAuthority, WorkerPolicy};

    use super::{
        ContainedSessionError, GrantClaimError, grant_reference_id, snapshot_output_reference,
        socket_directory_reference, started_vmnet_session_authority,
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

    #[test]
    fn vmnet_authority_is_published_only_with_each_started_session_identity() {
        let first =
            VmnetAuthority::try_new(true, false, 1, &[]).expect("first authority should validate");
        let second =
            VmnetAuthority::try_new(false, true, 2, &[]).expect("second authority should validate");
        let first_policy =
            WorkerPolicy::new(501, 20, 2048, None, false).with_vmnet_authority(first);
        let second_policy =
            WorkerPolicy::new(501, 20, 2048, None, false).with_vmnet_authority(second);
        let first_session = SessionId::from_bytes([1; 32]);
        let second_session = SessionId::from_bytes([2; 32]);

        assert_eq!(
            started_vmnet_session_authority(first_policy, Some(first_session), false),
            Err(ContainedSessionError),
            "cancelled pre-Proceed bootstrap must not publish policy"
        );
        assert_eq!(
            started_vmnet_session_authority(first_policy, None, true),
            Err(ContainedSessionError),
            "started bootstrap must retain its authenticated identity"
        );
        assert_eq!(
            started_vmnet_session_authority(first_policy, Some(SessionId::pre_session()), true),
            Err(ContainedSessionError),
            "the reserved greeting identity is never a usable owner"
        );
        assert_eq!(
            started_vmnet_session_authority(first_policy, Some(first_session), true),
            Ok((first_session, first))
        );
        assert_eq!(
            started_vmnet_session_authority(second_policy, Some(second_session), true),
            Ok((second_session, second))
        );
        assert_ne!(
            started_vmnet_session_authority(first_policy, Some(first_session), true),
            started_vmnet_session_authority(first_policy, Some(second_session), true),
            "identical policies from independent sessions remain distinct"
        );
        assert_ne!(
            started_vmnet_session_authority(first_policy, Some(first_session), true),
            started_vmnet_session_authority(second_policy, Some(second_session), true),
            "concurrent sessions retain independent values"
        );
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::env;
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io::{self, Read, Write};
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
    use std::os::unix::net::{UnixDatagram, UnixStream};
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    use bangbang_runtime::block::{
        BlockDeviceControl, BlockDeviceControlError, BlockDeviceGeometry, BlockFileBacking,
    };
    use bangbang_runtime::boot::BootSourceFiles;
    use bangbang_runtime::snapshot_artifact::{
        SnapshotArtifactKind, SnapshotArtifactOutput, SnapshotStagingOwnership,
        SnapshotStagingTracker, SnapshotStagingTrackingError,
    };
    use bangbang_session::macos::block_control::{
        BlockControlError, BlockControlMessage, BlockControlOperation, BlockControlTarget,
        receive_block_control_message, send_block_control_message,
    };
    use bangbang_session::macos::grant_registry::{
        CommittedGrantBatch, DirectoryGrantRegistry, FileGrantRegistry, GrantRegistry,
        GrantedDirectory, GrantedFile, StagedGrantBatch,
    };
    use bangbang_session::macos::grant_transport::receive_grant;
    use bangbang_session::macos::runtime::{
        SnapshotStagingKind, SnapshotStagingName, SnapshotStagingOwnershipRecord, WorkerNamespace,
        WorkerSocketNamespace,
    };
    use bangbang_session::macos::vhost_user_broker::{
        VhostUserBrokerError, VhostUserBrokerMessage, receive_vhost_user_broker_message,
        send_vhost_user_broker_message,
    };
    use bangbang_session::macos::{set_cloexec, verify_peer, verify_peer_pid};
    use bangbang_session::{
        BLOCK_CONTROL_BROKER_FD, BlockDeviceGrant, Frame, FrameDecoder, GRANT_FD, GrantAccess,
        GrantId, GrantObjectKind, Message, ObjectIdentity, Readiness, ResourceRole,
        SESSION_ENV_KEY, SESSION_ENV_VALUE, SESSION_FD, SOCKET_BROKER_FD, SessionId,
        SnapshotOutputChild, TerminalCategory, VHOST_USER_BROKER_FD, VmnetAuthority,
        WorkerLifecycle, WorkerPolicy, encode_frame,
    };

    use super::{
        ContainedSessionError, GrantClaimError, SocketChild, grant_reference_id,
        snapshot_output_reference, socket_directory_reference,
    };

    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
    const VHOST_USER_BROKER_TIMEOUT: Duration = Duration::from_secs(2);
    const BLOCK_CONTROL_BROKER_TIMEOUT: Duration = Duration::from_secs(2);
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
        block_control: Option<BlockControlBrokerAuthority>,
    }

    /// One exact file grant reserved for a failure-atomic runtime transaction.
    pub(crate) struct PreparedFileGrantClaim {
        registry: Arc<Mutex<Option<FileGrantRegistry>>>,
        id: GrantId,
        original: Option<GrantedFile>,
        duplicate: Option<File>,
    }

    /// One exact drive grant reserved for a failure-atomic runtime transaction.
    pub(crate) struct PreparedDriveBackingClaim {
        registry: Arc<Mutex<Option<FileGrantRegistry>>>,
        block_control: Option<BlockControlBrokerAuthority>,
        id: GrantId,
        original: Option<GrantedFile>,
        duplicate: Option<GrantedFile>,
    }

    impl std::fmt::Debug for PreparedFileGrantClaim {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("PreparedFileGrantClaim")
                .field("authority", &"<redacted>")
                .field("original", &self.original.as_ref().map(|_| "<reserved>"))
                .field("duplicate", &self.duplicate.as_ref().map(|_| "<owned>"))
                .finish()
        }
    }

    impl PreparedFileGrantClaim {
        pub(crate) fn take_file(&mut self) -> Result<File, GrantClaimError> {
            self.duplicate.take().ok_or(GrantClaimError)
        }

        pub(crate) fn commit(mut self) {
            self.original.take();
        }
    }

    impl Drop for PreparedFileGrantClaim {
        fn drop(&mut self) {
            let Some(original) = self.original.take() else {
                return;
            };
            let mut registry = match self.registry.lock() {
                Ok(registry) => registry,
                Err(poisoned) => poisoned.into_inner(),
            };
            let Some(registry) = registry.as_mut() else {
                return;
            };
            let _ = registry.restore_file(self.id.clone(), original);
        }
    }

    impl std::fmt::Debug for PreparedDriveBackingClaim {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("PreparedDriveBackingClaim")
                .field("authority", &"<redacted>")
                .field(
                    "block_control",
                    &self.block_control.as_ref().map(|_| "<redacted>"),
                )
                .field("original", &self.original.as_ref().map(|_| "<reserved>"))
                .field("duplicate", &self.duplicate.as_ref().map(|_| "<owned>"))
                .finish()
        }
    }

    impl PreparedDriveBackingClaim {
        pub(crate) fn take_snapshot_read_only_file(&mut self) -> Result<File, GrantClaimError> {
            let duplicate = self.duplicate.take().ok_or(GrantClaimError)?;
            if duplicate.access() != GrantAccess::ReadOnly
                || duplicate.kind() != GrantObjectKind::RegularFile
                || duplicate.block_device().is_some()
            {
                return Err(GrantClaimError);
            }
            Ok(File::from(duplicate.into_owned_fd()))
        }

        pub(crate) fn take_backing(
            &mut self,
            is_read_only: bool,
        ) -> Result<BlockFileBacking, GrantClaimError> {
            let duplicate = self.duplicate.take().ok_or(GrantClaimError)?;
            let expected_access = if is_read_only {
                GrantAccess::ReadOnly
            } else {
                GrantAccess::ReadWrite
            };
            if duplicate.access() != expected_access {
                return Err(GrantClaimError);
            }
            match (duplicate.kind(), duplicate.block_device()) {
                (GrantObjectKind::RegularFile, None) => {
                    BlockFileBacking::from_file(File::from(duplicate.into_owned_fd()), is_read_only)
                        .map_err(|_| GrantClaimError)
                }
                (GrantObjectKind::BlockDevice, Some(block_device)) => {
                    let target = BlockControlTarget::new(
                        self.id.clone(),
                        duplicate.access(),
                        duplicate.identity(),
                        duplicate.status_flags(),
                        block_device,
                    )
                    .ok_or(GrantClaimError)?;
                    let control = self.block_control.clone().ok_or(GrantClaimError)?;
                    BlockFileBacking::from_file_with_block_device_control(
                        File::from(duplicate.into_owned_fd()),
                        is_read_only,
                        Arc::new(ContainedBlockDeviceControl { control, target }),
                    )
                    .map_err(|_| GrantClaimError)
                }
                _ => Err(GrantClaimError),
            }
        }

        pub(crate) fn commit(mut self) {
            self.original.take();
        }
    }

    impl Drop for PreparedDriveBackingClaim {
        fn drop(&mut self) {
            let Some(original) = self.original.take() else {
                return;
            };
            let mut registry = match self.registry.lock() {
                Ok(registry) => registry,
                Err(poisoned) => poisoned.into_inner(),
            };
            let Some(registry) = registry.as_mut() else {
                return;
            };
            let _ = registry.restore_drive_backing(self.id.clone(), original);
        }
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
        #[cfg(test)]
        fn new(registry: FileGrantRegistry) -> Self {
            Self {
                registry: Arc::new(Mutex::new(Some(registry))),
                block_control: None,
            }
        }

        fn new_with_block_control(
            registry: FileGrantRegistry,
            block_control: BlockControlBrokerAuthority,
        ) -> Self {
            Self {
                registry: Arc::new(Mutex::new(Some(registry))),
                block_control: Some(block_control),
            }
        }

        pub(crate) fn is_active(&self) -> bool {
            self.registry
                .lock()
                .map(|registry| registry.is_some())
                .unwrap_or(false)
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

        pub(crate) fn prepare_file_claim(
            &self,
            reference: &Path,
            role: ResourceRole,
            access: GrantAccess,
        ) -> Result<Option<PreparedFileGrantClaim>, GrantClaimError> {
            let Some(id) = grant_reference_id(reference)? else {
                return Ok(None);
            };
            let mut registry = self.registry.lock().map_err(|_| GrantClaimError)?;
            let registry = registry.as_mut().ok_or(GrantClaimError)?;
            let mut duplicates = registry
                .duplicate_files(&[(id.clone(), role, access)])
                .map_err(|_| GrantClaimError)?;
            let duplicate = duplicates.pop().ok_or(GrantClaimError)?;
            debug_assert!(duplicates.is_empty());
            let original = registry
                .take_file(&id, role, access)
                .map_err(|_| GrantClaimError)?;
            Ok(Some(PreparedFileGrantClaim {
                registry: Arc::clone(&self.registry),
                id,
                original: Some(original),
                duplicate: Some(File::from(duplicate.into_owned_fd())),
            }))
        }

        pub(crate) fn prepare_drive_backing_claim(
            &self,
            reference: &Path,
            access: GrantAccess,
        ) -> Result<Option<PreparedDriveBackingClaim>, GrantClaimError> {
            let Some(id) = grant_reference_id(reference)? else {
                return Ok(None);
            };
            if !matches!(access, GrantAccess::ReadOnly | GrantAccess::ReadWrite) {
                return Err(GrantClaimError);
            }
            let mut registry = self.registry.lock().map_err(|_| GrantClaimError)?;
            let registry = registry.as_mut().ok_or(GrantClaimError)?;
            let duplicate = registry
                .duplicate_drive_backing(&id, access)
                .map_err(|_| GrantClaimError)?;
            let original = registry
                .take_drive_backing(&id, access)
                .map_err(|_| GrantClaimError)?;
            Ok(Some(PreparedDriveBackingClaim {
                registry: Arc::clone(&self.registry),
                block_control: self.block_control.clone(),
                id,
                original: Some(original),
                duplicate: Some(duplicate),
            }))
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
            if let Some(block_control) = &self.block_control {
                block_control.invalidate();
            }
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
        pub(crate) grant_id: GrantId,
        pub(crate) child: SocketChild,
    }

    /// One exact socket-directory grant reserved until launcher activation.
    pub(crate) struct PreparedSocketDirectoryClaim {
        authority: DirectoryGrantAuthority,
        directory: Option<GrantedDirectory>,
        grant_id: GrantId,
        child: SocketChild,
    }

    impl std::fmt::Debug for PreparedSocketDirectoryClaim {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("PreparedSocketDirectoryClaim")
                .field("authority", &"<redacted>")
                .field("directory", &self.directory.as_ref().map(|_| "<reserved>"))
                .field("grant_id", &"<redacted>")
                .field("child", &"<redacted>")
                .finish()
        }
    }

    impl PreparedSocketDirectoryClaim {
        pub(crate) fn directory(&self) -> Result<&GrantedDirectory, GrantClaimError> {
            self.directory.as_ref().ok_or(GrantClaimError)
        }

        pub(crate) fn child(&self) -> &SocketChild {
            &self.child
        }

        pub(crate) fn commit(mut self) -> ClaimedSocketDirectory {
            let Some(directory) = self.directory.take() else {
                abort_vhost_user_claim_invariant();
            };
            ClaimedSocketDirectory {
                directory,
                grant_id: self.grant_id.clone(),
                child: self.child.clone(),
            }
        }
    }

    impl Drop for PreparedSocketDirectoryClaim {
        fn drop(&mut self) {
            let Some(directory) = self.directory.take() else {
                return;
            };
            let Ok(mut registry) = self.authority.registry.try_borrow_mut() else {
                abort_vhost_user_claim_invariant();
            };
            let Some(registry) = registry.as_mut() else {
                abort_vhost_user_claim_invariant();
            };
            if registry
                .restore_scoped_directory(self.grant_id.clone(), directory)
                .is_err()
            {
                abort_vhost_user_claim_invariant();
            }
        }
    }

    /// One durable per-drive lease on a retained vhost-user directory child.
    #[derive(Clone)]
    pub(crate) struct ClaimedVhostUserSocket {
        _directory: Rc<GrantedDirectory>,
        grant_id: GrantId,
        child: SocketChild,
    }

    impl std::fmt::Debug for ClaimedVhostUserSocket {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ClaimedVhostUserSocket")
                .field("directory", &"<retained>")
                .field("grant_id", &"<redacted>")
                .field("child", &"<redacted>")
                .finish()
        }
    }

    impl ClaimedVhostUserSocket {
        pub(crate) fn grant_id(&self) -> &GrantId {
            &self.grant_id
        }

        pub(crate) fn child(&self) -> &SocketChild {
            &self.child
        }
    }

    enum PreparedVhostUserDirectory {
        Reserved(Option<GrantedDirectory>),
        Retained(Rc<GrantedDirectory>),
        Committed,
    }

    fn abort_vhost_user_claim_invariant() -> ! {
        // A prepared claim is committed only after external endpoint
        // publication. An internal ownership violation cannot be reported as
        // recoverable without exposing inconsistent public/private state.
        std::process::abort()
    }

    /// Failure-atomic first adoption or reuse of one retained vhost child.
    pub(crate) struct PreparedVhostUserSocketClaim {
        authority: DirectoryGrantAuthority,
        grant_id: GrantId,
        child: SocketChild,
        directory: PreparedVhostUserDirectory,
    }

    impl std::fmt::Debug for PreparedVhostUserSocketClaim {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let state = match self.directory {
                PreparedVhostUserDirectory::Reserved(_) => "<reserved>",
                PreparedVhostUserDirectory::Retained(_) => "<retained>",
                PreparedVhostUserDirectory::Committed => "<committed>",
            };
            formatter
                .debug_struct("PreparedVhostUserSocketClaim")
                .field("authority", &"<redacted>")
                .field("grant_id", &"<redacted>")
                .field("child", &"<redacted>")
                .field("state", &state)
                .finish()
        }
    }

    impl PreparedVhostUserSocketClaim {
        pub(crate) fn grant_id(&self) -> &GrantId {
            &self.grant_id
        }

        pub(crate) fn child(&self) -> &SocketChild {
            &self.child
        }

        pub(crate) fn commit(mut self) -> ClaimedVhostUserSocket {
            let directory = match &mut self.directory {
                PreparedVhostUserDirectory::Reserved(original) => {
                    let Ok(mut retained) = self.authority.retained_vhost.try_borrow_mut() else {
                        abort_vhost_user_claim_invariant();
                    };
                    if retained.contains_key(&self.grant_id) {
                        abort_vhost_user_claim_invariant();
                    }
                    let Some(original) = original.take() else {
                        abort_vhost_user_claim_invariant();
                    };
                    let directory = Rc::new(original);
                    let previous = retained.insert(self.grant_id.clone(), Rc::clone(&directory));
                    debug_assert!(previous.is_none());
                    directory
                }
                PreparedVhostUserDirectory::Retained(directory) => Rc::clone(directory),
                PreparedVhostUserDirectory::Committed => abort_vhost_user_claim_invariant(),
            };
            self.directory = PreparedVhostUserDirectory::Committed;
            ClaimedVhostUserSocket {
                _directory: directory,
                grant_id: self.grant_id.clone(),
                child: self.child.clone(),
            }
        }
    }

    impl Drop for PreparedVhostUserSocketClaim {
        fn drop(&mut self) {
            let PreparedVhostUserDirectory::Reserved(original) = &mut self.directory else {
                return;
            };
            let Some(original) = original.take() else {
                abort_vhost_user_claim_invariant();
            };
            let Ok(mut registry) = self.authority.registry.try_borrow_mut() else {
                abort_vhost_user_claim_invariant();
            };
            let Some(registry) = registry.as_mut() else {
                abort_vhost_user_claim_invariant();
            };
            if registry
                .restore_scoped_directory(self.grant_id.clone(), original)
                .is_err()
            {
                abort_vhost_user_claim_invariant();
            }
        }
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
        retained_vhost: Rc<RefCell<HashMap<GrantId, Rc<GrantedDirectory>>>>,
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
                retained_vhost: Rc::new(RefCell::new(HashMap::new())),
            }
        }

        pub(crate) fn is_active(&self) -> bool {
            self.registry
                .try_borrow()
                .map(|registry| registry.is_some())
                .unwrap_or(false)
        }

        pub(crate) fn validates_vhost_user_lease(
            &self,
            reference: &Path,
            lease: &ClaimedVhostUserSocket,
        ) -> Result<bool, GrantClaimError> {
            let Some((grant_id, child)) = socket_directory_reference(reference)? else {
                return Ok(false);
            };
            if grant_id != lease.grant_id || child != lease.child {
                return Ok(false);
            }
            let retained = self
                .retained_vhost
                .try_borrow()
                .map_err(|_| GrantClaimError)?;
            Ok(retained
                .get(&grant_id)
                .is_some_and(|directory| Rc::ptr_eq(directory, &lease._directory)))
        }

        pub(crate) fn prepare_vhost_user_socket(
            &self,
            reference: &Path,
        ) -> Result<Option<PreparedVhostUserSocketClaim>, GrantClaimError> {
            let Some((grant_id, child)) = socket_directory_reference(reference)? else {
                return Ok(None);
            };
            let mut retained = self
                .retained_vhost
                .try_borrow_mut()
                .map_err(|_| GrantClaimError)?;
            if let Some(directory) = retained.get(&grant_id) {
                return Ok(Some(PreparedVhostUserSocketClaim {
                    authority: self.clone(),
                    grant_id,
                    child,
                    directory: PreparedVhostUserDirectory::Retained(Rc::clone(directory)),
                }));
            }
            retained.try_reserve(1).map_err(|_| GrantClaimError)?;
            drop(retained);
            let directory = self
                .registry
                .try_borrow_mut()
                .map_err(|_| GrantClaimError)?
                .as_mut()
                .ok_or(GrantClaimError)?
                .take_scoped_directory(&grant_id, ResourceRole::VhostUserSocketDirectory)
                .map_err(|_| GrantClaimError)?;
            Ok(Some(PreparedVhostUserSocketClaim {
                authority: self.clone(),
                grant_id,
                child,
                directory: PreparedVhostUserDirectory::Reserved(Some(directory)),
            }))
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
            Ok(Some(ClaimedSocketDirectory {
                directory,
                grant_id: id,
                child,
            }))
        }

        pub(crate) fn prepare_socket_directory(
            &self,
            reference: &Path,
            role: ResourceRole,
        ) -> Result<Option<PreparedSocketDirectoryClaim>, GrantClaimError> {
            let Some((grant_id, child)) = socket_directory_reference(reference)? else {
                return Ok(None);
            };
            let directory = self
                .registry
                .try_borrow_mut()
                .map_err(|_| GrantClaimError)?
                .as_mut()
                .ok_or(GrantClaimError)?
                .take_scoped_directory(&grant_id, role)
                .map_err(|_| GrantClaimError)?;
            Ok(Some(PreparedSocketDirectoryClaim {
                authority: self.clone(),
                directory: Some(directory),
                grant_id,
                child,
            }))
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
            if let Ok(mut retained) = self.retained_vhost.try_borrow_mut() {
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

    /// One launcher broker endpoint reserved until its activation boundary.
    pub(crate) struct PreparedSocketBrokerEndpoint {
        authority: SocketBrokerAuthority,
        endpoint: Option<SocketBrokerEndpoint>,
    }

    impl std::fmt::Debug for PreparedSocketBrokerEndpoint {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("PreparedSocketBrokerEndpoint")
                .field("authority", &"<redacted>")
                .field("endpoint", &self.endpoint.as_ref().map(|_| "<reserved>"))
                .finish()
        }
    }

    impl PreparedSocketBrokerEndpoint {
        pub(crate) fn endpoint(&self) -> Result<&SocketBrokerEndpoint, GrantClaimError> {
            if !self
                .authority
                .state
                .try_borrow()
                .map_err(|_| GrantClaimError)?
                .active
            {
                return Err(GrantClaimError);
            }
            self.endpoint.as_ref().ok_or(GrantClaimError)
        }

        pub(crate) fn commit(mut self) -> Result<SocketBrokerEndpoint, GrantClaimError> {
            if !self
                .authority
                .state
                .try_borrow()
                .map_err(|_| GrantClaimError)?
                .active
            {
                return Err(GrantClaimError);
            }
            self.endpoint.take().ok_or(GrantClaimError)
        }
    }

    impl Drop for PreparedSocketBrokerEndpoint {
        fn drop(&mut self) {
            let Some(endpoint) = self.endpoint.take() else {
                return;
            };
            let Ok(mut state) = self.authority.state.try_borrow_mut() else {
                abort_vhost_user_claim_invariant();
            };
            if !state.active {
                return;
            }
            if state.endpoint.is_some() {
                abort_vhost_user_claim_invariant();
            }
            state.endpoint = Some(endpoint);
        }
    }

    struct SocketBrokerAuthorityState {
        endpoint: Option<SocketBrokerEndpoint>,
        active: bool,
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
        state: Rc<RefCell<SocketBrokerAuthorityState>>,
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
                state: Rc::new(RefCell::new(SocketBrokerAuthorityState {
                    endpoint: Some(SocketBrokerEndpoint {
                        socket,
                        session,
                        launcher_pid,
                    }),
                    active: true,
                })),
            }
        }

        pub(crate) fn take_endpoint(&self) -> Result<SocketBrokerEndpoint, GrantClaimError> {
            let mut state = self.state.try_borrow_mut().map_err(|_| GrantClaimError)?;
            if !state.active {
                return Err(GrantClaimError);
            }
            state.endpoint.take().ok_or(GrantClaimError)
        }

        pub(crate) fn prepare_endpoint(
            &self,
        ) -> Result<PreparedSocketBrokerEndpoint, GrantClaimError> {
            let mut state = self.state.try_borrow_mut().map_err(|_| GrantClaimError)?;
            if !state.active {
                return Err(GrantClaimError);
            }
            let endpoint = state.endpoint.take().ok_or(GrantClaimError)?;
            drop(state);
            Ok(PreparedSocketBrokerEndpoint {
                authority: self.clone(),
                endpoint: Some(endpoint),
            })
        }

        fn invalidate(&self) {
            if let Ok(mut state) = self.state.try_borrow_mut() {
                state.active = false;
                state.endpoint.take();
            }
        }
    }

    struct BlockControlBrokerState {
        socket: UnixDatagram,
        session: SessionId,
        launcher_pid: libc::pid_t,
        next_sequence: u64,
    }

    /// Shared serial authority for exact launcher-brokered block controls.
    #[derive(Clone)]
    struct BlockControlBrokerAuthority {
        state: Arc<Mutex<Option<BlockControlBrokerState>>>,
        shutdown: Arc<UnixDatagram>,
    }

    impl std::fmt::Debug for BlockControlBrokerAuthority {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("BlockControlBrokerAuthority")
                .field("state", &"<redacted>")
                .field("shutdown", &"<owned>")
                .finish()
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum BlockControlResponse {
        Inspected(BlockDeviceGrant),
        Synchronized,
    }

    impl BlockControlBrokerAuthority {
        fn new(
            socket: UnixDatagram,
            session: SessionId,
            launcher_pid: libc::pid_t,
        ) -> Result<Self, ContainedSessionError> {
            let shutdown = socket.try_clone().map_err(|_| ContainedSessionError)?;
            Ok(Self {
                state: Arc::new(Mutex::new(Some(BlockControlBrokerState {
                    socket,
                    session,
                    launcher_pid,
                    next_sequence: 1,
                }))),
                shutdown: Arc::new(shutdown),
            })
        }

        fn request(
            &self,
            operation: BlockControlOperation,
            target: &BlockControlTarget,
        ) -> Result<BlockControlResponse, BlockDeviceControlError> {
            let mut locked = match self.state.lock() {
                Ok(locked) => locked,
                Err(_) => {
                    let _ = self.shutdown.shutdown(std::net::Shutdown::Both);
                    return Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData));
                }
            };
            let Some(mut state) = locked.take() else {
                return Err(BlockDeviceControlError::new(io::ErrorKind::BrokenPipe));
            };
            let Some(next_sequence) = state.next_sequence.checked_add(1) else {
                return Err(poison_block_control_state(
                    state,
                    &self.shutdown,
                    io::ErrorKind::InvalidData,
                ));
            };
            let Some(deadline) = Instant::now().checked_add(BLOCK_CONTROL_BROKER_TIMEOUT) else {
                return Err(poison_block_control_state(
                    state,
                    &self.shutdown,
                    io::ErrorKind::TimedOut,
                ));
            };
            if verify_peer_pid(state.socket.as_raw_fd(), state.launcher_pid).is_err() {
                return Err(poison_block_control_state(
                    state,
                    &self.shutdown,
                    io::ErrorKind::PermissionDenied,
                ));
            }
            let request = match operation {
                BlockControlOperation::Inspect => BlockControlMessage::Inspect {
                    session: state.session,
                    sequence: state.next_sequence,
                    target: target.clone(),
                },
                BlockControlOperation::SynchronizeCache => BlockControlMessage::SynchronizeCache {
                    session: state.session,
                    sequence: state.next_sequence,
                    target: target.clone(),
                },
            };
            loop {
                let Some(remaining) = broker_remaining(deadline) else {
                    return Err(poison_block_control_state(
                        state,
                        &self.shutdown,
                        io::ErrorKind::TimedOut,
                    ));
                };
                if state.socket.set_write_timeout(Some(remaining)).is_err() {
                    return Err(poison_block_control_state(
                        state,
                        &self.shutdown,
                        io::ErrorKind::Other,
                    ));
                }
                match send_block_control_message(&state.socket, &request) {
                    Ok(()) => break,
                    Err(BlockControlError::Io(io::ErrorKind::Interrupted)) => continue,
                    Err(BlockControlError::Io(
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut,
                    )) => {
                        return Err(poison_block_control_state(
                            state,
                            &self.shutdown,
                            io::ErrorKind::TimedOut,
                        ));
                    }
                    Err(BlockControlError::Io(kind)) => {
                        return Err(poison_block_control_state(state, &self.shutdown, kind));
                    }
                    Err(BlockControlError::Invalid) => {
                        return Err(poison_block_control_state(
                            state,
                            &self.shutdown,
                            io::ErrorKind::InvalidData,
                        ));
                    }
                }
            }

            let response = loop {
                let Some(remaining) = broker_remaining(deadline) else {
                    return Err(poison_block_control_state(
                        state,
                        &self.shutdown,
                        io::ErrorKind::TimedOut,
                    ));
                };
                if state.socket.set_read_timeout(Some(remaining)).is_err() {
                    return Err(poison_block_control_state(
                        state,
                        &self.shutdown,
                        io::ErrorKind::Other,
                    ));
                }
                match receive_block_control_message(&state.socket) {
                    Ok(response) => break response,
                    Err(BlockControlError::Io(io::ErrorKind::Interrupted)) => continue,
                    Err(BlockControlError::Io(
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut,
                    )) => {
                        return Err(poison_block_control_state(
                            state,
                            &self.shutdown,
                            io::ErrorKind::TimedOut,
                        ));
                    }
                    Err(BlockControlError::Io(kind)) => {
                        return Err(poison_block_control_state(state, &self.shutdown, kind));
                    }
                    Err(BlockControlError::Invalid) => {
                        return Err(poison_block_control_state(
                            state,
                            &self.shutdown,
                            io::ErrorKind::InvalidData,
                        ));
                    }
                }
            };
            if verify_peer_pid(state.socket.as_raw_fd(), state.launcher_pid).is_err()
                || response.session() != state.session
                || response.sequence() != state.next_sequence
                || response.target() != target
                || response.operation() != operation
            {
                return Err(poison_block_control_state(
                    state,
                    &self.shutdown,
                    io::ErrorKind::InvalidData,
                ));
            }
            let result = match (operation, response) {
                (
                    BlockControlOperation::Inspect,
                    BlockControlMessage::Inspected { observed, .. },
                ) => Ok(BlockControlResponse::Inspected(observed)),
                (
                    BlockControlOperation::SynchronizeCache,
                    BlockControlMessage::Synchronized { .. },
                ) => Ok(BlockControlResponse::Synchronized),
                (_, BlockControlMessage::Failed { kind, .. }) => {
                    Err(BlockDeviceControlError::new(kind))
                }
                _ => {
                    return Err(poison_block_control_state(
                        state,
                        &self.shutdown,
                        io::ErrorKind::InvalidData,
                    ));
                }
            };
            state.next_sequence = next_sequence;
            *locked = Some(state);
            result
        }

        fn invalidate(&self) {
            // This independently owned handle is intentionally used before the
            // request mutex so a blocked exchange wakes immediately.
            let _ = self.shutdown.shutdown(std::net::Shutdown::Both);
            let mut locked = match self.state.lock() {
                Ok(locked) => locked,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(state) = locked.take() {
                let _ = state.socket.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    fn poison_block_control_state(
        state: BlockControlBrokerState,
        shutdown: &UnixDatagram,
        kind: io::ErrorKind,
    ) -> BlockDeviceControlError {
        let _ = shutdown.shutdown(std::net::Shutdown::Both);
        let _ = state.socket.shutdown(std::net::Shutdown::Both);
        BlockDeviceControlError::new(kind)
    }

    struct ContainedBlockDeviceControl {
        control: BlockControlBrokerAuthority,
        target: BlockControlTarget,
    }

    impl std::fmt::Debug for ContainedBlockDeviceControl {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("ContainedBlockDeviceControl")
                .field("control", &"<redacted>")
                .field("target", &"<redacted>")
                .finish()
        }
    }

    impl BlockDeviceControl for ContainedBlockDeviceControl {
        fn inspect(&self, file: &File) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
            validate_contained_block_descriptor(file, &self.target)?;
            let BlockControlResponse::Inspected(observed) = self
                .control
                .request(BlockControlOperation::Inspect, &self.target)?
            else {
                return Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData));
            };
            checked_observed_geometry(&self.target, observed)
        }

        fn synchronize_cache(&self, file: &File) -> Result<(), BlockDeviceControlError> {
            validate_contained_block_descriptor(file, &self.target)?;
            match self
                .control
                .request(BlockControlOperation::SynchronizeCache, &self.target)?
            {
                BlockControlResponse::Synchronized => Ok(()),
                BlockControlResponse::Inspected(_) => {
                    Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData))
                }
            }
        }
    }

    fn checked_observed_geometry(
        target: &BlockControlTarget,
        observed: BlockDeviceGrant,
    ) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
        let geometry =
            BlockDeviceGeometry::new(observed.logical_block_size(), observed.block_count())
                .ok_or(BlockDeviceControlError::new(io::ErrorKind::InvalidData))?;
        if geometry.len() != observed.capacity() || observed != target.block_device() {
            return Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData));
        }
        Ok(geometry)
    }

    fn validate_contained_block_descriptor(
        file: &File,
        target: &BlockControlTarget,
    ) -> Result<(), BlockDeviceControlError> {
        // SAFETY: F_GETFD and F_GETFL inspect the live transferred descriptor.
        let descriptor_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
        // SAFETY: F_GETFL inspects status flags on the same descriptor.
        let status_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        if descriptor_flags < 0 || status_flags < 0 {
            return Err(BlockDeviceControlError::new(
                io::Error::last_os_error().kind(),
            ));
        }
        if descriptor_flags & libc::FD_CLOEXEC == 0 {
            return Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData));
        }
        if bangbang_session::macos::normalized_block_status_flags(status_flags)
            != Some(target.status_flags())
        {
            return Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData));
        }
        if !block_access_matches(status_flags, target.access()) {
            return Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData));
        }
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: stat is writable and the transferred descriptor remains live.
        if unsafe { libc::fstat(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(BlockDeviceControlError::new(
                io::Error::last_os_error().kind(),
            ));
        }
        // SAFETY: Successful fstat initialized the complete structure.
        let stat = unsafe { stat.assume_init() };
        let identity = ObjectIdentity {
            device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
            inode: stat.st_ino,
        };
        if stat.st_mode & libc::S_IFMT != libc::S_IFBLK {
            return Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData));
        }
        if identity != target.identity() {
            return Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData));
        }
        if u64::from(u32::from_ne_bytes(stat.st_rdev.to_ne_bytes()))
            != target.block_device().target_device()
        {
            return Err(BlockDeviceControlError::new(io::ErrorKind::InvalidData));
        }
        Ok(())
    }

    fn block_access_matches(flags: libc::c_int, access: GrantAccess) -> bool {
        match access {
            GrantAccess::ReadOnly => flags & libc::O_ACCMODE == libc::O_RDONLY,
            GrantAccess::ReadWrite => flags & libc::O_ACCMODE == libc::O_RDWR,
            GrantAccess::WriteOnly | GrantAccess::CreateChildren | GrantAccess::ConnectChildren => {
                false
            }
        }
    }

    /// Fixed redacted contained vhost-user broker failure.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum VhostUserBrokerConnectError {
        /// The launcher reached the exact grant but could not connect it.
        Endpoint,
        /// The authenticated private broker violated its closed contract.
        Protocol,
    }

    impl std::fmt::Display for VhostUserBrokerConnectError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Endpoint => {
                    formatter.write_str("contained vhost-user socket connection failed")
                }
                Self::Protocol => formatter.write_str("contained vhost-user broker failed"),
            }
        }
    }

    impl std::error::Error for VhostUserBrokerConnectError {}

    struct VhostUserBrokerState {
        socket: UnixDatagram,
        session: SessionId,
        launcher_pid: libc::pid_t,
        next_sequence: u64,
    }

    /// Shared serial authority for exact launcher-brokered vhost connections.
    #[derive(Clone)]
    pub(crate) struct VhostUserBrokerAuthority {
        state: Arc<Mutex<Option<VhostUserBrokerState>>>,
    }

    impl std::fmt::Debug for VhostUserBrokerAuthority {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .debug_struct("VhostUserBrokerAuthority")
                .field("state", &"<redacted>")
                .finish()
        }
    }

    impl VhostUserBrokerAuthority {
        fn new(socket: UnixDatagram, session: SessionId, launcher_pid: libc::pid_t) -> Self {
            Self {
                state: Arc::new(Mutex::new(Some(VhostUserBrokerState {
                    socket,
                    session,
                    launcher_pid,
                    next_sequence: 1,
                }))),
            }
        }

        #[cfg(test)]
        pub(crate) fn for_test(
            socket: UnixDatagram,
            session: SessionId,
            launcher_pid: libc::pid_t,
        ) -> Self {
            Self::new(socket, session, launcher_pid)
        }

        pub(crate) fn preflight(&self) -> Result<(), VhostUserBrokerConnectError> {
            let mut locked = self
                .state
                .lock()
                .map_err(|_| VhostUserBrokerConnectError::Protocol)?;
            if locked.as_ref().is_some_and(|state| {
                verify_peer_pid(state.socket.as_raw_fd(), state.launcher_pid).is_ok()
            }) {
                return Ok(());
            }
            if let Some(state) = locked.take() {
                let _ = state.socket.shutdown(std::net::Shutdown::Both);
            }
            Err(VhostUserBrokerConnectError::Protocol)
        }

        pub(crate) fn connect(
            &self,
            grant_id: &GrantId,
            child: &SocketChild,
        ) -> Result<UnixStream, VhostUserBrokerConnectError> {
            let mut locked = self
                .state
                .lock()
                .map_err(|_| VhostUserBrokerConnectError::Protocol)?;
            let Some(mut state) = locked.take() else {
                return Err(VhostUserBrokerConnectError::Protocol);
            };
            let Some(deadline) = Instant::now().checked_add(VHOST_USER_BROKER_TIMEOUT) else {
                return Err(poison_vhost_user_broker_state(state));
            };
            if verify_peer_pid(state.socket.as_raw_fd(), state.launcher_pid).is_err() {
                return Err(poison_vhost_user_broker_state(state));
            }
            let request = VhostUserBrokerMessage::Connect {
                session: state.session,
                sequence: state.next_sequence,
                grant_id: grant_id.clone(),
                child: child.clone(),
            };
            loop {
                let Some(remaining) = broker_remaining(deadline) else {
                    return Err(poison_vhost_user_broker_state(state));
                };
                if state.socket.set_write_timeout(Some(remaining)).is_err() {
                    return Err(poison_vhost_user_broker_state(state));
                }
                match send_vhost_user_broker_message(&state.socket, &request, None) {
                    Ok(()) => break,
                    Err(VhostUserBrokerError::Io(io::ErrorKind::Interrupted)) => continue,
                    Err(_) => return Err(poison_vhost_user_broker_state(state)),
                }
            }

            let received = loop {
                let Some(remaining) = broker_remaining(deadline) else {
                    return Err(poison_vhost_user_broker_state(state));
                };
                if state.socket.set_read_timeout(Some(remaining)).is_err() {
                    return Err(poison_vhost_user_broker_state(state));
                }
                match receive_vhost_user_broker_message(&state.socket) {
                    Ok(received) => break received,
                    Err(VhostUserBrokerError::Io(io::ErrorKind::Interrupted)) => continue,
                    Err(_) => return Err(poison_vhost_user_broker_state(state)),
                }
            };
            if verify_peer_pid(state.socket.as_raw_fd(), state.launcher_pid).is_err()
                || received.message.session() != state.session
                || received.message.sequence() != state.next_sequence
                || received.message.grant_id() != grant_id
                || received.message.child() != child
            {
                return Err(poison_vhost_user_broker_state(state));
            }
            let Some(next_sequence) = state.next_sequence.checked_add(1) else {
                return Err(poison_vhost_user_broker_state(state));
            };
            match (received.message, received.descriptor) {
                (VhostUserBrokerMessage::Connected { .. }, Some(descriptor)) => {
                    if validate_vhost_user_stream(descriptor.as_raw_fd()).is_err() {
                        return Err(poison_vhost_user_broker_state(state));
                    }
                    state.next_sequence = next_sequence;
                    *locked = Some(state);
                    Ok(UnixStream::from(descriptor))
                }
                (VhostUserBrokerMessage::Failed { .. }, None) => {
                    state.next_sequence = next_sequence;
                    *locked = Some(state);
                    Err(VhostUserBrokerConnectError::Endpoint)
                }
                _ => Err(poison_vhost_user_broker_state(state)),
            }
        }

        fn invalidate(&self) {
            let mut locked = match self.state.lock() {
                Ok(locked) => locked,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let Some(state) = locked.take() {
                let _ = state.socket.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    fn poison_vhost_user_broker_state(state: VhostUserBrokerState) -> VhostUserBrokerConnectError {
        let _ = state.socket.shutdown(std::net::Shutdown::Both);
        VhostUserBrokerConnectError::Protocol
    }

    fn broker_remaining(deadline: Instant) -> Option<Duration> {
        deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
    }

    fn validate_vhost_user_stream(descriptor: RawFd) -> Result<(), ()> {
        // SAFETY: These fcntl calls inspect one live received descriptor.
        let descriptor_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
        // SAFETY: F_GETFL inspects status flags on the same live descriptor.
        let status_flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
        if descriptor_flags < 0
            || status_flags < 0
            || descriptor_flags & libc::FD_CLOEXEC == 0
            || status_flags & libc::O_NONBLOCK == 0
            || socket_int_option(descriptor, libc::SO_TYPE)? != libc::SOCK_STREAM
            || socket_int_option(descriptor, libc::SO_ERROR)? != 0
        {
            return Err(());
        }
        let mut address = MaybeUninit::<libc::sockaddr_un>::zeroed();
        let mut length =
            libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_un>()).map_err(|_| ())?;
        // SAFETY: Address storage and length are writable for the live stream.
        if unsafe { libc::getpeername(descriptor, address.as_mut_ptr().cast(), &raw mut length) }
            != 0
        {
            return Err(());
        }
        // SAFETY: Successful getpeername initialized the returned address prefix.
        let address = unsafe { address.assume_init() };
        if address.sun_family != libc::AF_UNIX as libc::sa_family_t {
            return Err(());
        }
        Ok(())
    }

    fn socket_int_option(descriptor: RawFd, option: libc::c_int) -> Result<i32, ()> {
        let mut value = 0_i32;
        let mut length = libc::socklen_t::try_from(std::mem::size_of::<i32>()).map_err(|_| ())?;
        // SAFETY: Value and length are writable for this live socket descriptor.
        if unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                option,
                (&raw mut value).cast(),
                &raw mut length,
            )
        } != 0
            || usize::try_from(length).ok() != Some(std::mem::size_of::<i32>())
        {
            return Err(());
        }
        Ok(value)
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
        vhost_user_broker: VhostUserBrokerAuthority,
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
            set_cloexec(VHOST_USER_BROKER_FD).map_err(|_| ContainedSessionError)?;
            set_cloexec(BLOCK_CONTROL_BROKER_FD).map_err(|_| ContainedSessionError)?;
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
            // SAFETY: The private bootstrap contract transfers fixed vhost broker fd 6 once.
            let vhost_broker_owned = unsafe { OwnedFd::from_raw_fd(VHOST_USER_BROKER_FD) };
            let vhost_broker_socket = UnixDatagram::from(vhost_broker_owned);
            // SAFETY: The private bootstrap contract transfers fixed block-control fd 7 once.
            let block_control_owned = unsafe { OwnedFd::from_raw_fd(BLOCK_CONTROL_BROKER_FD) };
            let block_control_socket = UnixDatagram::from(block_control_owned);
            stream
                .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
                .map_err(|_| ContainedSessionError)?;
            // SAFETY: `getppid` has no pointer or ownership contract.
            let parent = unsafe { libc::getppid() };
            verify_peer(stream.as_raw_fd(), parent).map_err(|_| ContainedSessionError)?;
            verify_peer_pid(grant_socket.as_raw_fd(), parent).map_err(|_| ContainedSessionError)?;
            verify_peer_pid(broker_socket.as_raw_fd(), parent)
                .map_err(|_| ContainedSessionError)?;
            verify_peer_pid(vhost_broker_socket.as_raw_fd(), parent)
                .map_err(|_| ContainedSessionError)?;
            verify_peer_pid(block_control_socket.as_raw_fd(), parent)
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
            let block_control =
                BlockControlBrokerAuthority::new(block_control_socket, session, parent)?;
            let file_grants =
                GrantAuthority::new_with_block_control(grants.take_file_registry(), block_control);
            let directory_grants = DirectoryGrantAuthority::new(grants.take_directory_registry());
            let socket_broker = SocketBrokerAuthority::new(broker_socket, session, parent);
            let vhost_user_broker =
                VhostUserBrokerAuthority::new(vhost_broker_socket, session, parent);
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
                    ReaderRevocationAuthorities {
                        grants: file_grants.clone(),
                        vhost_user_broker: vhost_user_broker.clone(),
                    },
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
                vhost_user_broker,
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

        pub(crate) fn vmnet_session_authority(
            &self,
        ) -> Result<(SessionId, VmnetAuthority), ContainedSessionError> {
            let session = self
                .lifecycle
                .lock()
                .map_err(|_| ContainedSessionError)?
                .session();
            super::started_vmnet_session_authority(self.policy, session, self.started)
        }

        pub(crate) fn directory_grant_authority(&self) -> Option<DirectoryGrantAuthority> {
            self.started.then(|| self.directory_grants.clone())
        }

        pub(crate) fn socket_broker_authority(&self) -> Option<SocketBrokerAuthority> {
            self.started.then(|| self.socket_broker.clone())
        }

        pub(crate) fn vhost_user_broker_authority(&self) -> Option<VhostUserBrokerAuthority> {
            self.started.then(|| self.vhost_user_broker.clone())
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
                || !self.policy.vmnet_authority().is_denied()
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
            self.vhost_user_broker.invalidate();
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

    struct ReaderRevocationAuthorities {
        grants: GrantAuthority,
        vhost_user_broker: VhostUserBrokerAuthority,
    }

    fn spawn_reader(
        mut stream: UnixStream,
        mut decoder: FrameDecoder,
        lifecycle: Arc<Mutex<WorkerLifecycle>>,
        namespace: Arc<Mutex<Option<WorkerNamespace>>>,
        control: Arc<SharedControl>,
        authorities: ReaderRevocationAuthorities,
        mut wakeup: UnixStream,
    ) -> Result<JoinHandle<()>, ContainedSessionError> {
        thread::Builder::new()
            .name("bangbang-session-control".to_string())
            .spawn(move || {
                let state = reader_loop(&mut stream, &mut decoder, &lifecycle);
                if !control.closing.load(Ordering::Acquire) {
                    authorities.grants.invalidate();
                    authorities.vhost_user_broker.invalidate();
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
    pub(crate) use tests::{
        TestDirectory as TestVhostDirectory, empty_grant_authority_for_vhost_test,
        file_grant_authority_for_test, vhost_directory_authority_for_test,
        vsock_directory_authority_for_test,
    };

    #[cfg(test)]
    mod tests {
        use std::fs::{self, OpenOptions};
        use std::io::{self, Read as _};
        use std::mem::MaybeUninit;
        use std::os::fd::{AsRawFd, OwnedFd};
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::net::{UnixDatagram, UnixStream};
        use std::path::{Path, PathBuf};
        use std::rc::Rc;
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::{Arc, Barrier};
        use std::thread;
        use std::time::{Duration, Instant};

        use bangbang_session::macos::block_control::{
            BlockControlError, BlockControlMessage, BlockControlOperation, BlockControlTarget,
            receive_block_control_message, send_block_control_message,
        };
        use bangbang_session::macos::bookmark::create_implicit_bookmark;
        use bangbang_session::macos::grant_registry::{GrantRegistry, StagedGrantBatch};
        use bangbang_session::macos::grant_transport::ReceivedGrant;
        use bangbang_session::macos::vhost_user_broker::{
            VhostUserBrokerMessage, receive_vhost_user_broker_message,
            send_vhost_user_broker_message,
        };
        use bangbang_session::{
            BatchId, BlockDeviceGrant, GrantAccess, GrantFrame, GrantId, GrantObjectKind,
            GrantRecord, ObjectIdentity, ResourceRole, SessionId,
        };

        use super::{
            BlockControlBrokerAuthority, BlockControlResponse, DirectoryGrantAuthority,
            GrantAuthority, GrantClaimError, SocketBrokerAuthority, VhostUserBrokerAuthority,
            VhostUserBrokerConnectError, checked_observed_geometry, exact_resource_limit,
        };

        static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(0);

        pub(crate) struct TestDirectory(PathBuf);

        impl TestDirectory {
            fn new() -> Self {
                loop {
                    let id = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                    let path =
                        PathBuf::from("/tmp").join(format!("bb-cv-{}-{id}", std::process::id()));
                    match fs::create_dir(&path) {
                        Ok(()) => return Self(path),
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => panic!("test directory should create: {error}"),
                    }
                }
            }

            pub(crate) fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for TestDirectory {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.0).expect("test directory should clean up");
            }
        }

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
                GrantAccess::ReadOnly
                | GrantAccess::CreateChildren
                | GrantAccess::ConnectChildren => {
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
                    block_device: None,
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

        fn directory_registry(id: &str, role: ResourceRole) -> (GrantRegistry, TestDirectory) {
            let directory = TestDirectory::new();
            let bookmark = create_implicit_bookmark(directory.path(), true)
                .expect("directory bookmark should create");
            let descriptor = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(directory.path())
                .expect("directory anchor should open");
            let descriptor: OwnedFd = descriptor.into();
            let mut stat = MaybeUninit::<libc::stat>::uninit();
            assert_eq!(
                // SAFETY: stat is writable and the directory descriptor is live.
                unsafe { libc::fstat(descriptor.as_raw_fd(), stat.as_mut_ptr()) },
                0
            );
            // SAFETY: Successful fstat initialized the complete structure.
            let stat = unsafe { stat.assume_init() };
            let identity = ObjectIdentity {
                device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
                inode: stat.st_ino,
            };
            let session = SessionId::from_bytes([41; 32]);
            let batch = BatchId::from_bytes([42; 16]);
            let grant_id = GrantId::parse(id).expect("grant ID should parse");
            let bookmark_bytes = u32::try_from(bookmark.len()).expect("bookmark should fit");
            let access = if role == ResourceRole::VhostUserSocketDirectory {
                GrantAccess::ConnectChildren
            } else {
                GrantAccess::CreateChildren
            };
            let mut staged = StagedGrantBatch::new(session);
            staged
                .accept(received(
                    session,
                    batch,
                    0,
                    GrantRecord::Begin {
                        grant_count: 1,
                        record_count: 4,
                        bookmark_bytes,
                    },
                    None,
                ))
                .expect("begin should stage");
            staged
                .accept(received(
                    session,
                    batch,
                    1,
                    GrantRecord::ScopedDirectory {
                        id: grant_id.clone(),
                        role,
                        access,
                        identity,
                        bookmark_bytes,
                        fragment_count: 1,
                    },
                    Some(descriptor),
                ))
                .expect("directory should stage");
            staged
                .accept(received(
                    session,
                    batch,
                    2,
                    GrantRecord::BookmarkFragment {
                        id: grant_id,
                        offset: 0,
                        bytes: bookmark,
                    },
                    None,
                ))
                .expect("bookmark should stage");
            let registry = staged
                .accept(received(
                    session,
                    batch,
                    3,
                    GrantRecord::Commit {
                        grant_count: 1,
                        record_count: 4,
                        bookmark_bytes,
                    },
                    None,
                ))
                .expect("commit should validate")
                .expect("commit should return registry")
                .registry;
            (registry, directory)
        }

        fn vhost_directory_registry() -> (GrantRegistry, TestDirectory) {
            directory_registry("vhost-directory", ResourceRole::VhostUserSocketDirectory)
        }

        pub(crate) fn empty_grant_authority_for_vhost_test() -> GrantAuthority {
            GrantAuthority::new(Default::default())
        }

        pub(crate) fn file_grant_authority_for_test() -> GrantAuthority {
            let mut registry = file_registry();
            GrantAuthority::new(registry.take_file_registry())
        }

        pub(crate) fn vhost_directory_authority_for_test()
        -> (DirectoryGrantAuthority, TestDirectory) {
            let (mut registry, directory) = vhost_directory_registry();
            (
                DirectoryGrantAuthority::new(registry.take_directory_registry()),
                directory,
            )
        }

        pub(crate) fn vsock_directory_authority_for_test()
        -> (DirectoryGrantAuthority, TestDirectory) {
            let (mut registry, directory) =
                directory_registry("vsock-directory", ResourceRole::VsockSocketDirectory);
            (
                DirectoryGrantAuthority::new(registry.take_directory_registry()),
                directory,
            )
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
                    .is_err()
            );
            let mut drive = authority
                .prepare_drive_backing_claim(
                    Path::new("bangbang-grant:drive-rw"),
                    GrantAccess::ReadWrite,
                )
                .expect("dedicated read-write drive claim should validate")
                .expect("drive reference should reserve a backing");
            drop(
                drive
                    .take_backing(false)
                    .expect("regular drive backing should adopt"),
            );
            drive.commit();
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
                    .is_err()
            );
            let mut drive = authority
                .prepare_drive_backing_claim(
                    Path::new("bangbang-grant:drive-ro"),
                    GrantAccess::ReadOnly,
                )
                .expect("wrong-role failure should preserve drive grant")
                .expect("drive reference should reserve a backing");
            drop(
                drive
                    .take_backing(true)
                    .expect("regular drive backing should adopt"),
            );
            drive.commit();

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
        fn prepared_drive_claim_restores_exact_authority_on_abort_and_consumes_on_commit() {
            let mut registry = file_registry();
            let authority = GrantAuthority::new(registry.take_file_registry());
            let mut snapshot_root = authority
                .prepare_drive_backing_claim(
                    Path::new("bangbang-grant:drive-ro"),
                    GrantAccess::ReadOnly,
                )
                .expect("read-only snapshot root should prepare")
                .expect("explicit snapshot root should reserve a grant");
            drop(
                snapshot_root
                    .take_snapshot_read_only_file()
                    .expect("regular read-only root should remain a dedicated drive claim"),
            );
            drop(snapshot_root);
            assert!(
                authority
                    .prepare_drive_backing_claim(
                        Path::new("bangbang-grant:drive-ro"),
                        GrantAccess::ReadOnly,
                    )
                    .expect("aborted snapshot root should restore authority")
                    .is_some()
            );

            let mut prepared = authority
                .prepare_drive_backing_claim(
                    Path::new("bangbang-grant:drive-rw"),
                    GrantAccess::ReadWrite,
                )
                .expect("exact runtime grant should prepare")
                .expect("explicit reference should reserve a grant");
            let duplicate = prepared
                .take_backing(false)
                .expect("prepared claim should expose one backing");
            drop(duplicate);
            drop(prepared);
            let restored = authority
                .prepare_drive_backing_claim(
                    Path::new("bangbang-grant:drive-rw"),
                    GrantAccess::ReadWrite,
                )
                .expect("aborted claim should restore authority")
                .expect("aborted claim should reserve again");
            drop(restored);

            let mut commit_registry = file_registry();
            let commit_authority = GrantAuthority::new(commit_registry.take_file_registry());
            let mut committed = commit_authority
                .prepare_drive_backing_claim(
                    Path::new("bangbang-grant:drive-rw"),
                    GrantAccess::ReadWrite,
                )
                .expect("exact runtime grant should prepare")
                .expect("explicit reference should reserve a grant");
            drop(
                committed
                    .take_backing(false)
                    .expect("committed claim should expose one backing"),
            );
            committed.commit();
            assert!(
                commit_authority
                    .prepare_drive_backing_claim(
                        Path::new("bangbang-grant:drive-rw"),
                        GrantAccess::ReadWrite,
                    )
                    .is_err()
            );
        }

        #[test]
        fn vhost_directory_first_adoption_rolls_back_then_retains_multiple_child_leases() {
            let (mut registry, _directory) = vhost_directory_registry();
            let authority = DirectoryGrantAuthority::new(registry.take_directory_registry());
            let first = authority
                .prepare_vhost_user_socket(Path::new("bangbang-grant:vhost-directory/first.sock"))
                .expect("fresh directory should prepare")
                .expect("explicit reference should reserve authority");
            assert_eq!(first.grant_id().as_bytes(), b"vhost-directory");
            assert_eq!(first.child().as_bytes(), b"first.sock");
            drop(first);

            let first = authority
                .prepare_vhost_user_socket(Path::new("bangbang-grant:vhost-directory/first.sock"))
                .expect("aborted reservation should restore")
                .expect("restored reference should prepare")
                .commit();
            let second = authority
                .prepare_vhost_user_socket(Path::new("bangbang-grant:vhost-directory/second.sock"))
                .expect("retained directory should be reusable")
                .expect("second child should prepare")
                .commit();
            assert!(Rc::ptr_eq(&first._directory, &second._directory));
            assert_eq!(second.child().as_bytes(), b"second.sock");
            let debug = format!("{first:?} {second:?}");
            assert!(!debug.contains("vhost-directory"));
            assert!(!debug.contains("first.sock"));
            assert!(!debug.contains("second.sock"));

            drop(first);
            drop(second);
            assert!(
                authority
                    .prepare_vhost_user_socket(Path::new(
                        "bangbang-grant:vhost-directory/first.sock",
                    ))
                    .expect("DELETE-like lease release should retain the directory")
                    .is_some()
            );
            assert!(
                authority
                    .prepare_vhost_user_socket(Path::new("bangbang-grant:missing/first.sock",))
                    .is_err()
            );
            authority.invalidate();
            assert!(
                authority
                    .prepare_vhost_user_socket(Path::new(
                        "bangbang-grant:vhost-directory/first.sock",
                    ))
                    .is_err()
            );
        }

        #[test]
        fn prepared_socket_directory_claim_rolls_back_and_commit_consumes_exact_authority() {
            let (mut registry, _directory) =
                directory_registry("vsock-directory", ResourceRole::VsockSocketDirectory);
            let authority = DirectoryGrantAuthority::new(registry.take_directory_registry());
            let reference = Path::new("bangbang-grant:vsock-directory/restored.sock");

            assert!(
                authority
                    .prepare_socket_directory(
                        Path::new("/tmp/ordinary.sock"),
                        ResourceRole::VsockSocketDirectory,
                    )
                    .expect("ordinary paths should be recognized without access")
                    .is_none()
            );
            let prepared = authority
                .prepare_socket_directory(reference, ResourceRole::VsockSocketDirectory)
                .expect("exact grant should prepare")
                .expect("explicit reference should reserve authority");
            assert_eq!(prepared.child().as_bytes(), b"restored.sock");
            let diagnostic = format!("{prepared:?}");
            assert!(!diagnostic.contains("vsock-directory"));
            assert!(!diagnostic.contains("restored.sock"));
            assert!(
                authority
                    .prepare_socket_directory(reference, ResourceRole::VsockSocketDirectory)
                    .is_err(),
                "reserved authority must not be claimed twice"
            );
            drop(prepared);

            let committed = authority
                .prepare_socket_directory(reference, ResourceRole::VsockSocketDirectory)
                .expect("aborted reservation should restore authority")
                .expect("restored exact grant should prepare")
                .commit();
            assert_eq!(committed.child.as_bytes(), b"restored.sock");
            assert!(
                authority
                    .prepare_socket_directory(reference, ResourceRole::VsockSocketDirectory)
                    .is_err(),
                "committed authority must remain consumed"
            );
        }

        #[test]
        fn prepared_socket_broker_endpoint_rolls_back_until_commit() {
            let (worker, launcher) = UnixDatagram::pair().expect("broker pair should create");
            let authority = SocketBrokerAuthority::new(
                worker,
                SessionId::from_bytes([91; 32]),
                // SAFETY: This test authenticates the other endpoint in the same process.
                unsafe { libc::getpid() },
            );

            let prepared = authority
                .prepare_endpoint()
                .expect("broker endpoint should reserve");
            assert!(authority.prepare_endpoint().is_err());
            assert!(!format!("{prepared:?}").contains("91"));
            drop(prepared);

            let committed = authority
                .prepare_endpoint()
                .expect("aborted endpoint should be reusable")
                .commit()
                .expect("active broker reservation should commit");
            assert!(authority.prepare_endpoint().is_err());
            drop(committed);
            drop(launcher);

            let (worker, launcher) = UnixDatagram::pair().expect("broker pair should create");
            let authority = SocketBrokerAuthority::new(
                worker,
                SessionId::from_bytes([92; 32]),
                // SAFETY: This test authenticates the other endpoint in the same process.
                unsafe { libc::getpid() },
            );
            let prepared = authority
                .prepare_endpoint()
                .expect("endpoint should reserve before invalidation");
            authority.invalidate();
            assert!(prepared.endpoint().is_err());
            assert!(prepared.commit().is_err());
            assert!(authority.prepare_endpoint().is_err());
            drop(launcher);
        }

        #[test]
        fn vhost_broker_retries_normal_failure_and_returns_exact_stream() {
            let session = SessionId::from_bytes([51; 32]);
            let (worker, launcher) = UnixDatagram::pair().expect("broker pair should open");
            // SAFETY: Both connected datagram peers belong to this test process.
            let pid = unsafe { libc::getpid() };
            let authority = VhostUserBrokerAuthority::new(worker, session, pid);
            let grant_id = GrantId::parse("vhost-directory").expect("grant should parse");
            let child = super::SocketChild::parse("backend.sock").expect("child should parse");
            let launcher_grant = grant_id.clone();
            let launcher_child = child.clone();
            let (release, hold) = std::sync::mpsc::channel();
            let peer = thread::spawn(move || {
                for sequence in 1..=2 {
                    let received = receive_vhost_user_broker_message(&launcher)
                        .expect("request should receive");
                    assert_eq!(
                        received.message,
                        VhostUserBrokerMessage::Connect {
                            session,
                            sequence,
                            grant_id: launcher_grant.clone(),
                            child: launcher_child.clone(),
                        }
                    );
                    assert!(received.descriptor.is_none());
                    if sequence == 1 {
                        send_vhost_user_broker_message(
                            &launcher,
                            &VhostUserBrokerMessage::Failed {
                                session,
                                sequence,
                                grant_id: launcher_grant.clone(),
                                child: launcher_child.clone(),
                                kind: std::io::ErrorKind::ConnectionRefused,
                            },
                            None,
                        )
                        .expect("normal failure should send");
                    } else {
                        let (stream, peer) = UnixStream::pair().expect("stream pair should open");
                        stream
                            .set_nonblocking(true)
                            .expect("brokered stream should be nonblocking");
                        send_vhost_user_broker_message(
                            &launcher,
                            &VhostUserBrokerMessage::Connected {
                                session,
                                sequence,
                                grant_id: launcher_grant.clone(),
                                child: launcher_child.clone(),
                            },
                            Some(stream.as_raw_fd()),
                        )
                        .expect("connected stream should send");
                        hold.recv()
                            .expect("launcher facet should remain live through peer validation");
                        return peer;
                    }
                }
                panic!("second request must return a peer")
            });

            authority
                .preflight()
                .expect("healthy broker preflight should preserve the facet");

            assert_eq!(
                authority
                    .connect(&grant_id, &child)
                    .expect_err("first request should report endpoint failure"),
                VhostUserBrokerConnectError::Endpoint
            );
            let stream = authority
                .connect(&grant_id, &child)
                .expect("second request should return the exact stream");
            // SAFETY: F_GETFD/F_GETFL inspect the live received stream.
            let descriptor_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
            // SAFETY: F_GETFL inspects status flags on the same stream.
            let status_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
            assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
            assert_ne!(status_flags & libc::O_NONBLOCK, 0);
            release
                .send(())
                .expect("launcher facet should release after validation");
            drop(stream);
            drop(peer.join().expect("broker peer should join"));
        }

        fn block_control_target() -> BlockControlTarget {
            BlockControlTarget::new(
                GrantId::parse("block-drive").expect("grant should parse"),
                GrantAccess::ReadWrite,
                ObjectIdentity {
                    device: 71,
                    inode: 72,
                },
                u32::try_from(libc::O_RDWR | libc::O_NONBLOCK).expect("flags should fit"),
                BlockDeviceGrant::new(73, 512, 32).expect("block tuple should validate"),
            )
            .expect("block target should validate")
        }

        #[test]
        fn contained_block_geometry_must_match_the_adopted_grant_exactly() {
            let target = block_control_target();
            let expected = target.block_device();
            let geometry = checked_observed_geometry(&target, expected)
                .expect("exact adopted geometry should validate");
            assert_eq!(geometry.logical_block_size(), expected.logical_block_size());
            assert_eq!(geometry.block_count(), expected.block_count());
            assert_eq!(geometry.len(), expected.capacity());

            let changed = BlockDeviceGrant::new(
                expected.target_device(),
                expected.logical_block_size(),
                expected.block_count() - 1,
            )
            .expect("changed geometry should remain structurally valid");
            assert_eq!(
                checked_observed_geometry(&target, changed)
                    .expect_err("fresh geometry drift must fail closed")
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }

        fn respond_to_block_control(
            socket: &UnixDatagram,
            request: BlockControlMessage,
        ) -> BlockControlOperation {
            let operation = request.operation();
            let response = match request {
                BlockControlMessage::Inspect {
                    session,
                    sequence,
                    target,
                } => BlockControlMessage::Inspected {
                    session,
                    sequence,
                    observed: target.block_device(),
                    target,
                },
                BlockControlMessage::SynchronizeCache {
                    session,
                    sequence,
                    target,
                } => BlockControlMessage::Synchronized {
                    session,
                    sequence,
                    target,
                },
                _ => panic!("worker must send only block-control requests"),
            };
            send_block_control_message(socket, &response).expect("response should send");
            operation
        }

        #[test]
        fn block_control_authority_reuses_failure_and_serializes_concurrent_requests() {
            let session = SessionId::from_bytes([61; 32]);
            let target = block_control_target();
            let (worker, launcher) = UnixDatagram::pair().expect("block broker pair should open");
            // SAFETY: Both connected peers belong to this test process.
            let pid = unsafe { libc::getpid() };
            let authority = BlockControlBrokerAuthority::new(worker, session, pid)
                .expect("block authority should initialize");
            let launcher_target = target.clone();
            let (release_peer, hold_peer) = std::sync::mpsc::channel();
            let peer = thread::spawn(move || {
                let first = receive_block_control_message(&launcher)
                    .expect("initial block request should receive");
                assert_eq!(
                    first,
                    BlockControlMessage::Inspect {
                        session,
                        sequence: 1,
                        target: launcher_target.clone(),
                    }
                );
                send_block_control_message(
                    &launcher,
                    &BlockControlMessage::Failed {
                        session,
                        sequence: 1,
                        target: launcher_target,
                        operation: BlockControlOperation::Inspect,
                        kind: io::ErrorKind::PermissionDenied,
                    },
                )
                .expect("correlated endpoint failure should send");

                let second = receive_block_control_message(&launcher)
                    .expect("one serialized request should receive");
                assert_eq!(second.sequence(), 2);
                launcher
                    .set_read_timeout(Some(Duration::from_millis(50)))
                    .expect("serialization probe timeout should set");
                assert!(matches!(
                    receive_block_control_message(&launcher),
                    Err(BlockControlError::Io(
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ))
                ));
                launcher
                    .set_read_timeout(None)
                    .expect("serialization probe timeout should clear");
                let first_operation = respond_to_block_control(&launcher, second);
                let third = receive_block_control_message(&launcher)
                    .expect("second serialized request should receive");
                assert_eq!(third.sequence(), 3);
                let second_operation = respond_to_block_control(&launcher, third);
                hold_peer
                    .recv()
                    .expect("peer should remain live through response validation");
                (first_operation, second_operation)
            });

            let failure = authority
                .request(BlockControlOperation::Inspect, &target)
                .expect_err("correlated failure should return to the caller");
            assert_eq!(failure.kind(), io::ErrorKind::PermissionDenied);

            let barrier = Arc::new(Barrier::new(3));
            let inspect_authority = authority.clone();
            let inspect_target = target.clone();
            let inspect_barrier = Arc::clone(&barrier);
            let inspect = thread::spawn(move || {
                inspect_barrier.wait();
                inspect_authority.request(BlockControlOperation::Inspect, &inspect_target)
            });
            let sync_authority = authority.clone();
            let sync_target = target.clone();
            let sync_barrier = Arc::clone(&barrier);
            let synchronize = thread::spawn(move || {
                sync_barrier.wait();
                sync_authority.request(BlockControlOperation::SynchronizeCache, &sync_target)
            });
            barrier.wait();

            let inspect_result = inspect.join().expect("inspect caller should join");
            let synchronize_result = synchronize.join().expect("sync caller should join");
            release_peer
                .send(())
                .expect("peer should release after caller validation");
            let operations = peer.join().expect("block broker peer should join");
            assert_ne!(operations.0, operations.1);
            assert_eq!(
                inspect_result,
                Ok(BlockControlResponse::Inspected(target.block_device())),
                "serialized operation order: {operations:?}; sync result: {synchronize_result:?}"
            );
            assert_eq!(synchronize_result, Ok(BlockControlResponse::Synchronized));
        }

        #[test]
        fn block_control_authority_poison_is_permanent_after_ambiguous_response() {
            let session = SessionId::from_bytes([62; 32]);
            let target = block_control_target();
            let (worker, launcher) = UnixDatagram::pair().expect("block broker pair should open");
            // SAFETY: Both connected peers belong to this test process.
            let pid = unsafe { libc::getpid() };
            let authority = BlockControlBrokerAuthority::new(worker, session, pid)
                .expect("block authority should initialize");
            let peer = thread::spawn(move || {
                let request = receive_block_control_message(&launcher)
                    .expect("ambiguous request should receive");
                let target = request.target().clone();
                send_block_control_message(
                    &launcher,
                    &BlockControlMessage::Inspected {
                        session,
                        sequence: request.sequence() + 1,
                        observed: target.block_device(),
                        target,
                    },
                )
                .expect("wrong-sequence response should send");
            });
            assert_eq!(
                authority
                    .request(BlockControlOperation::Inspect, &target)
                    .expect_err("wrong response must poison authority")
                    .kind(),
                io::ErrorKind::InvalidData
            );
            peer.join().expect("ambiguous peer should join");
            assert_eq!(
                authority
                    .request(BlockControlOperation::Inspect, &target)
                    .expect_err("poisoned authority must remain closed")
                    .kind(),
                io::ErrorKind::BrokenPipe
            );
            let debug = format!("{authority:?}");
            assert!(!debug.contains("block-drive"));
            assert!(!debug.contains("6161"));
        }

        #[test]
        fn block_control_authority_timeout_poison_and_shutdown_wakeup_are_bounded() {
            let session = SessionId::from_bytes([63; 32]);
            let target = block_control_target();
            let (worker, launcher) = UnixDatagram::pair().expect("timeout pair should open");
            // SAFETY: Both connected peers belong to this test process.
            let pid = unsafe { libc::getpid() };
            let timed = BlockControlBrokerAuthority::new(worker, session, pid)
                .expect("timeout authority should initialize");
            let started = Instant::now();
            assert_eq!(
                timed
                    .request(BlockControlOperation::Inspect, &target)
                    .expect_err("missing response should time out")
                    .kind(),
                io::ErrorKind::TimedOut
            );
            assert!(started.elapsed() >= Duration::from_secs(1));
            assert!(started.elapsed() < Duration::from_secs(4));
            drop(launcher);
            assert_eq!(
                timed
                    .request(BlockControlOperation::Inspect, &target)
                    .expect_err("timed-out authority must remain poisoned")
                    .kind(),
                io::ErrorKind::BrokenPipe
            );

            let (worker, launcher) = UnixDatagram::pair().expect("shutdown pair should open");
            let authority = BlockControlBrokerAuthority::new(worker, session, pid)
                .expect("shutdown authority should initialize");
            let requesting = authority.clone();
            let request_target = target.clone();
            let requester = thread::spawn(move || {
                requesting.request(BlockControlOperation::Inspect, &request_target)
            });
            receive_block_control_message(&launcher)
                .expect("blocked request should reach the peer");
            let started = Instant::now();
            authority.invalidate();
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "independent shutdown must wake the serialized request"
            );
            assert!(
                requester
                    .join()
                    .expect("blocked requester should join")
                    .is_err()
            );
            assert_eq!(
                authority
                    .request(BlockControlOperation::Inspect, &target)
                    .expect_err("invalidated authority must remain closed")
                    .kind(),
                io::ErrorKind::BrokenPipe
            );
        }

        #[test]
        fn block_control_authority_rejects_wrong_peer_sequence_overflow_and_lock_poison() {
            let session = SessionId::from_bytes([64; 32]);
            let target = block_control_target();
            // SAFETY: `getpid` has no pointer or ownership contract.
            let pid = unsafe { libc::getpid() };

            let (worker, _launcher) = UnixDatagram::pair().expect("peer pair should open");
            let wrong_peer = BlockControlBrokerAuthority::new(
                worker,
                session,
                pid.checked_add(1).expect("test PID should fit"),
            )
            .expect("wrong-peer authority should initialize");
            assert_eq!(
                wrong_peer
                    .request(BlockControlOperation::Inspect, &target)
                    .expect_err("wrong peer must poison before sending")
                    .kind(),
                io::ErrorKind::PermissionDenied
            );

            let (worker, _launcher) = UnixDatagram::pair().expect("overflow pair should open");
            let overflow = BlockControlBrokerAuthority::new(worker, session, pid)
                .expect("overflow authority should initialize");
            overflow
                .state
                .lock()
                .expect("overflow state should lock")
                .as_mut()
                .expect("overflow state should be active")
                .next_sequence = u64::MAX;
            assert_eq!(
                overflow
                    .request(BlockControlOperation::Inspect, &target)
                    .expect_err("wrapped sequence must poison before sending")
                    .kind(),
                io::ErrorKind::InvalidData
            );

            let (worker, _launcher) = UnixDatagram::pair().expect("poison pair should open");
            let poisoned = BlockControlBrokerAuthority::new(worker, session, pid)
                .expect("poison authority should initialize");
            let state = Arc::clone(&poisoned.state);
            assert!(
                thread::spawn(move || {
                    let _held = state.lock().expect("state should lock before poisoning");
                    panic!("intentional block-control lock poison");
                })
                .join()
                .is_err()
            );
            assert_eq!(
                poisoned
                    .request(BlockControlOperation::Inspect, &target)
                    .expect_err("lock poison must close the authority")
                    .kind(),
                io::ErrorKind::InvalidData
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

    #[derive(Debug)]
    pub(crate) struct PreparedFileGrantClaim;

    #[derive(Debug)]
    pub(crate) struct PreparedDriveBackingClaim;

    impl PreparedFileGrantClaim {
        pub(crate) fn take_file(&mut self) -> Result<std::fs::File, GrantClaimError> {
            Err(GrantClaimError)
        }

        pub(crate) fn commit(self) {}
    }

    impl PreparedDriveBackingClaim {
        pub(crate) fn take_backing(
            &mut self,
            _is_read_only: bool,
        ) -> Result<bangbang_runtime::block::BlockFileBacking, GrantClaimError> {
            Err(GrantClaimError)
        }

        pub(crate) fn commit(self) {}
    }

    #[derive(Debug, Clone)]
    pub(crate) struct DirectoryGrantAuthority;

    #[derive(Debug)]
    pub(crate) struct ClaimedSocketDirectory;

    #[derive(Debug)]
    pub(crate) struct PreparedSocketDirectoryClaim;

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

        pub(crate) fn prepare_file_claim(
            &self,
            reference: &Path,
            _role: ResourceRole,
            _access: GrantAccess,
        ) -> Result<Option<PreparedFileGrantClaim>, GrantClaimError> {
            match grant_reference_id(reference)? {
                Some(_) => Err(GrantClaimError),
                None => Ok(None),
            }
        }

        pub(crate) fn prepare_drive_backing_claim(
            &self,
            reference: &Path,
            _access: GrantAccess,
        ) -> Result<Option<PreparedDriveBackingClaim>, GrantClaimError> {
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

        pub(crate) fn prepare_socket_directory(
            &self,
            reference: &Path,
            _role: ResourceRole,
        ) -> Result<Option<PreparedSocketDirectoryClaim>, GrantClaimError> {
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

        pub(crate) fn vmnet_session_authority(
            &self,
        ) -> Result<
            (
                bangbang_session::SessionId,
                bangbang_session::VmnetAuthority,
            ),
            ContainedSessionError,
        > {
            Err(ContainedSessionError)
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
    PreparedDriveBackingClaim, PreparedFileGrantClaim, PreparedSocketDirectoryClaim,
};
#[cfg(target_os = "macos")]
pub(crate) use platform::{
    ClaimedVhostUserSocket, PreparedSocketBrokerEndpoint, PreparedVhostUserSocketClaim,
    SnapshotStagingRecordTracker, SocketBrokerAuthority, SocketBrokerEndpoint,
    VhostUserBrokerAuthority,
};
#[cfg(all(test, target_os = "macos"))]
pub(crate) use platform::{
    TestVhostDirectory, empty_grant_authority_for_vhost_test, file_grant_authority_for_test,
    vhost_directory_authority_for_test, vsock_directory_authority_for_test,
};
