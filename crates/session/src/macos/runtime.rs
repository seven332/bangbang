use std::ffi::{CStr, CString, OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::{ObjectIdentity, ResourceRole, SessionId, SocketChild};

const WORKER_CONTAINER_SUFFIX: &str = "Library/Containers/dev.bangbang.worker/Data/tmp";
const RUNTIME_ROOT_NAME: &str = "bangbang-sessions-v1";
const SESSION_PREFIX: &str = "session-";
const SESSION_NAME_BYTES: usize = SESSION_PREFIX.len() + 64;
const MAX_CONFSTR_BYTES: usize = 4096;
const MAX_PASSWD_BUFFER_BYTES: usize = 64 * 1024;
const DEFAULT_PASSWD_BUFFER_BYTES: usize = 16 * 1024;
const MAX_RECOVERY_ENTRIES: usize = 128;
const SOCKET_RECORD_BYTES: usize = 96;
const SOCKET_RECORD_MAGIC: [u8; 4] = *b"BBS1";
const SOCKET_RECORD_VERSION: u16 = 1;
const SNAPSHOT_RECORD_BYTES: usize = 128;
const SNAPSHOT_RECORD_MAGIC: [u8; 4] = *b"BBT1";
const SNAPSHOT_RECORD_VERSION: u16 = 1;
const SNAPSHOT_STATE_STAGING_PREFIX: &str = ".bangbang-snapshot-state-";
const SNAPSHOT_MEMORY_STAGING_PREFIX: &str = ".bangbang-snapshot-memory-";
const SNAPSHOT_STAGING_RANDOM_HEX_BYTES: usize = 32;

fn socket_record_name(role: ResourceRole) -> Result<&'static CStr, RuntimeError> {
    match role {
        ResourceRole::ApiSocketDirectory => Ok(c".api-socket-owner"),
        ResourceRole::VsockSocketDirectory => Ok(c".vsock-socket-owner"),
        _ => Err(RuntimeError::InvalidEntry),
    }
}

fn snapshot_record_name(kind: SnapshotStagingKind) -> &'static CStr {
    match kind {
        SnapshotStagingKind::State => c".snapshot-state-owner",
        SnapshotStagingKind::Memory => c".snapshot-memory-owner",
    }
}

/// Returns the fixed private staging name for one socket-directory role.
pub fn socket_staging_name(role: ResourceRole) -> Result<&'static CStr, RuntimeError> {
    match role {
        ResourceRole::ApiSocketDirectory => Ok(c".api-socket.pending"),
        ResourceRole::VsockSocketDirectory => Ok(c".vsock-socket.pending"),
        _ => Err(RuntimeError::InvalidEntry),
    }
}

/// Fixed cleanup evidence shared by the worker and its owning launcher.
#[derive(Clone, PartialEq, Eq)]
pub struct SocketOwnershipRecord {
    role: ResourceRole,
    child: SocketChild,
    identity: ObjectIdentity,
}

impl fmt::Debug for SocketOwnershipRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SocketOwnershipRecord")
            .field("role", &self.role)
            .field("child", &"<redacted>")
            .field("identity", &"<redacted>")
            .finish()
    }
}

impl SocketOwnershipRecord {
    /// Creates exact cleanup evidence for one published socket.
    pub fn new(
        role: ResourceRole,
        child: SocketChild,
        identity: ObjectIdentity,
    ) -> Result<Self, RuntimeError> {
        socket_record_name(role)?;
        Ok(Self {
            role,
            child,
            identity,
        })
    }

    /// Returns the exact singleton directory role.
    #[must_use]
    pub const fn role(&self) -> ResourceRole {
        self.role
    }

    /// Returns the redacted safe child value.
    #[must_use]
    pub const fn child(&self) -> &SocketChild {
        &self.child
    }

    /// Returns the socket identity captured before publication.
    #[must_use]
    pub const fn identity(&self) -> ObjectIdentity {
        self.identity
    }
}

/// One of the two independently staged native snapshot artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotStagingKind {
    /// State commit-marker artifact.
    State,
    /// Guest-memory artifact.
    Memory,
}

impl SnapshotStagingKind {
    const fn protocol_byte(self) -> u8 {
        match self {
            Self::State => 1,
            Self::Memory => 2,
        }
    }

    fn from_protocol_byte(value: u8) -> Result<Self, RuntimeError> {
        match value {
            1 => Ok(Self::State),
            2 => Ok(Self::Memory),
            _ => Err(RuntimeError::InvalidEntry),
        }
    }

    const fn staging_prefix(self) -> &'static str {
        match self {
            Self::State => SNAPSHOT_STATE_STAGING_PREFIX,
            Self::Memory => SNAPSHOT_MEMORY_STAGING_PREFIX,
        }
    }
}

/// Strict random staging component retained only as private cleanup evidence.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotStagingName(String);

impl SnapshotStagingName {
    /// Validates the exact artifact-specific prefix and lowercase random hex.
    pub fn parse(kind: SnapshotStagingKind, value: &str) -> Result<Self, RuntimeError> {
        let suffix = value
            .strip_prefix(kind.staging_prefix())
            .ok_or(RuntimeError::InvalidEntry)?;
        if suffix.len() != SNAPSHOT_STAGING_RANDOM_HEX_BYTES
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(RuntimeError::InvalidEntry);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the exact private component bytes for anchored cleanup.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for SnapshotStagingName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SnapshotStagingName(<redacted>)")
    }
}

/// Fixed cleanup evidence for one granted external snapshot staging inode.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotStagingOwnershipRecord {
    kind: SnapshotStagingKind,
    directory_identity: ObjectIdentity,
    name: SnapshotStagingName,
    file_identity: ObjectIdentity,
}

impl SnapshotStagingOwnershipRecord {
    /// Creates one exact, fully validated ownership record.
    pub fn new(
        kind: SnapshotStagingKind,
        directory_identity: ObjectIdentity,
        name: SnapshotStagingName,
        file_identity: ObjectIdentity,
    ) -> Self {
        Self {
            kind,
            directory_identity,
            name,
            file_identity,
        }
    }

    /// Returns the state or memory artifact kind.
    #[must_use]
    pub const fn kind(&self) -> SnapshotStagingKind {
        self.kind
    }

    /// Returns the exact granted-directory identity.
    #[must_use]
    pub const fn directory_identity(&self) -> ObjectIdentity {
        self.directory_identity
    }

    /// Returns the strict private staging component.
    #[must_use]
    pub const fn name(&self) -> &SnapshotStagingName {
        &self.name
    }

    /// Returns the exact staging-file identity.
    #[must_use]
    pub const fn file_identity(&self) -> ObjectIdentity {
        self.file_identity
    }
}

impl fmt::Debug for SnapshotStagingOwnershipRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotStagingOwnershipRecord")
            .field("kind", &self.kind)
            .field("directory_identity", &"<redacted>")
            .field("name", &"<redacted>")
            .field("file_identity", &"<redacted>")
            .finish()
    }
}

/// Whether a worker namespace may persist ownership records beneath its
/// linked session name.
#[cfg(feature = "elevated-bootstrap-probe")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceRecordPolicy {
    /// The ordinary linked namespace supports the existing record protocol.
    Linked,
    /// A feature-daemon namespace is retired before grants and remains record-free.
    RetiredRecordFree,
}

#[cfg(feature = "elevated-bootstrap-probe")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamespacePublication {
    Linked,
    Retired,
}

/// Worker-side duplicate of the locked private namespace directory.
pub struct WorkerSocketNamespace {
    directory: OwnedFd,
    identity: NamespaceIdentity,
    #[cfg(feature = "elevated-bootstrap-probe")]
    record_policy: NamespaceRecordPolicy,
}

impl fmt::Debug for WorkerSocketNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerSocketNamespace")
            .field("directory", &"<owned>")
            .field("identity", &self.identity)
            .finish()
    }
}

impl WorkerSocketNamespace {
    /// Constructs an anchored namespace from a test-owned directory.
    ///
    /// This bypasses production session naming and locking and is available
    /// only to repository test targets.
    #[doc(hidden)]
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_directory_for_test(path: &Path) -> Result<Self, RuntimeError> {
        let directory = open_directory(path)?;
        let identity = validate_directory(directory.as_raw_fd())?;
        Ok(Self {
            directory,
            identity,
            #[cfg(feature = "elevated-bootstrap-probe")]
            record_policy: NamespaceRecordPolicy::Linked,
        })
    }

    /// Duplicates the validated namespace anchor with close-on-exec ownership.
    pub fn try_clone(&self) -> Result<Self, RuntimeError> {
        Ok(Self {
            directory: duplicate_fd(self.directory.as_raw_fd())?,
            identity: self.identity,
            #[cfg(feature = "elevated-bootstrap-probe")]
            record_policy: self.record_policy,
        })
    }

    /// Returns the validated namespace anchor without transferring ownership.
    #[must_use]
    pub fn anchor_fd(&self) -> RawFd {
        self.directory.as_raw_fd()
    }

    /// Returns the validated namespace identity.
    #[must_use]
    pub const fn identity(&self) -> NamespaceIdentity {
        self.identity
    }

    /// Exclusively writes one fixed ownership record before publication.
    pub fn write_socket_record(&self, record: &SocketOwnershipRecord) -> Result<(), RuntimeError> {
        self.require_record_support()?;
        write_socket_record(self.anchor_fd(), record)
    }

    /// Reads one strict socket record for an exact role.
    pub fn socket_record(
        &self,
        role: ResourceRole,
    ) -> Result<Option<SocketOwnershipRecord>, RuntimeError> {
        self.require_record_support()?;
        read_socket_record(self.anchor_fd(), role)
    }

    /// Requires the current role record to equal the expected value exactly.
    pub fn require_socket_record(
        &self,
        expected: &SocketOwnershipRecord,
    ) -> Result<(), RuntimeError> {
        match self.socket_record(expected.role())? {
            Some(actual) if actual == *expected => Ok(()),
            Some(_) | None => Err(RuntimeError::InvalidEntry),
        }
    }

    /// Removes only the exact fixed staging socket described by a record.
    pub fn unlink_staged_socket(&self, record: &SocketOwnershipRecord) -> Result<(), RuntimeError> {
        self.require_record_support()?;
        unlink_staged_socket(self.anchor_fd(), record)
    }

    /// Removes only the exact current ownership record.
    pub fn clear_socket_record(&self, record: &SocketOwnershipRecord) -> Result<(), RuntimeError> {
        self.require_record_support()?;
        clear_socket_record(self.anchor_fd(), record)
    }

    /// Durably writes one fixed snapshot staging-ownership record.
    pub fn write_snapshot_staging_record(
        &self,
        record: &SnapshotStagingOwnershipRecord,
    ) -> Result<(), RuntimeError> {
        self.require_record_support()?;
        write_snapshot_record(self.anchor_fd(), record)
    }

    /// Removes only the exact current snapshot staging-ownership record.
    pub fn clear_snapshot_staging_record(
        &self,
        record: &SnapshotStagingOwnershipRecord,
    ) -> Result<(), RuntimeError> {
        self.require_record_support()?;
        clear_snapshot_record(self.anchor_fd(), record)
    }

    fn require_record_support(&self) -> Result<(), RuntimeError> {
        #[cfg(feature = "elevated-bootstrap-probe")]
        if self.record_policy == NamespaceRecordPolicy::RetiredRecordFree {
            return Err(RuntimeError::InvalidEntry);
        }
        Ok(())
    }
}

/// Device/inode proof sent in the bounded bootstrap protocol.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NamespaceIdentity {
    /// Filesystem device number.
    pub device: u64,
    /// Filesystem inode number.
    pub inode: u64,
}

impl NamespaceIdentity {
    /// Returns the equivalent protocol object identity.
    #[must_use]
    pub const fn object_identity(self) -> ObjectIdentity {
        ObjectIdentity {
            device: self.device,
            inode: self.inode,
        }
    }
}

impl fmt::Debug for NamespaceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("NamespaceIdentity(<redacted>)")
    }
}

/// Redacted runtime namespace failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeError {
    /// A filesystem operation failed.
    Filesystem(io::ErrorKind),
    /// Creating the exact random session directory failed.
    NamespaceCreate(io::ErrorKind),
    /// The expected fixed container/root contract was not satisfied.
    InvalidRoot,
    /// A session entry failed owner, mode, type, identity, lock, or emptiness checks.
    InvalidEntry,
    /// The random session entry already exists.
    Collision,
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private runtime namespace failure")
    }
}

impl std::error::Error for RuntimeError {}

#[derive(Clone, Copy)]
struct DirectoryOwner {
    uid: libc::uid_t,
    gid: Option<libc::gid_t>,
}

