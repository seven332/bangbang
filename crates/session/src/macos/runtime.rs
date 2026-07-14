use std::ffi::{CStr, CString, OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::SessionId;

const WORKER_CONTAINER_SUFFIX: &str = "Library/Containers/dev.bangbang.worker/Data/tmp";
const RUNTIME_ROOT_NAME: &str = "bangbang-sessions-v1";
const SESSION_PREFIX: &str = "session-";
const SESSION_NAME_BYTES: usize = SESSION_PREFIX.len() + 64;
const MAX_CONFSTR_BYTES: usize = 4096;
const MAX_PASSWD_BUFFER_BYTES: usize = 64 * 1024;
const DEFAULT_PASSWD_BUFFER_BYTES: usize = 16 * 1024;
const MAX_RECOVERY_ENTRIES: usize = 128;

/// Device/inode proof sent in the bounded bootstrap protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamespaceIdentity {
    /// Filesystem device number.
    pub device: u64,
    /// Filesystem inode number.
    pub inode: u64,
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

    /// Recovers the exact empty namespace after the owned worker is reaped.
    ///
    /// This covers failures before `Prepared` was decoded. A missing root or
    /// session name is ordinary; live, replaced, populated, or invalid entries
    /// remain fail-closed.
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
        if !directory_is_empty(directory.as_raw_fd())?
            || !try_lock_exclusive(directory.as_raw_fd())?
        {
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
        || stat.st_mode & 0o777 != 0o700
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
        if !try_lock_exclusive(directory.as_raw_fd())?
            || !directory_is_empty(directory.as_raw_fd())?
        {
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
    // SAFETY: `fd` is live and `F_DUPFD_CLOEXEC` returns an independently owned fd.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(RuntimeError::Filesystem(io::Error::last_os_error().kind()));
    }
    // SAFETY: `duplicate` is a fresh descriptor; `fdopendir` takes ownership on success.
    let directory = unsafe { libc::fdopendir(duplicate) };
    if directory.is_null() {
        // SAFETY: `fdopendir` failed and did not consume `duplicate`.
        let _ = unsafe { libc::close(duplicate) };
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
