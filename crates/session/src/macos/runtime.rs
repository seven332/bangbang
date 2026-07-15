use std::ffi::{CStr, CString, OsStr, OsString};
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
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

fn socket_record_name(role: ResourceRole) -> Result<&'static CStr, RuntimeError> {
    match role {
        ResourceRole::ApiSocketDirectory => Ok(c".api-socket-owner"),
        ResourceRole::VsockSocketDirectory => Ok(c".vsock-socket-owner"),
        _ => Err(RuntimeError::InvalidEntry),
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

/// Worker-side duplicate of the locked private namespace directory.
pub struct WorkerSocketNamespace {
    directory: OwnedFd,
    identity: NamespaceIdentity,
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
    /// Duplicates the validated namespace anchor with close-on-exec ownership.
    pub fn try_clone(&self) -> Result<Self, RuntimeError> {
        Ok(Self {
            directory: duplicate_fd(self.directory.as_raw_fd())?,
            identity: self.identity,
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
        write_socket_record(self.anchor_fd(), record)
    }

    /// Removes only the exact current ownership record.
    pub fn clear_socket_record(&self, record: &SocketOwnershipRecord) -> Result<(), RuntimeError> {
        clear_socket_record(self.anchor_fd(), record)
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

/// Worker-owned locked namespace inside its App Sandbox container.
pub struct WorkerNamespace {
    root: OwnedFd,
    directory: OwnedFd,
    name: CString,
    identity: NamespaceIdentity,
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
        let root_lock = RootLock::acquire(root.as_raw_fd())?;
        recover_stale_entries(root.as_raw_fd())?;
        let name = session_name(session)?;
        // SAFETY: `root` is a live directory fd, `name` is NUL-terminated, and
        // no pointer is retained.
        if unsafe { libc::mkdirat(root.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::AlreadyExists {
                Err(RuntimeError::Collision)
            } else {
                Err(RuntimeError::Filesystem(error.kind()))
            };
        }

        // Once the name is published, failures preserve it for identity-checked
        // stale recovery. Blind rollback could remove a same-user replacement
        // installed between `mkdirat` and the failing operation.
        let directory = openat_directory(root.as_raw_fd(), &name)?;
        let identity = validate_directory(directory.as_raw_fd())?;
        lock_exclusive(directory.as_raw_fd())?;
        drop(root_lock);
        Ok(Self {
            root,
            directory,
            name,
            identity,
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
        })
    }

    /// Removes only the same empty namespace inode.
    pub fn cleanup(&mut self) -> Result<(), RuntimeError> {
        if self.cleaned {
            return Ok(());
        }
        cleanup_exact(
            self.root.as_raw_fd(),
            self.directory.as_raw_fd(),
            &self.name,
            self.identity,
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
    /// Independently derives and validates the worker-created namespace.
    pub fn validate(session: SessionId, expected: NamespaceIdentity) -> Result<Self, RuntimeError> {
        let root_path = launcher_runtime_root()?;
        let root = open_directory(&root_path)?;
        validate_directory(root.as_raw_fd())?;
        let name = session_name(session)?;
        let directory = openat_directory(root.as_raw_fd(), &name)?;
        let actual = validate_directory(directory.as_raw_fd())?;
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
        validate_directory(root.as_raw_fd())?;
        let name = session_name(session)?;
        let directory = match openat_directory(root.as_raw_fd(), &name) {
            Ok(directory) => directory,
            Err(RuntimeError::Filesystem(io::ErrorKind::NotFound)) => return Ok(None),
            Err(error) => return Err(error),
        };
        let identity = validate_directory(directory.as_raw_fd())?;
        if !try_lock_exclusive(directory.as_raw_fd())? {
            return Err(RuntimeError::InvalidEntry);
        }
        if !directory_contains_only_socket_records(directory.as_raw_fd())? {
            return Err(RuntimeError::InvalidEntry);
        }
        Ok(Some(Self {
            root,
            directory,
            name,
            identity,
            cleaned: false,
        }))
    }

    /// Removes only the same empty entry after the worker lock is released.
    pub fn cleanup(&mut self) -> Result<(), RuntimeError> {
        if self.cleaned {
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
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `stat` is writable for one result and `fd` remains owned by caller.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
    // SAFETY: Successful `fstat` initialized the result.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: Identity call has no pointer or ownership contract.
    let uid = unsafe { libc::geteuid() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_mode & 0o7777 != 0o700
        || stat.st_uid != uid
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

fn recover_stale_entries(root: RawFd) -> Result<(), RuntimeError> {
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
        let Ok(identity) = validate_directory(directory.as_raw_fd()) else {
            continue;
        };
        if !try_lock_exclusive(directory.as_raw_fd())? {
            continue;
        }
        if !directory_is_empty(directory.as_raw_fd())? {
            continue;
        }
        let _ = cleanup_exact(root, directory.as_raw_fd(), &name, identity);
    }
    Ok(())
}

fn directory_entries(fd: RawFd, limit: usize) -> Result<Vec<OsString>, RuntimeError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    // SAFETY: `fd` is a live directory and the fixed relative path opens the
    // same directory with an independent file description and directory cursor.
    let independent = unsafe {
        libc::openat(
            fd,
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if independent < 0 {
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
    // SAFETY: `independent` is a fresh descriptor; `fdopendir` takes ownership on success.
    let directory = unsafe { libc::fdopendir(independent) };
    if directory.is_null() {
        // SAFETY: `fdopendir` failed and did not consume `independent`.
        let _ = unsafe { libc::close(independent) };
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
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

fn directory_contains_only_socket_records(fd: RawFd) -> Result<bool, RuntimeError> {
    let mut expected = Vec::with_capacity(2);
    for role in [
        ResourceRole::ApiSocketDirectory,
        ResourceRole::VsockSocketDirectory,
    ] {
        if read_socket_record(fd, role)?.is_some() {
            expected.push(socket_record_name(role)?.to_bytes());
        }
    }
    let entries = directory_entries(fd, 3)?;
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
) -> Result<(), RuntimeError> {
    if validate_directory(directory)? != expected || !directory_is_empty(directory)? {
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

        recover_stale_entries(directory.as_raw_fd()).expect("recovery should succeed");

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
            cleaned: false,
        };

        assert_eq!(namespace.cleanup(), Err(RuntimeError::InvalidEntry));
        drop(namespace);
        assert!(named_path.is_dir(), "replacement must be preserved");
        assert!(moved_path.is_dir(), "original fd target must be preserved");
    }
}