impl DirectoryOwner {
    fn current_user() -> Self {
        // SAFETY: The identity getter has no pointer or ownership contract.
        let uid = unsafe { libc::geteuid() };
        Self { uid, gid: None }
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    const fn exact(uid: libc::uid_t, gid: libc::gid_t) -> Self {
        Self {
            uid,
            gid: Some(gid),
        }
    }
}

/// Independently opened, descriptor-rooted authority for evidence-only runtimes.
#[cfg(feature = "elevated-bootstrap-probe")]
pub struct ExplicitRuntimeRoot {
    directory: OwnedFd,
    identity: NamespaceIdentity,
    owner: DirectoryOwner,
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl fmt::Debug for ExplicitRuntimeRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExplicitRuntimeRoot(<redacted>)")
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl ExplicitRuntimeRoot {
    /// Adopts and validates one already-opened exact target-owned runtime root.
    pub fn from_owned_fd(
        directory: OwnedFd,
        expected: ObjectIdentity,
        uid: libc::uid_t,
        gid: libc::gid_t,
        require_empty: bool,
    ) -> Result<Self, RuntimeError> {
        let owner = DirectoryOwner::exact(uid, gid);
        let stat = directory_stat(directory.as_raw_fd())?;
        let linked = stat.st_nlink >= 2;
        let identity = validate_directory_stat(stat, owner)?;
        if !linked
            || identity.device != expected.device
            || identity.inode != expected.inode
            || (require_empty && !directory_is_empty(directory.as_raw_fd())?)
        {
            return Err(RuntimeError::InvalidRoot);
        }
        Ok(Self {
            directory,
            identity,
            owner,
        })
    }

    /// Reopens the same directory with an independent file description.
    pub fn try_reopen(&self, require_empty: bool) -> Result<Self, RuntimeError> {
        let directory = openat_directory(self.directory.as_raw_fd(), c".")?;
        let identity = validate_directory_owned(directory.as_raw_fd(), self.owner)?;
        if identity != self.identity
            || (require_empty && !directory_is_empty(directory.as_raw_fd())?)
        {
            return Err(RuntimeError::InvalidRoot);
        }
        Ok(Self {
            directory,
            identity,
            owner: self.owner,
        })
    }

    /// Returns the retained root descriptor without transferring ownership.
    #[must_use]
    pub fn as_raw_fd(&self) -> RawFd {
        self.directory.as_raw_fd()
    }

    /// Returns the validated root identity without revealing its path.
    #[must_use]
    pub const fn identity(&self) -> NamespaceIdentity {
        self.identity
    }
}

/// Unpublished launcher-created session with three independent descriptions.
#[cfg(feature = "elevated-bootstrap-probe")]
pub struct PreparedLauncherSession {
    transfer: OwnedFd,
    handles: LauncherSessionHandles,
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl fmt::Debug for PreparedLauncherSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedLauncherSession(<redacted>)")
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl PreparedLauncherSession {
    /// Creates one target-owned session and preopens every later authority.
    pub fn create(root: ExplicitRuntimeRoot, session: SessionId) -> Result<Self, RuntimeError> {
        let validation_root = root.try_reopen(true)?;
        let recovery_root = root.try_reopen(true)?;
        let ExplicitRuntimeRoot {
            directory: root,
            identity: root_identity,
            owner,
        } = root;
        if validate_directory_owned(root.as_raw_fd(), owner)? != root_identity
            || !directory_is_empty(root.as_raw_fd())?
        {
            return Err(RuntimeError::InvalidRoot);
        }
        let root_lock = RootLock::acquire(root.as_raw_fd())?;
        recover_stale_entries(root.as_raw_fd(), owner)?;
        let name = session_name(session)?;
        // SAFETY: `root` is a live exact directory, `name` is NUL-terminated,
        // and the permanently transitioned launcher owns the new child.
        if unsafe { libc::mkdirat(root.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::AlreadyExists {
                Err(RuntimeError::Collision)
            } else {
                Err(RuntimeError::NamespaceCreate(error.kind()))
            };
        }

        let transfer = match openat_directory(root.as_raw_fd(), &name) {
            Ok(transfer) => transfer,
            Err(error) => {
                return Err(cleanup_created_session_after_error(
                    root.as_raw_fd(),
                    &name,
                    owner,
                    error,
                ));
            }
        };
        let identity = match validate_linked_directory_owned(transfer.as_raw_fd(), owner) {
            Ok(identity) => identity,
            Err(error) => {
                return Err(cleanup_open_session_after_error(
                    root.as_raw_fd(),
                    transfer.as_raw_fd(),
                    &name,
                    owner,
                    error,
                ));
            }
        };
        let session_is_exact = match directory_is_empty(transfer.as_raw_fd()).and_then(|empty| {
            identity_at(root.as_raw_fd(), &name).map(|linked| empty && linked == Some(identity))
        }) {
            Ok(exact) => exact,
            Err(error) => {
                return Err(cleanup_open_session_after_error(
                    root.as_raw_fd(),
                    transfer.as_raw_fd(),
                    &name,
                    owner,
                    error,
                ));
            }
        };
        if !session_is_exact {
            return Err(cleanup_open_session_after_error(
                root.as_raw_fd(),
                transfer.as_raw_fd(),
                &name,
                owner,
                RuntimeError::InvalidEntry,
            ));
        }

        let validation =
            openat_directory(validation_root.directory.as_raw_fd(), &name).and_then(|directory| {
                PreopenedLauncherNamespace::new(
                    validation_root.directory,
                    directory,
                    name.clone(),
                    identity,
                    owner,
                )
            });
        let validation = match validation {
            Ok(validation) => validation,
            Err(error) => {
                return Err(cleanup_open_session_after_error(
                    root.as_raw_fd(),
                    transfer.as_raw_fd(),
                    &name,
                    owner,
                    error,
                ));
            }
        };
        let recovery =
            openat_directory(recovery_root.directory.as_raw_fd(), &name).and_then(|directory| {
                PreopenedLauncherNamespace::new(
                    recovery_root.directory,
                    directory,
                    name.clone(),
                    identity,
                    owner,
                )
            });
        let recovery = match recovery {
            Ok(recovery) => recovery,
            Err(error) => {
                return Err(cleanup_open_session_after_error(
                    root.as_raw_fd(),
                    transfer.as_raw_fd(),
                    &name,
                    owner,
                    error,
                ));
            }
        };
        drop(root_lock);
        drop(root);
        Ok(Self {
            transfer,
            handles: LauncherSessionHandles {
                validation: Some(validation),
                recovery: Some(recovery),
                session,
                identity,
            },
        })
    }

    /// Returns the exact created session identity.
    #[must_use]
    pub const fn identity(&self) -> NamespaceIdentity {
        self.handles.identity
    }

    /// Separates the single-use transfer descriptor from later launcher state.
    #[must_use]
    pub fn into_publication(self) -> (OwnedFd, LauncherSessionHandles) {
        (self.transfer, self.handles)
    }

    /// Removes an exact session that was never published to the worker.
    pub fn cleanup_unpublished(self) -> Result<(), RuntimeError> {
        let Self {
            transfer,
            mut handles,
        } = self;
        drop(transfer);
        handles.discard_validation();
        let session = handles.session();
        handles
            .recover_after_worker_exit(session)?
            .map_or(Ok(()), |mut namespace| namespace.cleanup())
    }
}

/// Preopened independent validation and recovery state after publication.
#[cfg(feature = "elevated-bootstrap-probe")]
pub struct LauncherSessionHandles {
    validation: Option<PreopenedLauncherNamespace>,
    recovery: Option<PreopenedLauncherNamespace>,
    session: SessionId,
    identity: NamespaceIdentity,
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl fmt::Debug for LauncherSessionHandles {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LauncherSessionHandles(<redacted>)")
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl LauncherSessionHandles {
    /// Returns the exact created session identity.
    #[must_use]
    pub const fn identity(&self) -> NamespaceIdentity {
        self.identity
    }

    /// Returns the lifecycle session bound to the canonical directory name.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Consumes the independent live-validation description after Prepared.
    pub fn validate_live(
        &mut self,
        session: SessionId,
        expected: NamespaceIdentity,
    ) -> Result<LauncherNamespace, RuntimeError> {
        if session != self.session || expected != self.identity {
            return Err(RuntimeError::InvalidEntry);
        }
        self.validation
            .take()
            .ok_or(RuntimeError::InvalidEntry)?
            .validate_live(expected)
    }

    /// Consumes the independent recovery description after the worker is reaped.
    pub fn recover_after_worker_exit(
        &mut self,
        session: SessionId,
    ) -> Result<Option<LauncherNamespace>, RuntimeError> {
        if session != self.session {
            return Err(RuntimeError::InvalidEntry);
        }
        self.recovery
            .take()
            .ok_or(RuntimeError::InvalidEntry)?
            .recover_after_worker_exit()
    }

    fn discard_validation(&mut self) {
        drop(self.validation.take());
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
struct PreopenedLauncherNamespace {
    root: OwnedFd,
    directory: OwnedFd,
    name: CString,
    identity: NamespaceIdentity,
    owner: DirectoryOwner,
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl PreopenedLauncherNamespace {
    fn new(
        root: OwnedFd,
        directory: OwnedFd,
        name: CString,
        identity: NamespaceIdentity,
        owner: DirectoryOwner,
    ) -> Result<Self, RuntimeError> {
        validate_preopened_namespace(
            root.as_raw_fd(),
            directory.as_raw_fd(),
            &name,
            identity,
            owner,
            true,
        )?;
        Ok(Self {
            root,
            directory,
            name,
            identity,
            owner,
        })
    }

    fn validate_live(self, expected: NamespaceIdentity) -> Result<LauncherNamespace, RuntimeError> {
        validate_preopened_namespace(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            expected,
            self.owner,
            true,
        )?;
        if expected != self.identity {
            return Err(RuntimeError::InvalidEntry);
        }
        if try_lock_exclusive(self.directory.as_raw_fd())? {
            unlock(self.directory.as_raw_fd());
            return Err(RuntimeError::InvalidEntry);
        }
        Ok(LauncherNamespace {
            root: self.root,
            directory: self.directory,
            name: self.name,
            identity: self.identity,
            owner: self.owner,
            publication: NamespacePublication::Linked,
            cleaned: false,
        })
    }

    fn recover_after_worker_exit(self) -> Result<Option<LauncherNamespace>, RuntimeError> {
        validate_directory_owned(self.root.as_raw_fd(), self.owner)?;
        if identity_at(self.root.as_raw_fd(), &self.name)?.is_none() {
            return Ok(None);
        }
        validate_preopened_namespace(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            self.identity,
            self.owner,
            false,
        )?;
        if !try_lock_exclusive(self.directory.as_raw_fd())?
            || !directory_contains_only_ownership_records(self.directory.as_raw_fd())?
        {
            return Err(RuntimeError::InvalidEntry);
        }
        Ok(Some(LauncherNamespace {
            root: self.root,
            directory: self.directory,
            name: self.name,
            identity: self.identity,
            owner: self.owner,
            publication: NamespacePublication::Linked,
            cleaned: false,
        }))
    }
}

/// Validated but not yet locked worker adoption state.
#[cfg(feature = "elevated-bootstrap-probe")]
pub struct ValidatedWorkerNamespace {
    root: OwnedFd,
    directory: OwnedFd,
    name: CString,
    identity: NamespaceIdentity,
    owner: DirectoryOwner,
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl fmt::Debug for ValidatedWorkerNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ValidatedWorkerNamespace(<redacted>)")
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl ValidatedWorkerNamespace {
    /// Validates one launcher-created session without acquiring its live lock.
    pub fn from_explicit_root(
        root: ExplicitRuntimeRoot,
        directory: OwnedFd,
        session: SessionId,
        expected: ObjectIdentity,
    ) -> Result<Self, RuntimeError> {
        let name = session_name(session)?;
        let identity = NamespaceIdentity {
            device: expected.device,
            inode: expected.inode,
        };
        validate_preopened_namespace(
            root.directory.as_raw_fd(),
            directory.as_raw_fd(),
            &name,
            identity,
            root.owner,
            true,
        )?;
        Ok(Self {
            root: root.directory,
            directory,
            name,
            identity,
            owner: root.owner,
        })
    }

    /// Acquires the worker lock and repeats every identity/emptiness check.
    pub fn lock(self) -> Result<WorkerNamespace, RuntimeError> {
        lock_exclusive(self.directory.as_raw_fd())?;
        validate_preopened_namespace(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            self.identity,
            self.owner,
            true,
        )?;
        Ok(WorkerNamespace {
            root: self.root,
            directory: self.directory,
            name: self.name,
            identity: self.identity,
            owner: self.owner,
            publication: NamespacePublication::Linked,
            cleaned: false,
        })
    }
}

/// Worker-owned locked namespace inside its App Sandbox container.
pub struct WorkerNamespace {
    root: OwnedFd,
    directory: OwnedFd,
    name: CString,
    identity: NamespaceIdentity,
    owner: DirectoryOwner,
    #[cfg(feature = "elevated-bootstrap-probe")]
    publication: NamespacePublication,
    cleaned: bool,
}

impl fmt::Debug for WorkerNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkerNamespace")
            .field("identity", &self.identity)
            .field("path", &"<redacted>")
            .finish()
    }
}

impl WorkerNamespace {
    /// Recovers bounded stale entries and creates the exact session namespace.
    pub fn create(session: SessionId) -> Result<Self, RuntimeError> {
        let root_path = worker_runtime_root()?;
        let root = ensure_runtime_root(&root_path)?;
        Self::create_in_root(root, DirectoryOwner::current_user(), session)
    }

    /// Creates the exact session beneath an already-opened target-owned root.
    #[cfg(feature = "elevated-bootstrap-probe")]
    pub fn create_from_explicit_root(
        root: ExplicitRuntimeRoot,
        session: SessionId,
    ) -> Result<Self, RuntimeError> {
        Self::create_in_root(root.directory, root.owner, session)
    }

    /// Validates, locks, and adopts one launcher-created target session.
    #[cfg(feature = "elevated-bootstrap-probe")]
    pub fn adopt_from_explicit_root(
        root: ExplicitRuntimeRoot,
        directory: OwnedFd,
        session: SessionId,
        expected: ObjectIdentity,
    ) -> Result<Self, RuntimeError> {
        ValidatedWorkerNamespace::from_explicit_root(root, directory, session, expected)?.lock()
    }

    fn create_in_root(
        root: OwnedFd,
        owner: DirectoryOwner,
        session: SessionId,
    ) -> Result<Self, RuntimeError> {
        validate_directory_owned(root.as_raw_fd(), owner)?;
        let root_lock = RootLock::acquire(root.as_raw_fd())?;
        recover_stale_entries(root.as_raw_fd(), owner)?;
        let name = session_name(session)?;
        // SAFETY: `root` is a live directory fd, `name` is NUL-terminated, and
        // no pointer is retained.
        if unsafe { libc::mkdirat(root.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::AlreadyExists {
                Err(RuntimeError::Collision)
            } else {
                Err(RuntimeError::NamespaceCreate(error.kind()))
            };
        }

        // Once the name is published, failures preserve it for identity-checked
        // stale recovery. Blind rollback could remove a same-user replacement
        // installed between `mkdirat` and the failing operation.
        let directory = openat_directory(root.as_raw_fd(), &name)?;
        let identity = validate_directory_owned(directory.as_raw_fd(), owner)?;
        lock_exclusive(directory.as_raw_fd())?;
        drop(root_lock);
        Ok(Self {
            root,
            directory,
            name,
            identity,
            owner,
            #[cfg(feature = "elevated-bootstrap-probe")]
            publication: NamespacePublication::Linked,
            cleaned: false,
        })
    }

    /// Returns the exact identity to report in `Prepared`.
    #[must_use]
    pub const fn identity(&self) -> NamespaceIdentity {
        self.identity
    }

    /// Duplicates the locked namespace anchor for socket staging and records.
    pub fn socket_namespace(&self) -> Result<WorkerSocketNamespace, RuntimeError> {
        Ok(WorkerSocketNamespace {
            directory: duplicate_fd(self.directory.as_raw_fd())?,
            identity: self.identity,
            #[cfg(feature = "elevated-bootstrap-probe")]
            record_policy: NamespaceRecordPolicy::Linked,
        })
    }

    /// Duplicates this namespace with an explicit feature-probe record policy.
    #[cfg(feature = "elevated-bootstrap-probe")]
    pub fn socket_namespace_with_policy(
        &self,
        record_policy: NamespaceRecordPolicy,
    ) -> Result<WorkerSocketNamespace, RuntimeError> {
        Ok(WorkerSocketNamespace {
            directory: duplicate_fd(self.directory.as_raw_fd())?,
            identity: self.identity,
            record_policy,
        })
    }

    /// Enters the locked namespace as the process working directory and rechecks it.
    pub fn enter(&self) -> Result<(), RuntimeError> {
        // SAFETY: `directory` is the retained, locked namespace anchor.
        if unsafe { libc::fchdir(self.directory.as_raw_fd()) } != 0 {
            return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
        }
        self.verify_current_directory()
    }

    /// Rechecks that the process working directory is the retained namespace.
    pub fn verify_current_directory(&self) -> Result<(), RuntimeError> {
        if current_directory_identity(self.owner)? == self.identity {
            Ok(())
        } else {
            Err(RuntimeError::InvalidEntry)
        }
    }

    /// Revalidates the linked namespace while this lock-owning authority remains live.
    #[cfg(feature = "elevated-bootstrap-probe")]
    pub fn verify_live(&self) -> Result<(), RuntimeError> {
        match self.publication {
            NamespacePublication::Linked => validate_preopened_namespace(
                self.root.as_raw_fd(),
                self.directory.as_raw_fd(),
                &self.name,
                self.identity,
                self.owner,
                false,
            )?,
            NamespacePublication::Retired => validate_retired_namespace(
                self.root.as_raw_fd(),
                self.directory.as_raw_fd(),
                &self.name,
                self.identity,
                self.owner,
                false,
            )?,
        }
        self.verify_current_directory()
    }

    /// Observes that the launcher retired the exact canonical session name.
    #[cfg(feature = "elevated-bootstrap-probe")]
    pub fn observe_retired(&mut self) -> Result<(), RuntimeError> {
        let current = current_directory_identity(self.owner)?;
        self.observe_retired_with_current_directory(current)
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn observe_retired_with_current_directory(
        &mut self,
        current: NamespaceIdentity,
    ) -> Result<(), RuntimeError> {
        if self.publication != NamespacePublication::Linked {
            return Err(RuntimeError::InvalidEntry);
        }
        validate_retired_namespace(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            self.identity,
            self.owner,
            true,
        )?;
        if current != self.identity {
            return Err(RuntimeError::InvalidEntry);
        }
        self.publication = NamespacePublication::Retired;
        Ok(())
    }

    /// Removes only the same empty namespace inode.
    pub fn cleanup(&mut self) -> Result<(), RuntimeError> {
        if self.cleaned {
            return Ok(());
        }
        #[cfg(feature = "elevated-bootstrap-probe")]
        match self.publication {
            NamespacePublication::Linked => cleanup_exact(
                self.root.as_raw_fd(),
                self.directory.as_raw_fd(),
                &self.name,
                self.identity,
                self.owner,
            )?,
            NamespacePublication::Retired => validate_retired_namespace(
                self.root.as_raw_fd(),
                self.directory.as_raw_fd(),
                &self.name,
                self.identity,
                self.owner,
                true,
            )?,
        }
        #[cfg(not(feature = "elevated-bootstrap-probe"))]
        cleanup_exact(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            self.identity,
            self.owner,
        )?;
        self.cleaned = true;
        Ok(())
    }
}

impl Drop for WorkerNamespace {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

struct RootLock(RawFd);

impl RootLock {
    fn acquire(fd: RawFd) -> Result<Self, RuntimeError> {
        loop {
            // SAFETY: `fd` is the live runtime-root descriptor retained by the
            // caller. The blocking lock covers only bounded recovery and one
            // session-directory creation.
            if unsafe { libc::flock(fd, libc::LOCK_EX) } == 0 {
                return Ok(Self(fd));
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(RuntimeError::Filesystem(error.kind()));
            }
        }
    }
}

impl Drop for RootLock {
    fn drop(&mut self) {
        unlock(self.0);
    }
}

/// Launcher-held independent validation/cleanup handle for one worker namespace.
pub struct LauncherNamespace {
    root: OwnedFd,
    directory: OwnedFd,
    name: CString,
    identity: NamespaceIdentity,
    owner: DirectoryOwner,
    #[cfg(feature = "elevated-bootstrap-probe")]
    publication: NamespacePublication,
    cleaned: bool,
}

impl fmt::Debug for LauncherNamespace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LauncherNamespace")
            .field("identity", &self.identity)
            .field("path", &"<redacted>")
            .finish()
    }
}

impl LauncherNamespace {
    /// Duplicates the live namespace for one bounded socket publication transaction.
    pub fn socket_namespace(&self) -> Result<WorkerSocketNamespace, RuntimeError> {
        self.verify_worker_lock_inner()?;
        Ok(WorkerSocketNamespace {
            directory: duplicate_fd(self.directory.as_raw_fd())?,
            identity: self.identity,
            #[cfg(feature = "elevated-bootstrap-probe")]
            record_policy: match self.publication {
                NamespacePublication::Linked => NamespaceRecordPolicy::Linked,
                NamespacePublication::Retired => NamespaceRecordPolicy::RetiredRecordFree,
            },
        })
    }

    /// Revalidates the exact retained namespace and proves the worker lock remains held.
    #[cfg(feature = "elevated-bootstrap-probe")]
    pub fn verify_worker_lock(&self) -> Result<(), RuntimeError> {
        self.verify_worker_lock_inner()
    }

    fn verify_worker_lock_inner(&self) -> Result<(), RuntimeError> {
        #[cfg(feature = "elevated-bootstrap-probe")]
        match self.publication {
            NamespacePublication::Linked => validate_preopened_namespace(
                self.root.as_raw_fd(),
                self.directory.as_raw_fd(),
                &self.name,
                self.identity,
                self.owner,
                false,
            )?,
            NamespacePublication::Retired => validate_retired_namespace(
                self.root.as_raw_fd(),
                self.directory.as_raw_fd(),
                &self.name,
                self.identity,
                self.owner,
                false,
            )?,
        }
        #[cfg(not(feature = "elevated-bootstrap-probe"))]
        validate_preopened_namespace(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            self.identity,
            self.owner,
            false,
        )?;
        if try_lock_exclusive(self.directory.as_raw_fd())? {
            unlock(self.directory.as_raw_fd());
            return Err(RuntimeError::InvalidEntry);
        }
        Ok(())
    }

    /// Retires the exact empty canonical name while the worker lock is live.
    #[cfg(feature = "elevated-bootstrap-probe")]
    pub fn retire_linked(&mut self) -> Result<(), RuntimeError> {
        if self.publication != NamespacePublication::Linked {
            return Err(RuntimeError::InvalidEntry);
        }
        validate_preopened_namespace(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            self.identity,
            self.owner,
            true,
        )?;
        if try_lock_exclusive(self.directory.as_raw_fd())? {
            unlock(self.directory.as_raw_fd());
            return Err(RuntimeError::InvalidEntry);
        }
        // SAFETY: the retained root and canonical name identify the exact empty
        // directory validated above. Any syscall failure is fail-closed.
        if unsafe {
            libc::unlinkat(
                self.root.as_raw_fd(),
                self.name.as_ptr(),
                libc::AT_REMOVEDIR,
            )
        } != 0
        {
            return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
        }
        self.publication = NamespacePublication::Retired;
        validate_retired_namespace(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            self.identity,
            self.owner,
            true,
        )
    }

    /// Independently derives and validates the worker-created namespace.
    pub fn validate(session: SessionId, expected: NamespaceIdentity) -> Result<Self, RuntimeError> {
        let root_path = launcher_runtime_root()?;
        let root = open_directory(&root_path)?;
        Self::validate_in_root(root, DirectoryOwner::current_user(), session, expected)
    }

    /// Validates a worker-created namespace beneath an explicit target-owned root.
    #[cfg(feature = "elevated-bootstrap-probe")]
    pub fn validate_from_explicit_root(
        root: ExplicitRuntimeRoot,
        session: SessionId,
        expected: NamespaceIdentity,
    ) -> Result<Self, RuntimeError> {
        Self::validate_in_root(root.directory, root.owner, session, expected)
    }

    fn validate_in_root(
        root: OwnedFd,
        owner: DirectoryOwner,
        session: SessionId,
        expected: NamespaceIdentity,
    ) -> Result<Self, RuntimeError> {
        validate_directory_owned(root.as_raw_fd(), owner)?;
        let name = session_name(session)?;
        let directory = openat_directory(root.as_raw_fd(), &name)?;
        let actual = validate_directory_owned(directory.as_raw_fd(), owner)?;
        if actual != expected || !directory_is_empty(directory.as_raw_fd())? {
            return Err(RuntimeError::InvalidEntry);
        }
        // A successful lock would mean no live worker holds ownership. Release
        // it immediately and reject the bootstrap rather than authorizing it.
        if try_lock_exclusive(directory.as_raw_fd())? {
            unlock(directory.as_raw_fd());
            return Err(RuntimeError::InvalidEntry);
        }
        Ok(Self {
            root,
            directory,
            name,
            identity: actual,
            owner,
            #[cfg(feature = "elevated-bootstrap-probe")]
            publication: NamespacePublication::Linked,
            cleaned: false,
        })
    }

    /// Recovers the exact namespace after the owned worker is reaped.
    ///
    /// This covers failures before `Prepared` was decoded. A missing root or
    /// session name is ordinary; live, replaced, unrelated, or invalid entries
    /// remain fail-closed. Strict socket records remain for launcher cleanup.
    pub fn recover_after_worker_exit(session: SessionId) -> Result<Option<Self>, RuntimeError> {
        let root_path = launcher_runtime_root()?;
        let root = match open_directory(&root_path) {
            Ok(root) => root,
            Err(RuntimeError::Filesystem(io::ErrorKind::NotFound)) => return Ok(None),
            Err(error) => return Err(error),
        };
        Self::recover_in_root(root, DirectoryOwner::current_user(), session)
    }

    /// Recovers an exact namespace after worker exit beneath an explicit root.
    #[cfg(feature = "elevated-bootstrap-probe")]
    pub fn recover_after_worker_exit_from_explicit_root(
        root: ExplicitRuntimeRoot,
        session: SessionId,
    ) -> Result<Option<Self>, RuntimeError> {
        Self::recover_in_root(root.directory, root.owner, session)
    }

    fn recover_in_root(
        root: OwnedFd,
        owner: DirectoryOwner,
        session: SessionId,
    ) -> Result<Option<Self>, RuntimeError> {
        validate_directory_owned(root.as_raw_fd(), owner)?;
        let name = session_name(session)?;
        let directory = match openat_directory(root.as_raw_fd(), &name) {
            Ok(directory) => directory,
            Err(RuntimeError::Filesystem(io::ErrorKind::NotFound)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let identity = validate_directory_owned(directory.as_raw_fd(), owner)?;
        if !try_lock_exclusive(directory.as_raw_fd())? {
            return Err(RuntimeError::InvalidEntry);
        }
        if !directory_contains_only_ownership_records(directory.as_raw_fd())? {
            return Err(RuntimeError::InvalidEntry);
        }
        Ok(Some(Self {
            root,
            directory,
            name,
            identity,
            owner,
            #[cfg(feature = "elevated-bootstrap-probe")]
            publication: NamespacePublication::Linked,
            cleaned: false,
        }))
    }

    /// Removes only the same empty entry after the worker lock is released.
    pub fn cleanup(&mut self) -> Result<(), RuntimeError> {
        if self.cleaned {
            return Ok(());
        }
        #[cfg(feature = "elevated-bootstrap-probe")]
        if self.publication == NamespacePublication::Retired {
            validate_retired_namespace(
                self.root.as_raw_fd(),
                self.directory.as_raw_fd(),
                &self.name,
                self.identity,
                self.owner,
                true,
            )?;
            if !try_lock_exclusive(self.directory.as_raw_fd())? {
                return Err(RuntimeError::InvalidEntry);
            }
            unlock(self.directory.as_raw_fd());
            self.cleaned = true;
            return Ok(());
        }
        if path_missing(self.root.as_raw_fd(), &self.name)? {
            self.cleaned = true;
            return Ok(());
        }
        if !try_lock_exclusive(self.directory.as_raw_fd())? {
            return Err(RuntimeError::InvalidEntry);
        }
        cleanup_exact(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            self.identity,
            self.owner,
        )?;
        self.cleaned = true;
        Ok(())
    }

    /// Reads the at-most-two strict socket ownership records after worker exit.
    pub fn socket_ownership_records(&self) -> Result<Vec<SocketOwnershipRecord>, RuntimeError> {
        let mut records = Vec::with_capacity(2);
        for role in [
            ResourceRole::ApiSocketDirectory,
            ResourceRole::VsockSocketDirectory,
        ] {
            if let Some(record) = read_socket_record(self.directory.as_raw_fd(), role)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// Removes only the exact private staging socket described by a record.
    pub fn unlink_staged_socket(&self, record: &SocketOwnershipRecord) -> Result<(), RuntimeError> {
        unlink_staged_socket(self.directory.as_raw_fd(), record)
    }

    /// Removes only an exact validated socket ownership record.
    pub fn clear_socket_record(&self, record: &SocketOwnershipRecord) -> Result<(), RuntimeError> {
        clear_socket_record(self.directory.as_raw_fd(), record)
    }

    /// Reads the at-most-two strict snapshot staging records after worker exit.
    pub fn snapshot_staging_records(
        &self,
    ) -> Result<Vec<SnapshotStagingOwnershipRecord>, RuntimeError> {
        let mut records = Vec::with_capacity(2);
        for kind in [SnapshotStagingKind::State, SnapshotStagingKind::Memory] {
            if let Some(record) = read_snapshot_record(self.directory.as_raw_fd(), kind)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// Removes only an exact validated snapshot staging record.
    pub fn clear_snapshot_staging_record(
        &self,
        record: &SnapshotStagingOwnershipRecord,
    ) -> Result<(), RuntimeError> {
        clear_snapshot_record(self.directory.as_raw_fd(), record)
    }
}

impl Drop for LauncherNamespace {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn worker_runtime_root() -> Result<PathBuf, RuntimeError> {
    let temp = confstr_path(libc::_CS_DARWIN_USER_TEMP_DIR)?;
    if !temp.is_absolute() || !temp.ends_with(Path::new(WORKER_CONTAINER_SUFFIX)) {
        return Err(RuntimeError::InvalidRoot);
    }
    Ok(temp.join(RUNTIME_ROOT_NAME))
}

fn launcher_runtime_root() -> Result<PathBuf, RuntimeError> {
    Ok(user_home()?
        .join(WORKER_CONTAINER_SUFFIX)
        .join(RUNTIME_ROOT_NAME))
}

fn user_home() -> Result<PathBuf, RuntimeError> {
    // SAFETY: Identity and sysconf calls have no pointer ownership contract.
    let uid = unsafe { libc::geteuid() };
    // SAFETY: `_SC_GETPW_R_SIZE_MAX` returns a size hint or -1.
    let suggested = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
    let buffer_bytes = usize::try_from(suggested)
        .ok()
        .filter(|size| *size > 0)
        .unwrap_or(DEFAULT_PASSWD_BUFFER_BYTES)
        .min(MAX_PASSWD_BUFFER_BYTES);
    let mut buffer = vec![0_u8; buffer_bytes];
    let mut password = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    // SAFETY: `password`, `buffer`, and `result` provide writable storage for
    // the synchronous reentrant lookup and remain live for the call.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            password.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if status != 0 || result.is_null() || result != password.as_mut_ptr() {
        return Err(RuntimeError::InvalidRoot);
    }
    // SAFETY: Successful `getpwuid_r` initialized `password`, and `pw_dir`
    // points into `buffer`, which remains live while it is copied below.
    let password = unsafe { password.assume_init() };
    if password.pw_dir.is_null() {
        return Err(RuntimeError::InvalidRoot);
    }
    // SAFETY: A successful passwd result supplies a NUL-terminated home path.
    let home = PathBuf::from(OsString::from_vec(
        unsafe { CStr::from_ptr(password.pw_dir) }
            .to_bytes()
            .to_vec(),
    ));
    if !home.is_absolute() {
        return Err(RuntimeError::InvalidRoot);
    }
    Ok(home)
}

fn confstr_path(key: libc::c_int) -> Result<PathBuf, RuntimeError> {
    // SAFETY: A null buffer and zero length query only the required size.
    let required = unsafe { libc::confstr(key, std::ptr::null_mut(), 0) };
    if !(2..=MAX_CONFSTR_BYTES).contains(&required) {
        return Err(RuntimeError::InvalidRoot);
    }
    let mut bytes = vec![0_u8; required];
    // SAFETY: `bytes` has exactly `required` writable bytes and remains live.
    let written = unsafe { libc::confstr(key, bytes.as_mut_ptr().cast(), bytes.len()) };
    let content = bytes
        .len()
        .checked_sub(1)
        .and_then(|end| bytes.get(..end))
        .ok_or(RuntimeError::InvalidRoot)?;
    if written != required || bytes.last() != Some(&0) || content.contains(&0) {
        return Err(RuntimeError::InvalidRoot);
    }
    bytes.pop();
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn ensure_runtime_root(path: &Path) -> Result<OwnedFd, RuntimeError> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(RuntimeError::Filesystem(error.kind())),
    }
    let root = open_directory(path)?;
    validate_directory(root.as_raw_fd())?;
    Ok(root)
}

trait DirBuilderMode {
    fn mode(&mut self, mode: u32) -> &mut Self;
}

impl DirBuilderMode for fs::DirBuilder {
    fn mode(&mut self, mode: u32) -> &mut Self {
        std::os::unix::fs::DirBuilderExt::mode(self, mode)
    }
}

fn open_directory(path: &Path) -> Result<OwnedFd, RuntimeError> {
    let path = cstring(path.as_os_str()).map_err(|_| RuntimeError::InvalidRoot)?;
    // SAFETY: `path` is NUL-terminated and no pointer is retained. A successful
    // descriptor is transferred immediately to `OwnedFd`.
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned_fd(fd)
}

fn openat_directory(root: RawFd, name: &CStr) -> Result<OwnedFd, RuntimeError> {
    // SAFETY: `root` is a live directory descriptor, `name` is NUL-terminated,
    // and a successful descriptor is transferred immediately to `OwnedFd`.
    let fd = unsafe {
        libc::openat(
            root,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    owned_fd(fd)
}

fn owned_fd(fd: RawFd) -> Result<OwnedFd, RuntimeError> {
    if fd < 0 {
        Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()))
    } else {
        // SAFETY: `fd` is a fresh successful result and ownership is transferred.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn duplicate_fd(fd: RawFd) -> Result<OwnedFd, RuntimeError> {
    // SAFETY: `fd` remains live for `fcntl`; success returns a fresh owned descriptor.
    owned_fd(unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) })
}

fn encode_socket_record(record: &SocketOwnershipRecord) -> [u8; SOCKET_RECORD_BYTES] {
    let mut bytes = [0_u8; SOCKET_RECORD_BYTES];
    bytes[0..4].copy_from_slice(&SOCKET_RECORD_MAGIC);
    bytes[4..6].copy_from_slice(&SOCKET_RECORD_VERSION.to_be_bytes());
    bytes[6] = record.role as u8;
    bytes[7] = u8::try_from(record.child.as_bytes().len()).unwrap_or(0);
    bytes[8..16].copy_from_slice(&record.identity.device.to_be_bytes());
    bytes[16..24].copy_from_slice(&record.identity.inode.to_be_bytes());
    let child_end = 24 + record.child.as_bytes().len();
    if let Some(target) = bytes.get_mut(24..child_end) {
        target.copy_from_slice(record.child.as_bytes());
    }
    bytes
}

fn decode_socket_record(
    expected_role: ResourceRole,
    bytes: &[u8; SOCKET_RECORD_BYTES],
) -> Result<SocketOwnershipRecord, RuntimeError> {
    if bytes.get(0..4) != Some(SOCKET_RECORD_MAGIC.as_slice())
        || bytes.get(4..6) != Some(SOCKET_RECORD_VERSION.to_be_bytes().as_slice())
        || bytes.get(6).copied() != Some(expected_role as u8)
    {
        return Err(RuntimeError::InvalidEntry);
    }
    let child_length = usize::from(*bytes.get(7).ok_or(RuntimeError::InvalidEntry)?);
    let child_end = 24_usize
        .checked_add(child_length)
        .filter(|end| *end <= 88)
        .ok_or(RuntimeError::InvalidEntry)?;
    if bytes
        .get(child_end..)
        .ok_or(RuntimeError::InvalidEntry)?
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(RuntimeError::InvalidEntry);
    }
    let child = std::str::from_utf8(bytes.get(24..child_end).ok_or(RuntimeError::InvalidEntry)?)
        .map_err(|_| RuntimeError::InvalidEntry)?;
    let device = u64::from_be_bytes(
        bytes
            .get(8..16)
            .ok_or(RuntimeError::InvalidEntry)?
            .try_into()
            .map_err(|_| RuntimeError::InvalidEntry)?,
    );
    let inode = u64::from_be_bytes(
        bytes
            .get(16..24)
            .ok_or(RuntimeError::InvalidEntry)?
            .try_into()
            .map_err(|_| RuntimeError::InvalidEntry)?,
    );
    SocketOwnershipRecord::new(
        expected_role,
        SocketChild::parse(child).map_err(|_| RuntimeError::InvalidEntry)?,
        ObjectIdentity { device, inode },
    )
}

fn encode_snapshot_record(record: &SnapshotStagingOwnershipRecord) -> [u8; SNAPSHOT_RECORD_BYTES] {
    let mut bytes = [0_u8; SNAPSHOT_RECORD_BYTES];
    bytes[0..4].copy_from_slice(&SNAPSHOT_RECORD_MAGIC);
    bytes[4..6].copy_from_slice(&SNAPSHOT_RECORD_VERSION.to_be_bytes());
    bytes[6] = record.kind.protocol_byte();
    bytes[7] = u8::try_from(record.name.as_bytes().len()).unwrap_or(0);
    bytes[8..16].copy_from_slice(&record.directory_identity.device.to_be_bytes());
    bytes[16..24].copy_from_slice(&record.directory_identity.inode.to_be_bytes());
    bytes[24..32].copy_from_slice(&record.file_identity.device.to_be_bytes());
    bytes[32..40].copy_from_slice(&record.file_identity.inode.to_be_bytes());
    let name_end = 40 + record.name.as_bytes().len();
    if let Some(target) = bytes.get_mut(40..name_end) {
        target.copy_from_slice(record.name.as_bytes());
    }
    bytes
}

fn decode_snapshot_record(
    expected_kind: SnapshotStagingKind,
    bytes: &[u8; SNAPSHOT_RECORD_BYTES],
) -> Result<SnapshotStagingOwnershipRecord, RuntimeError> {
    if bytes.get(0..4) != Some(SNAPSHOT_RECORD_MAGIC.as_slice())
        || bytes.get(4..6) != Some(SNAPSHOT_RECORD_VERSION.to_be_bytes().as_slice())
        || SnapshotStagingKind::from_protocol_byte(
            bytes.get(6).copied().ok_or(RuntimeError::InvalidEntry)?,
        )? != expected_kind
    {
        return Err(RuntimeError::InvalidEntry);
    }
    let name_length = usize::from(*bytes.get(7).ok_or(RuntimeError::InvalidEntry)?);
    let name_end = 40_usize
        .checked_add(name_length)
        .filter(|end| *end <= SNAPSHOT_RECORD_BYTES)
        .ok_or(RuntimeError::InvalidEntry)?;
    if bytes
        .get(name_end..)
        .ok_or(RuntimeError::InvalidEntry)?
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(RuntimeError::InvalidEntry);
    }
    let name = std::str::from_utf8(bytes.get(40..name_end).ok_or(RuntimeError::InvalidEntry)?)
        .map_err(|_| RuntimeError::InvalidEntry)?;
    let read_u64 = |range: std::ops::Range<usize>| -> Result<u64, RuntimeError> {
        Ok(u64::from_be_bytes(
            bytes
                .get(range)
                .ok_or(RuntimeError::InvalidEntry)?
                .try_into()
                .map_err(|_| RuntimeError::InvalidEntry)?,
        ))
    };
    Ok(SnapshotStagingOwnershipRecord::new(
        expected_kind,
        ObjectIdentity {
            device: read_u64(8..16)?,
            inode: read_u64(16..24)?,
        },
        SnapshotStagingName::parse(expected_kind, name)?,
        ObjectIdentity {
            device: read_u64(24..32)?,
            inode: read_u64(32..40)?,
        },
    ))
}

fn write_socket_record(
    directory: RawFd,
    record: &SocketOwnershipRecord,
) -> Result<(), RuntimeError> {
    let name = socket_record_name(record.role)?;
    // SAFETY: `directory` and `name` are live; success returns a fresh record fd.
    let fd = unsafe {
        libc::openat(
            directory,
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    let mut file = File::from(owned_fd(fd)?);
    let bytes = encode_socket_record(record);
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        // SAFETY: The fixed name is NUL-terminated and relative to the live namespace.
        let _ = unsafe { libc::unlinkat(directory, name.as_ptr(), 0) };
        return Err(RuntimeError::Filesystem(error.kind()));
    }
    // SAFETY: `directory` is live and fsync has no pointer contract.
    if unsafe { libc::fsync(directory) } != 0 {
        // SAFETY: Same fixed private record cleanup after failed durability.
        let _ = unsafe { libc::unlinkat(directory, name.as_ptr(), 0) };
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
    Ok(())
}

fn read_socket_record(
    directory: RawFd,
    role: ResourceRole,
) -> Result<Option<SocketOwnershipRecord>, RuntimeError> {
    let name = socket_record_name(role)?;
    // SAFETY: `directory` and `name` are live; success returns a fresh record fd.
    let fd = unsafe {
        libc::openat(
            directory,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(RuntimeError::Filesystem(error.kind()))
        };
    }
    let mut file = File::from(owned_fd(fd)?);
    let metadata = file
        .metadata()
        .map_err(|error| RuntimeError::Filesystem(error.kind()))?;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // SAFETY: Identity call has no pointer or ownership contract.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o7777 != 0o600
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.len() != u64::try_from(SOCKET_RECORD_BYTES).unwrap_or(u64::MAX)
    {
        return Err(RuntimeError::InvalidEntry);
    }
    let mut bytes = [0_u8; SOCKET_RECORD_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|error| RuntimeError::Filesystem(error.kind()))?;
    decode_socket_record(role, &bytes).map(Some)
}

fn clear_socket_record(
    directory: RawFd,
    expected: &SocketOwnershipRecord,
) -> Result<(), RuntimeError> {
    match read_socket_record(directory, expected.role)? {
        None => return Ok(()),
        Some(actual) if actual == *expected => {}
        Some(_) => return Err(RuntimeError::InvalidEntry),
    }
    let name = socket_record_name(expected.role)?;
    // SAFETY: `directory` and the fixed record name remain live for unlinkat.
    if unsafe { libc::unlinkat(directory, name.as_ptr(), 0) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::NotFound {
            return Err(RuntimeError::Filesystem(error.kind()));
        }
    }
    Ok(())
}

fn write_snapshot_record(
    directory: RawFd,
    record: &SnapshotStagingOwnershipRecord,
) -> Result<(), RuntimeError> {
    let name = snapshot_record_name(record.kind);
    // SAFETY: the namespace descriptor and fixed record name remain live.
    let fd = unsafe {
        libc::openat(
            directory,
            name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    let mut file = File::from(owned_fd(fd)?);
    let bytes = encode_snapshot_record(record);
    let result = file.write_all(&bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = result {
        // SAFETY: the fixed record name is relative to the live namespace.
        let _ = unsafe { libc::unlinkat(directory, name.as_ptr(), 0) };
        return Err(RuntimeError::Filesystem(error.kind()));
    }
    // SAFETY: fsync has no pointer contract and the directory remains live.
    if unsafe { libc::fsync(directory) } != 0 {
        // SAFETY: same fixed record cleanup after failed durability.
        let _ = unsafe { libc::unlinkat(directory, name.as_ptr(), 0) };
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
    Ok(())
}

fn read_snapshot_record(
    directory: RawFd,
    kind: SnapshotStagingKind,
) -> Result<Option<SnapshotStagingOwnershipRecord>, RuntimeError> {
    let name = snapshot_record_name(kind);
    // SAFETY: the namespace descriptor and fixed record name remain live.
    let fd = unsafe {
        libc::openat(
            directory,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(RuntimeError::Filesystem(error.kind()))
        };
    }
    let mut file = File::from(owned_fd(fd)?);
    let metadata = file
        .metadata()
        .map_err(|error| RuntimeError::Filesystem(error.kind()))?;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // SAFETY: identity lookup has no pointer or ownership contract.
    let uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o7777 != 0o600
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.len() != u64::try_from(SNAPSHOT_RECORD_BYTES).unwrap_or(u64::MAX)
    {
        return Err(RuntimeError::InvalidEntry);
    }
    let mut bytes = [0_u8; SNAPSHOT_RECORD_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|error| RuntimeError::Filesystem(error.kind()))?;
    decode_snapshot_record(kind, &bytes).map(Some)
}

fn clear_snapshot_record(
    directory: RawFd,
    expected: &SnapshotStagingOwnershipRecord,
) -> Result<(), RuntimeError> {
    match read_snapshot_record(directory, expected.kind)? {
        None => return Ok(()),
        Some(actual) if actual == *expected => {}
        Some(_) => return Err(RuntimeError::InvalidEntry),
    }
    let name = snapshot_record_name(expected.kind);
    // SAFETY: the namespace descriptor and fixed record name remain live.
    if unsafe { libc::unlinkat(directory, name.as_ptr(), 0) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::NotFound {
            return Err(RuntimeError::Filesystem(error.kind()));
        }
    }
    // SAFETY: fsync durably orders record removal in the private namespace.
    if unsafe { libc::fsync(directory) } != 0 {
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
    Ok(())
}

fn unlink_staged_socket(
    directory: RawFd,
    record: &SocketOwnershipRecord,
) -> Result<(), RuntimeError> {
    let name = socket_staging_name(record.role)?;
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The directory and fixed staging name remain live, and `stat`
    // provides writable storage for the synchronous metadata result.
    if unsafe {
        libc::fstatat(
            directory,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(RuntimeError::Filesystem(error.kind()))
        };
    }
    // SAFETY: Successful `fstatat` initialized the complete result.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: `geteuid` has no pointer or ownership contract.
    let uid = unsafe { libc::geteuid() };
    let identity = ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK
        || stat.st_mode & 0o7777 != 0o600
        || stat.st_uid != uid
        || stat.st_nlink != 1
        || identity != record.identity()
    {
        return Err(RuntimeError::InvalidEntry);
    }
    // SAFETY: The locked namespace and fixed name identify the exact validated
    // staging socket. The owned worker has already been reaped.
    if unsafe { libc::unlinkat(directory, name.as_ptr(), 0) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::NotFound {
            return Err(RuntimeError::Filesystem(error.kind()));
        }
    }
    Ok(())
}

fn validate_directory(fd: RawFd) -> Result<NamespaceIdentity, RuntimeError> {
    validate_directory_owned(fd, DirectoryOwner::current_user())
}

fn validate_directory_owned(
    fd: RawFd,
    owner: DirectoryOwner,
) -> Result<NamespaceIdentity, RuntimeError> {
    validate_directory_stat(directory_stat(fd)?, owner)
}

fn validate_linked_directory_owned(
    fd: RawFd,
    owner: DirectoryOwner,
) -> Result<NamespaceIdentity, RuntimeError> {
    let stat = directory_stat(fd)?;
    if stat.st_nlink < 2 {
        return Err(RuntimeError::InvalidEntry);
    }
    validate_directory_stat(stat, owner)
}

fn validate_preopened_namespace(
    root: RawFd,
    directory: RawFd,
    name: &CStr,
    expected: NamespaceIdentity,
    owner: DirectoryOwner,
    require_empty: bool,
) -> Result<(), RuntimeError> {
    validate_linked_directory_owned(root, owner)?;
    if validate_linked_directory_owned(directory, owner)? != expected
        || identity_at(root, name)? != Some(expected)
        || (require_empty && !directory_is_empty(directory)?)
    {
        return Err(RuntimeError::InvalidEntry);
    }
    Ok(())
}

#[cfg(feature = "elevated-bootstrap-probe")]
fn validate_retired_namespace(
    root: RawFd,
    directory: RawFd,
    name: &CStr,
    expected: NamespaceIdentity,
    owner: DirectoryOwner,
    require_empty: bool,
) -> Result<(), RuntimeError> {
    validate_linked_directory_owned(root, owner)?;
    if validate_directory_owned(directory, owner)? != expected
        || identity_at(root, name)?.is_some()
        || (require_empty && !directory_is_empty(directory)?)
    {
        return Err(RuntimeError::InvalidEntry);
    }
    Ok(())
}

#[cfg(feature = "elevated-bootstrap-probe")]
fn cleanup_created_session_after_error(
    root: RawFd,
    name: &CStr,
    owner: DirectoryOwner,
    error: RuntimeError,
) -> RuntimeError {
    match openat_directory(root, name) {
        Ok(directory) => {
            cleanup_open_session_after_error(root, directory.as_raw_fd(), name, owner, error)
        }
        Err(cleanup_error) => cleanup_error,
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
fn cleanup_open_session_after_error(
    root: RawFd,
    directory: RawFd,
    name: &CStr,
    owner: DirectoryOwner,
    error: RuntimeError,
) -> RuntimeError {
    match validate_linked_directory_owned(directory, owner)
        .and_then(|identity| cleanup_exact(root, directory, name, identity, owner))
    {
        Ok(()) => error,
        Err(cleanup_error) => cleanup_error,
    }
}

fn directory_stat(fd: RawFd) -> Result<libc::stat, RuntimeError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is writable for one result and `fd` remains owned by caller.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
    // SAFETY: Successful `fstat` initialized the result.
    Ok(unsafe { stat.assume_init() })
}

fn current_directory_identity(owner: DirectoryOwner) -> Result<NamespaceIdentity, RuntimeError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: The fixed dot component is NUL-terminated, `AT_FDCWD` names the
    // process cwd, and `stat` is writable for one result.
    if unsafe {
        libc::fstatat(
            libc::AT_FDCWD,
            c".".as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
    // SAFETY: Successful `fstatat` initialized the result.
    validate_directory_stat(unsafe { stat.assume_init() }, owner)
}

fn validate_directory_stat(
    stat: libc::stat,
    owner: DirectoryOwner,
) -> Result<NamespaceIdentity, RuntimeError> {
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_mode & 0o7777 != 0o700
        || stat.st_uid != owner.uid
        || owner.gid.is_some_and(|gid| stat.st_gid != gid)
    {
        return Err(RuntimeError::InvalidEntry);
    }
    Ok(NamespaceIdentity {
        device: u64::try_from(stat.st_dev).map_err(|_| RuntimeError::InvalidEntry)?,
        inode: stat.st_ino,
    })
}

fn session_name(session: SessionId) -> Result<CString, RuntimeError> {
    CString::new(format!("{SESSION_PREFIX}{}", session.private_hex()))
        .map_err(|_| RuntimeError::InvalidEntry)
}

fn valid_session_name(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    let Some(suffix) = bytes.strip_prefix(SESSION_PREFIX.as_bytes()) else {
        return false;
    };
    bytes.len() == SESSION_NAME_BYTES
        && suffix.iter().all(u8::is_ascii_hexdigit)
        && suffix.iter().all(|byte| !byte.is_ascii_uppercase())
}

fn recover_stale_entries(root: RawFd, owner: DirectoryOwner) -> Result<(), RuntimeError> {
    for entry in directory_entries(root, MAX_RECOVERY_ENTRIES)? {
        if !valid_session_name(&entry) {
            continue;
        }
        let Ok(name) = cstring(&entry) else {
            continue;
        };
        let Ok(directory) = openat_directory(root, &name) else {
            continue;
        };
        let Ok(identity) = validate_directory_owned(directory.as_raw_fd(), owner) else {
            continue;
        };
        if !try_lock_exclusive(directory.as_raw_fd())? {
            continue;
        }
        if !directory_is_empty(directory.as_raw_fd())? {
            continue;
        }
        let _ = cleanup_exact(root, directory.as_raw_fd(), &name, identity, owner);
    }
    Ok(())
}

fn directory_entries(fd: RawFd, limit: usize) -> Result<Vec<OsString>, RuntimeError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    // Reopening an inherited directory through `openat(fd, ".")` performs a
    // fresh path lookup that App Sandbox can deny even though the process owns
    // the exact validated descriptor. Duplicate that authority instead and
    // explicitly rewind its shared directory cursor before consuming it.
    let independent = duplicate_fd(fd)?.into_raw_fd();
    // SAFETY: `independent` is a fresh descriptor; `fdopendir` takes ownership on success.
    let directory = unsafe { libc::fdopendir(independent) };
    if directory.is_null() {
        // SAFETY: `fdopendir` failed and did not consume `independent`.
        let _ = unsafe { libc::close(independent) };
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
    // SAFETY: `directory` is live and uniquely consumed by this bounded scan.
    unsafe { libc::rewinddir(directory) };
    let mut entries = Vec::new();
    loop {
        // SAFETY: Darwin's thread-local errno pointer is writable for this call sequence.
        unsafe { *libc::__error() = 0 };
        // SAFETY: `directory` remains a live DIR until `closedir` below.
        let entry = unsafe { libc::readdir(directory) };
        if entry.is_null() {
            // SAFETY: Reading thread-local errno after `readdir` is valid.
            let errno = unsafe { *libc::__error() };
            // SAFETY: `directory` is live and consumed exactly once.
            let close_result = unsafe { libc::closedir(directory) };
            if errno != 0 {
                return Err(RuntimeError::Filesystem(
                    io::Error::from_raw_os_error(errno).kind(),
                ));
            }
            if close_result != 0 {
                return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
            }
            return Ok(entries);
        }
        // SAFETY: `readdir` returned a live entry whose `d_name` is NUL-terminated.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            entries.push(OsString::from_vec(name.to_bytes().to_vec()));
            if entries.len() == limit {
                // SAFETY: `directory` is live and consumed exactly once.
                if unsafe { libc::closedir(directory) } != 0 {
                    return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
                }
                return Ok(entries);
            }
        }
    }
}

fn directory_is_empty(fd: RawFd) -> Result<bool, RuntimeError> {
    Ok(directory_entries(fd, 1)?.is_empty())
}

fn directory_contains_only_ownership_records(fd: RawFd) -> Result<bool, RuntimeError> {
    let mut expected = Vec::with_capacity(4);
    for role in [
        ResourceRole::ApiSocketDirectory,
        ResourceRole::VsockSocketDirectory,
    ] {
        if read_socket_record(fd, role)?.is_some() {
            expected.push(socket_record_name(role)?.to_bytes());
        }
    }
    for kind in [SnapshotStagingKind::State, SnapshotStagingKind::Memory] {
        if read_snapshot_record(fd, kind)?.is_some() {
            expected.push(snapshot_record_name(kind).to_bytes());
        }
    }
    let entries = directory_entries(fd, 5)?;
    Ok(entries.len() == expected.len()
        && entries.iter().all(|entry| {
            expected
                .iter()
                .any(|expected| entry.as_os_str().as_bytes() == *expected)
        }))
}

fn cleanup_exact(
    root: RawFd,
    directory: RawFd,
    name: &CStr,
    expected: NamespaceIdentity,
    owner: DirectoryOwner,
) -> Result<(), RuntimeError> {
    if validate_directory_owned(directory, owner)? != expected || !directory_is_empty(directory)? {
        return Err(RuntimeError::InvalidEntry);
    }
    match identity_at(root, name)? {
        Some(actual) if actual == expected => {}
        None => return Ok(()),
        Some(_) => return Err(RuntimeError::InvalidEntry),
    }
    // SAFETY: `root` and `name` identify the exact checked empty directory; no
    // pointer is retained.
    if unsafe { libc::unlinkat(root, name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::NotFound {
            return Err(RuntimeError::Filesystem(error.kind()));
        }
    }
    Ok(())
}

fn identity_at(root: RawFd, name: &CStr) -> Result<Option<NamespaceIdentity>, RuntimeError> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is writable, the path is NUL-terminated, and no pointer is retained.
    if unsafe {
        libc::fstatat(
            root,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(None)
        } else {
            Err(RuntimeError::Filesystem(error.kind()))
        };
    }
    // SAFETY: Successful `fstatat` initialized the result.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(RuntimeError::InvalidEntry);
    }
    Ok(Some(NamespaceIdentity {
        device: u64::try_from(stat.st_dev).map_err(|_| RuntimeError::InvalidEntry)?,
        inode: stat.st_ino,
    }))
}

fn path_missing(root: RawFd, name: &CStr) -> Result<bool, RuntimeError> {
    Ok(identity_at(root, name)?.is_none())
}

fn try_lock_exclusive(fd: RawFd) -> Result<bool, RuntimeError> {
    // SAFETY: `fd` remains owned by the caller; `flock` changes only its advisory lock.
    if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error
        .raw_os_error()
        .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
    {
        Ok(false)
    } else {
        Err(RuntimeError::Filesystem(error.kind()))
    }
}

fn lock_exclusive(fd: RawFd) -> Result<(), RuntimeError> {
    if try_lock_exclusive(fd)? {
        Ok(())
    } else {
        Err(RuntimeError::InvalidEntry)
    }
}

fn unlock(fd: RawFd) {
    // SAFETY: `fd` remains owned by the caller; failure only leaves the lock to
    // be released automatically when its descriptor closes.
    let _ = unsafe { libc::flock(fd, libc::LOCK_UN) };
}

fn cstring(value: &OsStr) -> Result<CString, std::ffi::NulError> {
    CString::new(value.as_bytes())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEST_ROOT: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new() -> Self {
            loop {
                let id = NEXT_TEST_ROOT.fetch_add(1, Ordering::SeqCst);
                let path = std::env::temp_dir().join(format!(
                    "bangbang-session-runtime-{}-{id}",
                    std::process::id()
                ));
                match fs::DirBuilder::new().mode(0o700).create(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("test root should be created: {error}"),
                }
            }
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    fn explicit_test_root(root: &TestRoot) -> ExplicitRuntimeRoot {
        let metadata = fs::symlink_metadata(root.path()).expect("root metadata should read");
        ExplicitRuntimeRoot::from_owned_fd(
            open_directory(root.path()).expect("root should open"),
            ObjectIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            metadata.uid(),
            metadata.gid(),
            true,
        )
        .expect("explicit root should validate")
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("test root should be removed");
        }
    }

    #[test]
    fn exact_session_names_are_lowercase_and_fixed_length() {
        let session = SessionId::from_bytes([0xab; 32]);
        let name = session_name(session).expect("name should derive");
        assert_eq!(name.as_bytes().len(), SESSION_NAME_BYTES);
        assert!(valid_session_name(OsStr::from_bytes(name.as_bytes())));
        assert!(!valid_session_name(OsStr::new("session-AB")));
        assert!(!valid_session_name(OsStr::new("unrelated")));

        let identity = NamespaceIdentity {
            device: 1_234_567_891,
            inode: 1_234_567_893,
        };
        assert_eq!(format!("{identity:?}"), "NamespaceIdentity(<redacted>)");
    }

    #[test]
    fn current_user_paths_follow_fixed_container_contract() {
        let home = user_home().expect("user home should resolve");
        assert_eq!(
            launcher_runtime_root().expect("launcher root should derive"),
            home.join(WORKER_CONTAINER_SUFFIX).join(RUNTIME_ROOT_NAME)
        );
    }

    #[test]
    fn directory_validation_rejects_special_mode_bits() {
        let root = TestRoot::new();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o1700))
            .expect("test permissions should change");
        let directory = open_directory(root.path()).expect("test root should open");
        assert_eq!(
            validate_directory(directory.as_raw_fd()),
            Err(RuntimeError::InvalidEntry)
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn explicit_roots_require_exact_owner_and_independent_session_locks() {
        let root = TestRoot::new();
        let metadata = fs::symlink_metadata(root.path()).expect("root metadata should read");
        let expected = ObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        };
        let uid = metadata.uid();
        let gid = metadata.gid();

        assert_eq!(
            ExplicitRuntimeRoot::from_owned_fd(
                open_directory(root.path()).expect("root should open"),
                expected,
                uid,
                gid.wrapping_add(1),
                true,
            )
            .expect_err("wrong gid must be rejected"),
            RuntimeError::InvalidEntry
        );

        let unexpected = root.path().join("unexpected");
        fs::write(&unexpected, b"occupied").expect("unexpected root entry should write");
        assert_eq!(
            ExplicitRuntimeRoot::from_owned_fd(
                open_directory(root.path()).expect("root should open"),
                expected,
                uid,
                gid,
                true,
            )
            .expect_err("an explicit runtime root must be empty before handoff"),
            RuntimeError::InvalidRoot
        );
        fs::remove_file(unexpected).expect("unexpected root entry should clean");

        let root_authority = ExplicitRuntimeRoot::from_owned_fd(
            open_directory(root.path()).expect("root should open"),
            expected,
            uid,
            gid,
            true,
        )
        .expect("exact root should validate");
        let worker_root = root_authority
            .try_reopen(true)
            .expect("worker root should independently reopen");
        let launcher_root = root_authority
            .try_reopen(true)
            .expect("launcher root should independently reopen");
        drop(root_authority);

        let session = SessionId::from_bytes([0x91; 32]);
        let worker = WorkerNamespace::create_from_explicit_root(worker_root, session)
            .expect("target-owned session should create and lock");
        let identity = worker.identity();
        let mut launcher =
            LauncherNamespace::validate_from_explicit_root(launcher_root, session, identity)
                .expect("independent launcher description should observe the worker lock");
        drop(worker);
        launcher
            .cleanup()
            .expect("launcher cleanup should accept worker removal");
        assert!(
            directory_is_empty(
                open_directory(root.path())
                    .expect("root should reopen")
                    .as_raw_fd()
            )
            .expect("root should inspect")
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn launcher_created_session_is_adopted_locked_validated_and_cleaned() {
        let root = TestRoot::new();
        let worker_root = explicit_test_root(&root);
        let session = SessionId::from_bytes([0x92; 32]);
        let prepared = PreparedLauncherSession::create(explicit_test_root(&root), session)
            .expect("launcher session should prepare");
        let identity = prepared.identity();
        assert_eq!(
            format!("{prepared:?}"),
            "PreparedLauncherSession(<redacted>)"
        );
        let (transfer, mut handles) = prepared.into_publication();
        assert_eq!(handles.session(), session);
        assert_eq!(handles.identity(), identity);
        assert_eq!(format!("{handles:?}"), "LauncherSessionHandles(<redacted>)");

        let validated = ValidatedWorkerNamespace::from_explicit_root(
            worker_root,
            transfer,
            session,
            identity.object_identity(),
        )
        .expect("transferred session should validate");
        assert_eq!(
            format!("{validated:?}"),
            "ValidatedWorkerNamespace(<redacted>)"
        );
        let mut worker = validated.lock().expect("worker should acquire its lock");
        let mut launcher = handles
            .validate_live(session, identity)
            .expect("independent launcher description should observe the lock");

        // Model an abrupt worker exit: close its descriptions without running
        // the worker-owned best-effort removal, then let the launcher clean.
        worker.cleaned = true;
        drop(worker);
        launcher
            .cleanup()
            .expect("launcher should remove the exact unlocked session");
        drop(handles);
        assert!(
            directory_is_empty(
                open_directory(root.path())
                    .expect("root should reopen")
                    .as_raw_fd()
            )
            .expect("root should inspect")
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn live_locked_session_retires_once_and_finishes_without_a_linked_name() {
        let root = TestRoot::new();
        let session = SessionId::from_bytes([0xa1; 32]);
        let worker_root = explicit_test_root(&root);
        let prepared = PreparedLauncherSession::create(explicit_test_root(&root), session)
            .expect("launcher session should prepare");
        let identity = prepared.identity();
        let (transfer, mut handles) = prepared.into_publication();
        let mut worker = WorkerNamespace::adopt_from_explicit_root(
            worker_root,
            transfer,
            session,
            identity.object_identity(),
        )
        .expect("worker should adopt and lock the session");
        let mut launcher = handles
            .validate_live(session, identity)
            .expect("launcher should validate the live lock");

        launcher
            .retire_linked()
            .expect("launcher should retire the exact empty name");
        assert_eq!(launcher.retire_linked(), Err(RuntimeError::InvalidEntry));
        launcher
            .verify_worker_lock()
            .expect("retired handle should retain the live worker-lock proof");
        assert_eq!(
            worker.observe_retired_with_current_directory(NamespaceIdentity {
                device: identity.device,
                inode: identity.inode.wrapping_add(1),
            }),
            Err(RuntimeError::InvalidEntry),
            "wrong cwd identity must not advance worker state"
        );
        worker
            .observe_retired_with_current_directory(identity)
            .expect("worker should observe the exact retired inode");
        assert_eq!(
            worker.observe_retired_with_current_directory(identity),
            Err(RuntimeError::InvalidEntry),
            "worker retirement observation is single-use"
        );
        let name = session_name(session).expect("name should derive");
        assert!(
            !root
                .path()
                .join(OsStr::from_bytes(name.as_bytes()))
                .exists(),
            "retirement must remove the canonical name"
        );
        assert_eq!(launcher.cleanup(), Err(RuntimeError::InvalidEntry));

        worker.cleaned = true;
        drop(worker);
        launcher
            .cleanup()
            .expect("retired cleanup should validate absence after lock release");
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn retirement_rejects_missing_populated_unlocked_and_replaced_names() {
        let missing_root = TestRoot::new();
        let missing_session = SessionId::from_bytes([0xa2; 32]);
        let worker_root = explicit_test_root(&missing_root);
        let prepared =
            PreparedLauncherSession::create(explicit_test_root(&missing_root), missing_session)
                .expect("missing fixture should prepare");
        let identity = prepared.identity();
        let (transfer, mut handles) = prepared.into_publication();
        let mut worker = WorkerNamespace::adopt_from_explicit_root(
            worker_root,
            transfer,
            missing_session,
            identity.object_identity(),
        )
        .expect("missing fixture worker should lock");
        let mut launcher = handles
            .validate_live(missing_session, identity)
            .expect("missing fixture launcher should validate");
        let missing_name = session_name(missing_session).expect("name should derive");
        fs::remove_dir(
            missing_root
                .path()
                .join(OsStr::from_bytes(missing_name.as_bytes())),
        )
        .expect("fixture should remove the linked name");
        assert_eq!(
            launcher.retire_linked(),
            Err(RuntimeError::InvalidEntry),
            "an already-missing name is not a successful retirement"
        );
        worker.cleaned = true;
        launcher.cleaned = true;
        drop(worker);
        drop(launcher);

        let populated_root = TestRoot::new();
        let populated_session = SessionId::from_bytes([0xa3; 32]);
        let populated_worker_root = explicit_test_root(&populated_root);
        let prepared =
            PreparedLauncherSession::create(explicit_test_root(&populated_root), populated_session)
                .expect("populated fixture should prepare");
        let identity = prepared.identity();
        let (transfer, mut handles) = prepared.into_publication();
        let mut worker = WorkerNamespace::adopt_from_explicit_root(
            populated_worker_root,
            transfer,
            populated_session,
            identity.object_identity(),
        )
        .expect("populated fixture worker should lock");
        let mut launcher = handles
            .validate_live(populated_session, identity)
            .expect("populated fixture launcher should validate");
        fs::write(
            populated_root
                .path()
                .join(OsStr::from_bytes(
                    session_name(populated_session)
                        .expect("name should derive")
                        .as_bytes(),
                ))
                .join("unexpected"),
            b"preserve",
        )
        .expect("unexpected entry should write");
        assert_eq!(launcher.retire_linked(), Err(RuntimeError::InvalidEntry));
        worker.cleaned = true;
        launcher.cleaned = true;
        drop(worker);
        drop(launcher);

        let unlocked_root = TestRoot::new();
        let unlocked_session = SessionId::from_bytes([0xa4; 32]);
        let unlocked_worker_root = explicit_test_root(&unlocked_root);
        let prepared =
            PreparedLauncherSession::create(explicit_test_root(&unlocked_root), unlocked_session)
                .expect("unlocked fixture should prepare");
        let identity = prepared.identity();
        let (transfer, mut handles) = prepared.into_publication();
        let mut worker = WorkerNamespace::adopt_from_explicit_root(
            unlocked_worker_root,
            transfer,
            unlocked_session,
            identity.object_identity(),
        )
        .expect("unlocked fixture worker should lock");
        let mut launcher = handles
            .validate_live(unlocked_session, identity)
            .expect("unlocked fixture launcher should validate");
        worker.cleaned = true;
        drop(worker);
        assert_eq!(launcher.retire_linked(), Err(RuntimeError::InvalidEntry));

        let replacement_root = TestRoot::new();
        let replacement_session = SessionId::from_bytes([0xa5; 32]);
        let replacement_worker_root = explicit_test_root(&replacement_root);
        let prepared = PreparedLauncherSession::create(
            explicit_test_root(&replacement_root),
            replacement_session,
        )
        .expect("replacement fixture should prepare");
        let identity = prepared.identity();
        let (transfer, mut handles) = prepared.into_publication();
        let mut worker = WorkerNamespace::adopt_from_explicit_root(
            replacement_worker_root,
            transfer,
            replacement_session,
            identity.object_identity(),
        )
        .expect("replacement fixture worker should lock");
        let mut launcher = handles
            .validate_live(replacement_session, identity)
            .expect("replacement fixture launcher should validate");
        launcher
            .retire_linked()
            .expect("replacement fixture should retire");
        let replacement_name = session_name(replacement_session).expect("name should derive");
        let replacement_path = replacement_root
            .path()
            .join(OsStr::from_bytes(replacement_name.as_bytes()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&replacement_path)
            .expect("replacement name should create");
        assert_eq!(
            worker.observe_retired_with_current_directory(identity),
            Err(RuntimeError::InvalidEntry),
            "worker must reject a canonical-name replacement"
        );
        worker.cleaned = true;
        drop(worker);
        assert_eq!(launcher.cleanup(), Err(RuntimeError::InvalidEntry));
        launcher.cleaned = true;
        drop(launcher);
        assert!(
            replacement_path.is_dir(),
            "replacement must remain untouched"
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn retired_record_policy_rejects_every_record_before_a_syscall() {
        let root = TestRoot::new();
        let session = SessionId::from_bytes([0xa6; 32]);
        let worker = WorkerNamespace::create_from_explicit_root(explicit_test_root(&root), session)
            .expect("record-free fixture should create");
        let namespace = worker
            .socket_namespace_with_policy(NamespaceRecordPolicy::RetiredRecordFree)
            .expect("record-free namespace should duplicate");
        let socket = SocketOwnershipRecord::new(
            ResourceRole::ApiSocketDirectory,
            SocketChild::parse("api.sock").expect("socket child should parse"),
            ObjectIdentity {
                device: 11,
                inode: 13,
            },
        )
        .expect("socket record should construct");
        let snapshot = SnapshotStagingOwnershipRecord::new(
            SnapshotStagingKind::State,
            ObjectIdentity {
                device: 17,
                inode: 19,
            },
            SnapshotStagingName::parse(
                SnapshotStagingKind::State,
                ".bangbang-snapshot-state-0123456789abcdef0123456789abcdef",
            )
            .expect("snapshot name should parse"),
            ObjectIdentity {
                device: 23,
                inode: 29,
            },
        );

        assert_eq!(
            namespace.write_socket_record(&socket),
            Err(RuntimeError::InvalidEntry)
        );
        assert_eq!(
            namespace.socket_record(ResourceRole::ApiSocketDirectory),
            Err(RuntimeError::InvalidEntry)
        );
        assert_eq!(
            namespace.require_socket_record(&socket),
            Err(RuntimeError::InvalidEntry)
        );
        assert_eq!(
            namespace.unlink_staged_socket(&socket),
            Err(RuntimeError::InvalidEntry)
        );
        assert_eq!(
            namespace.clear_socket_record(&socket),
            Err(RuntimeError::InvalidEntry)
        );
        assert_eq!(
            namespace.write_snapshot_staging_record(&snapshot),
            Err(RuntimeError::InvalidEntry)
        );
        assert_eq!(
            namespace.clear_snapshot_staging_record(&snapshot),
            Err(RuntimeError::InvalidEntry)
        );
        assert!(
            directory_is_empty(namespace.anchor_fd()).expect("namespace should inspect"),
            "record rejection must happen before filesystem mutation"
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn unpublished_and_post_exit_recovery_use_only_preopened_handles() {
        let root = TestRoot::new();
        let unpublished_session = SessionId::from_bytes([0x93; 32]);
        PreparedLauncherSession::create(explicit_test_root(&root), unpublished_session)
            .expect("unpublished session should prepare")
            .cleanup_unpublished()
            .expect("unpublished session should clean");

        let session = SessionId::from_bytes([0x94; 32]);
        let worker_root = explicit_test_root(&root);
        let prepared = PreparedLauncherSession::create(explicit_test_root(&root), session)
            .expect("published session should prepare");
        let identity = prepared.identity();
        let (transfer, mut handles) = prepared.into_publication();
        let mut worker = WorkerNamespace::adopt_from_explicit_root(
            worker_root,
            transfer,
            session,
            identity.object_identity(),
        )
        .expect("worker should adopt the exact session");
        worker.cleaned = true;
        drop(worker);
        handles.discard_validation();
        let mut recovered = handles
            .recover_after_worker_exit(session)
            .expect("recovery should validate")
            .expect("session should remain after abrupt exit");
        recovered
            .cleanup()
            .expect("recovery handle should remove exact session");
        assert!(
            directory_is_empty(
                open_directory(root.path())
                    .expect("root should reopen")
                    .as_raw_fd()
            )
            .expect("root should inspect")
        );
    }

    #[cfg(feature = "elevated-bootstrap-probe")]
    #[test]
    fn adopted_session_rejects_wrong_identity_content_and_replacement() {
        let wrong_root = TestRoot::new();
        let session = SessionId::from_bytes([0x95; 32]);
        let wrong_worker_root = explicit_test_root(&wrong_root);
        let prepared = PreparedLauncherSession::create(explicit_test_root(&wrong_root), session)
            .expect("session should prepare");
        let identity = prepared.identity();
        let (transfer, _handles) = prepared.into_publication();
        assert_eq!(
            ValidatedWorkerNamespace::from_explicit_root(
                wrong_worker_root,
                transfer,
                session,
                ObjectIdentity {
                    device: identity.device,
                    inode: identity.inode.wrapping_add(1),
                },
            )
            .expect_err("wrong identity must fail"),
            RuntimeError::InvalidEntry
        );

        let wrong_session_root = TestRoot::new();
        let correct_session = SessionId::from_bytes([0x98; 32]);
        let wrong_session = SessionId::from_bytes([0x99; 32]);
        let wrong_session_worker_root = explicit_test_root(&wrong_session_root);
        let prepared = PreparedLauncherSession::create(
            explicit_test_root(&wrong_session_root),
            correct_session,
        )
        .expect("wrong-session fixture should prepare");
        let identity = prepared.identity();
        let (transfer, _handles) = prepared.into_publication();
        assert_eq!(
            ValidatedWorkerNamespace::from_explicit_root(
                wrong_session_worker_root,
                transfer,
                wrong_session,
                identity.object_identity(),
            )
            .expect_err("wrong session name must fail"),
            RuntimeError::InvalidEntry
        );

        let source_root = TestRoot::new();
        let foreign_root = TestRoot::new();
        let cross_root_session = SessionId::from_bytes([0x9a; 32]);
        let prepared =
            PreparedLauncherSession::create(explicit_test_root(&source_root), cross_root_session)
                .expect("cross-root fixture should prepare");
        let identity = prepared.identity();
        let (transfer, _handles) = prepared.into_publication();
        assert_eq!(
            ValidatedWorkerNamespace::from_explicit_root(
                explicit_test_root(&foreign_root),
                transfer,
                cross_root_session,
                identity.object_identity(),
            )
            .expect_err("cross-root descriptor must fail"),
            RuntimeError::InvalidEntry
        );

        let populated_root = TestRoot::new();
        let populated_session = SessionId::from_bytes([0x96; 32]);
        let populated_worker_root = explicit_test_root(&populated_root);
        let populated =
            PreparedLauncherSession::create(explicit_test_root(&populated_root), populated_session)
                .expect("populated session should prepare");
        let populated_identity = populated.identity();
        let populated_name = session_name(populated_session).expect("name should derive");
        fs::write(
            populated_root
                .path()
                .join(OsStr::from_bytes(populated_name.as_bytes()))
                .join("unexpected"),
            b"occupied",
        )
        .expect("unexpected entry should write");
        let (populated_transfer, _populated_handles) = populated.into_publication();
        assert_eq!(
            ValidatedWorkerNamespace::from_explicit_root(
                populated_worker_root,
                populated_transfer,
                populated_session,
                populated_identity.object_identity(),
            )
            .expect_err("nonempty session must fail"),
            RuntimeError::InvalidEntry
        );

        let replacement_root = TestRoot::new();
        let replacement_session = SessionId::from_bytes([0x97; 32]);
        let replacement = PreparedLauncherSession::create(
            explicit_test_root(&replacement_root),
            replacement_session,
        )
        .expect("replacement session should prepare");
        let replacement_name = session_name(replacement_session).expect("name should derive");
        let named_path = replacement_root
            .path()
            .join(OsStr::from_bytes(replacement_name.as_bytes()));
        let moved_path = replacement_root.path().join("moved-launcher-session");
        fs::rename(&named_path, &moved_path).expect("original session should move");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&named_path)
            .expect("replacement should create");
        assert_eq!(
            replacement.cleanup_unpublished(),
            Err(RuntimeError::InvalidEntry)
        );
        assert!(named_path.is_dir(), "replacement must be preserved");
        assert!(
            moved_path.is_dir(),
            "original descriptor target must be preserved"
        );
    }

    #[test]
    fn directory_iteration_is_bounded_before_all_entries_are_loaded() {
        let root = TestRoot::new();
        for index in 0..16 {
            fs::write(root.path().join(format!("entry-{index}")), b"")
                .expect("test entry should be written");
        }
        let directory = open_directory(root.path()).expect("test root should open");
        assert_eq!(
            directory_entries(directory.as_raw_fd(), 3)
                .expect("bounded entries should read")
                .len(),
            3
        );
    }

    #[test]
    fn repeated_directory_checks_restart_from_the_beginning() {
        let root = TestRoot::new();
        let directory = open_directory(root.path()).expect("test root should open");
        assert!(directory_is_empty(directory.as_raw_fd()).expect("empty check should succeed"));

        fs::write(root.path().join("later-entry"), b"").expect("later entry should be written");
        assert!(
            !directory_is_empty(directory.as_raw_fd()).expect("second check should succeed"),
            "a repeated check must observe entries created after the first scan"
        );
    }

    #[test]
    fn socket_records_round_trip_redacted_and_clear_exactly() {
        let root = TestRoot::new();
        let directory = open_directory(root.path()).expect("test root should open");
        let api = SocketOwnershipRecord::new(
            ResourceRole::ApiSocketDirectory,
            SocketChild::parse("api.sock").expect("child should parse"),
            ObjectIdentity {
                device: 41,
                inode: 43,
            },
        )
        .expect("record should construct");
        let vsock = SocketOwnershipRecord::new(
            ResourceRole::VsockSocketDirectory,
            SocketChild::parse("vsock.sock").expect("child should parse"),
            ObjectIdentity {
                device: 47,
                inode: 53,
            },
        )
        .expect("record should construct");

        write_socket_record(directory.as_raw_fd(), &api).expect("API record should write");
        write_socket_record(directory.as_raw_fd(), &vsock).expect("vsock record should write");
        assert_eq!(
            read_socket_record(directory.as_raw_fd(), ResourceRole::ApiSocketDirectory)
                .expect("API record should read"),
            Some(api.clone())
        );
        assert_eq!(
            read_socket_record(directory.as_raw_fd(), ResourceRole::VsockSocketDirectory)
                .expect("vsock record should read"),
            Some(vsock.clone())
        );
        let debug = format!("{api:?} {vsock:?}");
        assert!(!debug.contains("api.sock") && !debug.contains("vsock.sock"));

        clear_socket_record(directory.as_raw_fd(), &api).expect("API record should clear");
        assert!(
            read_socket_record(directory.as_raw_fd(), ResourceRole::ApiSocketDirectory)
                .expect("API absence should read")
                .is_none()
        );
        assert!(
            read_socket_record(directory.as_raw_fd(), ResourceRole::VsockSocketDirectory)
                .expect("vsock record should remain")
                .is_some()
        );
        clear_socket_record(directory.as_raw_fd(), &vsock).expect("vsock record should clear");
        assert!(directory_is_empty(directory.as_raw_fd()).expect("directory should inspect"));
    }

    #[test]
    fn worker_socket_namespace_requires_one_exact_record() {
        let root = TestRoot::new();
        let namespace = WorkerSocketNamespace::from_directory_for_test(root.path())
            .expect("test namespace should validate");
        let record = SocketOwnershipRecord::new(
            ResourceRole::ApiSocketDirectory,
            SocketChild::parse("api.sock").expect("child should parse"),
            ObjectIdentity {
                device: 101,
                inode: 103,
            },
        )
        .expect("record should construct");
        let wrong = SocketOwnershipRecord::new(
            ResourceRole::ApiSocketDirectory,
            SocketChild::parse("other.sock").expect("child should parse"),
            record.identity(),
        )
        .expect("wrong record should construct");
        assert!(
            namespace
                .socket_record(ResourceRole::ApiSocketDirectory)
                .expect("record absence should read")
                .is_none()
        );
        namespace
            .write_socket_record(&record)
            .expect("record should write");
        namespace
            .require_socket_record(&record)
            .expect("exact record should match");
        assert_eq!(
            namespace.require_socket_record(&wrong),
            Err(RuntimeError::InvalidEntry)
        );
        namespace
            .clear_socket_record(&record)
            .expect("exact record should clear");
        assert!(directory_is_empty(namespace.anchor_fd()).expect("namespace should inspect"));
    }

    #[test]
    fn snapshot_staging_records_round_trip_redacted_and_clear_exactly() {
        let root = TestRoot::new();
        let directory = open_directory(root.path()).expect("test root should open");
        let state_name = SnapshotStagingName::parse(
            SnapshotStagingKind::State,
            ".bangbang-snapshot-state-0123456789abcdef0123456789abcdef",
        )
        .expect("state staging name should parse");
        let memory_name = SnapshotStagingName::parse(
            SnapshotStagingKind::Memory,
            ".bangbang-snapshot-memory-fedcba9876543210fedcba9876543210",
        )
        .expect("memory staging name should parse");
        let state = SnapshotStagingOwnershipRecord::new(
            SnapshotStagingKind::State,
            ObjectIdentity {
                device: 61,
                inode: 67,
            },
            state_name,
            ObjectIdentity {
                device: 71,
                inode: 73,
            },
        );
        let memory = SnapshotStagingOwnershipRecord::new(
            SnapshotStagingKind::Memory,
            ObjectIdentity {
                device: 79,
                inode: 83,
            },
            memory_name,
            ObjectIdentity {
                device: 89,
                inode: 97,
            },
        );

        write_snapshot_record(directory.as_raw_fd(), &state).expect("state record should write");
        write_snapshot_record(directory.as_raw_fd(), &memory).expect("memory record should write");
        assert_eq!(
            read_snapshot_record(directory.as_raw_fd(), SnapshotStagingKind::State)
                .expect("state record should read"),
            Some(state.clone())
        );
        assert_eq!(
            read_snapshot_record(directory.as_raw_fd(), SnapshotStagingKind::Memory)
                .expect("memory record should read"),
            Some(memory.clone())
        );
        assert!(
            directory_contains_only_ownership_records(directory.as_raw_fd())
                .expect("known records should validate")
        );
        let debug = format!("{state:?} {memory:?}");
        assert!(!debug.contains("012345") && !debug.contains("fedcba"));
        assert!(!debug.contains("61") && !debug.contains("97"));

        clear_snapshot_record(directory.as_raw_fd(), &state).expect("state record should clear");
        assert!(
            read_snapshot_record(directory.as_raw_fd(), SnapshotStagingKind::State)
                .expect("state absence should read")
                .is_none()
        );
        assert!(
            read_snapshot_record(directory.as_raw_fd(), SnapshotStagingKind::Memory)
                .expect("memory record should remain")
                .is_some()
        );
        clear_snapshot_record(directory.as_raw_fd(), &memory).expect("memory record should clear");
        assert!(directory_is_empty(directory.as_raw_fd()).expect("directory should inspect"));
    }

    #[test]
    fn snapshot_staging_names_reject_wrong_kind_case_length_and_components() {
        for (kind, value) in [
            (
                SnapshotStagingKind::State,
                ".bangbang-snapshot-memory-0123456789abcdef0123456789abcdef",
            ),
            (
                SnapshotStagingKind::State,
                ".bangbang-snapshot-state-0123456789abcdef",
            ),
            (
                SnapshotStagingKind::Memory,
                ".bangbang-snapshot-memory-ABCDEF0123456789abcdef0123456789",
            ),
            (
                SnapshotStagingKind::Memory,
                ".bangbang-snapshot-memory-0123456789abcdef0123456789abcdeg",
            ),
        ] {
            assert_eq!(
                SnapshotStagingName::parse(kind, value),
                Err(RuntimeError::InvalidEntry)
            );
        }
    }

    #[test]
    fn socket_records_reject_corruption_and_wrong_expected_identity() {
        let root = TestRoot::new();
        let directory = open_directory(root.path()).expect("test root should open");
        let record = SocketOwnershipRecord::new(
            ResourceRole::ApiSocketDirectory,
            SocketChild::parse("api.sock").expect("child should parse"),
            ObjectIdentity {
                device: 59,
                inode: 61,
            },
        )
        .expect("record should construct");
        write_socket_record(directory.as_raw_fd(), &record).expect("record should write");
        let wrong = SocketOwnershipRecord::new(
            ResourceRole::ApiSocketDirectory,
            SocketChild::parse("other.sock").expect("child should parse"),
            record.identity(),
        )
        .expect("wrong record should construct");
        assert_eq!(
            clear_socket_record(directory.as_raw_fd(), &wrong),
            Err(RuntimeError::InvalidEntry)
        );
        clear_socket_record(directory.as_raw_fd(), &record).expect("record should clear");

        let name =
            socket_record_name(ResourceRole::ApiSocketDirectory).expect("record name should exist");
        // SAFETY: The directory and fixed name are live; the test owns the fresh file.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                0o600,
            )
        };
        let mut file = File::from(owned_fd(fd).expect("corrupt file should open"));
        file.write_all(&[0_u8; SOCKET_RECORD_BYTES])
            .expect("corrupt bytes should write");
        drop(file);
        assert_eq!(
            read_socket_record(directory.as_raw_fd(), ResourceRole::ApiSocketDirectory),
            Err(RuntimeError::InvalidEntry)
        );
        // SAFETY: The fixed corrupt test file is owned by this fixture.
        let unlink_result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
        assert_eq!(unlink_result, 0);
    }

    #[test]
    fn staged_socket_cleanup_requires_the_recorded_identity() {
        let root = TestRoot::new();
        let directory = open_directory(root.path()).expect("test root should open");
        let staging = socket_staging_name(ResourceRole::ApiSocketDirectory)
            .expect("staging name should exist");
        let path = root.path().join(OsStr::from_bytes(staging.to_bytes()));
        let listener = UnixListener::bind(&path).expect("staging socket should bind");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("staging permissions should tighten");
        let metadata = fs::symlink_metadata(&path).expect("staging metadata should read");
        let record = SocketOwnershipRecord::new(
            ResourceRole::ApiSocketDirectory,
            SocketChild::parse("api.sock").expect("child should parse"),
            ObjectIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
        )
        .expect("record should construct");

        unlink_staged_socket(directory.as_raw_fd(), &record)
            .expect("recorded staging socket should clean");
        assert!(!path.exists());
        drop(listener);

        let replacement = UnixListener::bind(&path).expect("replacement socket should bind");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("replacement permissions should tighten");
        assert_eq!(
            unlink_staged_socket(directory.as_raw_fd(), &record),
            Err(RuntimeError::InvalidEntry)
        );
        assert!(path.exists(), "replacement identity must be preserved");
        drop(replacement);
        fs::remove_file(path).expect("replacement fixture should clean");
    }

    #[test]
    fn stale_recovery_removes_only_empty_valid_entries() {
        let root = TestRoot::new();
        let directory = open_directory(root.path()).expect("test root should open");
        let empty_name = session_name(SessionId::from_bytes([1; 32])).expect("name should derive");
        let populated_name =
            session_name(SessionId::from_bytes([2; 32])).expect("name should derive");
        for name in [&empty_name, &populated_name] {
            // SAFETY: The directory and fixed names remain live for this call.
            let result = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) };
            assert_eq!(result, 0);
        }
        fs::write(
            root.path()
                .join(OsStr::from_bytes(populated_name.as_bytes()))
                .join("owned-data"),
            b"preserve",
        )
        .expect("populated marker should be written");

        recover_stale_entries(directory.as_raw_fd(), DirectoryOwner::current_user())
            .expect("recovery should succeed");

        assert!(
            !root
                .path()
                .join(OsStr::from_bytes(empty_name.as_bytes()))
                .exists()
        );
        assert!(
            root.path()
                .join(OsStr::from_bytes(populated_name.as_bytes()))
                .exists()
        );
    }

    #[test]
    fn launcher_cleanup_preserves_a_replaced_namespace_name() {
        let root = TestRoot::new();
        let name = session_name(SessionId::from_bytes([3; 32])).expect("name should derive");
        let named_path = root.path().join(OsStr::from_bytes(name.as_bytes()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&named_path)
            .expect("original namespace should be created");
        let root_fd = open_directory(root.path()).expect("test root should open");
        let original =
            openat_directory(root_fd.as_raw_fd(), &name).expect("original namespace should open");
        let identity = validate_directory(original.as_raw_fd()).expect("original should validate");
        let moved_path = root.path().join("moved-original");
        fs::rename(&named_path, &moved_path).expect("original should move");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&named_path)
            .expect("replacement should be created");
        let mut namespace = LauncherNamespace {
            root: root_fd,
            directory: original,
            name,
            identity,
            owner: DirectoryOwner::current_user(),
            #[cfg(feature = "elevated-bootstrap-probe")]
            publication: NamespacePublication::Linked,
            cleaned: false,
        };

        assert_eq!(namespace.cleanup(), Err(RuntimeError::InvalidEntry));
        drop(namespace);
        assert!(named_path.is_dir(), "replacement must be preserved");
        assert!(moved_path.is_dir(), "original fd target must be preserved");
    }

    #[test]
    fn launcher_socket_namespace_requires_linked_name_and_live_worker_lock() {
        let root = TestRoot::new();
        let name = session_name(SessionId::from_bytes([4; 32])).expect("name should derive");
        let named_path = root.path().join(OsStr::from_bytes(name.as_bytes()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&named_path)
            .expect("namespace should be created");
        let root_fd = open_directory(root.path()).expect("test root should open");
        let launcher_directory =
            openat_directory(root_fd.as_raw_fd(), &name).expect("launcher anchor should open");
        let identity =
            validate_directory(launcher_directory.as_raw_fd()).expect("identity should validate");
        let mut worker_directory = open_directory(&named_path).expect("worker anchor should open");
        lock_exclusive(worker_directory.as_raw_fd()).expect("worker lock should acquire");
        let mut namespace = LauncherNamespace {
            root: root_fd,
            directory: launcher_directory,
            name,
            identity,
            owner: DirectoryOwner::current_user(),
            #[cfg(feature = "elevated-bootstrap-probe")]
            publication: NamespacePublication::Linked,
            cleaned: false,
        };

        namespace
            .socket_namespace()
            .expect("linked namespace with a worker lock should duplicate");
        drop(worker_directory);
        assert!(matches!(
            namespace.socket_namespace(),
            Err(RuntimeError::InvalidEntry)
        ));

        worker_directory = open_directory(&named_path).expect("worker anchor should reopen");
        lock_exclusive(worker_directory.as_raw_fd()).expect("worker lock should reacquire");
        let moved_path = root.path().join("moved-live-namespace");
        fs::rename(&named_path, &moved_path).expect("original namespace should move");
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&named_path)
            .expect("replacement should be created");
        assert!(matches!(
            namespace.socket_namespace(),
            Err(RuntimeError::InvalidEntry)
        ));

        drop(worker_directory);
        assert_eq!(namespace.cleanup(), Err(RuntimeError::InvalidEntry));
        namespace.cleaned = true;
        assert!(named_path.is_dir(), "replacement must be preserved");
        assert!(moved_path.is_dir(), "original fd target must be preserved");
    }
}
