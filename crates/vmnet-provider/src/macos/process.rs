use std::ffi::{CString, c_char, c_void};
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::ptr;
use std::time::{Duration, Instant};

use crate::broker::BrokerError;

use super::{BOOTSTRAP_FD, PRIVATE_OWNER_MODE, PROVIDER_FD};

const MIN_SOURCE_FD: RawFd = 10;
const CHILD_WAIT: Duration = Duration::from_secs(2);
const PROC_PIDREGIONPATHINFO: libc::c_int = 8;
const MAX_PATH_BYTES: usize = 1024;

unsafe extern "C" {
    fn proc_pidinfo(
        pid: libc::c_int,
        flavor: libc::c_int,
        argument: u64,
        buffer: *mut c_void,
        buffer_size: libc::c_int,
    ) -> libc::c_int;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcRegionInfo {
    protection: u32,
    _maximum_protection: u32,
    _inheritance: u32,
    _flags: u32,
    offset: u64,
    _metrics: [u32; 14],
    address: u64,
    size: u64,
}

#[repr(C)]
struct VinfoStat {
    device: u32,
    mode: u16,
    link_count: u16,
    inode: u64,
    uid: u32,
    gid: u32,
    _timestamps: [i64; 8],
    _file_size: i64,
    _blocks: i64,
    _block_size: i32,
    _flags: u32,
    _generation: u32,
    _special_device: u32,
    _spare: [i64; 2],
}

#[repr(C)]
struct VnodeInfo {
    stat: VinfoStat,
    _kind: libc::c_int,
    _padding: libc::c_int,
    _filesystem: [i32; 2],
}

#[repr(C)]
struct VnodeInfoPath {
    vnode: VnodeInfo,
    _path: [libc::c_char; MAX_PATH_BYTES],
}

#[repr(C)]
struct ProcRegionWithPathInfo {
    region: ProcRegionInfo,
    vnode_path: VnodeInfoPath,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    link_count: u64,
    uid: u32,
    gid: u32,
    mode: u32,
}

pub(super) struct PinnedExecutable {
    path: CString,
    _descriptor: OwnedFd,
    identity: FileIdentity,
}

impl PinnedExecutable {
    pub(super) fn current() -> Result<Self, BrokerError> {
        let path = std::env::current_exe().map_err(|error| BrokerError::Io(error.kind()))?;
        if !path.is_absolute() {
            return Err(BrokerError::Process);
        }
        let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| BrokerError::Process)?;
        // SAFETY: `path` is a live NUL-terminated absolute path. A successful
        // descriptor is immediately uniquely owned.
        let descriptor = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if descriptor < 0 {
            return Err(BrokerError::Io(io::Error::last_os_error().kind()));
        }
        // SAFETY: `descriptor` is a fresh successful open result.
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let identity = identity_for_fd(descriptor.as_raw_fd())?;
        validate_executable_identity(identity)?;
        Ok(Self {
            path,
            _descriptor: descriptor,
            identity,
        })
    }

    fn validate_child(&self, pid: libc::pid_t) -> Result<(), BrokerError> {
        require_exact_identity(self.identity, identity_for_process_image(pid)?)
    }
}

impl std::fmt::Debug for PinnedExecutable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PinnedExecutable(<redacted>)")
    }
}

pub(super) struct SpawnedOwner {
    pub(super) child: OwnedChild,
    pub(super) supervision: UnixStream,
    pub(super) client_data: UnixStream,
}

impl std::fmt::Debug for SpawnedOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SpawnedOwner(<redacted>)")
    }
}

pub(super) fn spawn_owner(executable: &PinnedExecutable) -> Result<SpawnedOwner, BrokerError> {
    let (supervision, owner_supervision) =
        UnixStream::pair().map_err(|error| BrokerError::Io(error.kind()))?;
    let (owner_data, client_data) =
        UnixStream::pair().map_err(|error| BrokerError::Io(error.kind()))?;
    let owner_supervision = duplicate_stream(owner_supervision)?;
    let owner_data = duplicate_stream(owner_data)?;
    let null = open_null()?;

    let mode = CString::new(PRIVATE_OWNER_MODE).map_err(|_| BrokerError::Process)?;
    let mut argv = [
        executable.path.as_ptr().cast_mut(),
        mode.as_ptr().cast_mut(),
        ptr::null_mut::<c_char>(),
    ];
    let mut environment = [ptr::null_mut::<c_char>()];
    let mut attributes = SpawnAttributes::new()?;
    attributes.configure()?;
    let mut actions = SpawnFileActions::new()?;
    for standard in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        actions.duplicate(null.as_raw_fd(), standard)?;
    }
    actions.duplicate(owner_supervision.as_raw_fd(), BOOTSTRAP_FD)?;
    actions.duplicate(owner_data.as_raw_fd(), PROVIDER_FD)?;
    actions.close(owner_supervision.as_raw_fd())?;
    actions.close(owner_data.as_raw_fd())?;
    actions.close(null.as_raw_fd())?;

    let mut pid = 0;
    // SAFETY: All C strings, pointer arrays, spawn attributes/actions, and PID
    // storage remain live for this synchronous call.
    let result = unsafe {
        libc::posix_spawn(
            &raw mut pid,
            executable.path.as_ptr(),
            actions.as_ptr(),
            attributes.as_ptr(),
            argv.as_mut_ptr(),
            environment.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(BrokerError::Process);
    }
    let mut child = OwnedChild::new_suspended(pid).map_err(|_| BrokerError::CleanupUncertain)?;
    let validation = executable.validate_child(pid);
    complete_suspended_identity_gate(&mut child, validation)?;
    drop(owner_supervision);
    drop(owner_data);
    Ok(SpawnedOwner {
        child,
        supervision,
        client_data,
    })
}

pub(super) struct OwnedChild {
    pid: libc::pid_t,
    status: Option<ExitStatus>,
}

impl OwnedChild {
    fn new_suspended(pid: libc::pid_t) -> Result<Self, BrokerError> {
        if pid <= 0 {
            return Err(BrokerError::Process);
        }
        Ok(Self { pid, status: None })
    }

    fn resume(&self) -> Result<(), BrokerError> {
        self.signal(libc::SIGCONT)
    }

    pub(super) fn signal(&self, signal: libc::c_int) -> Result<(), BrokerError> {
        if self.status.is_some() {
            return Err(BrokerError::Process);
        }
        // SAFETY: `pid` remains an exact owned unreaped child and the signal is
        // selected by fixed provider lifecycle code.
        if unsafe { libc::kill(self.pid, signal) } == 0 {
            return Ok(());
        }
        if io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(BrokerError::Process)
        }
    }

    pub(super) fn try_wait(&mut self) -> Result<Option<ExitStatus>, BrokerError> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        let mut raw = 0;
        // SAFETY: `pid` is this object's exact unreaped child and `raw` is
        // writable for the synchronous wait query.
        let result = unsafe { libc::waitpid(self.pid, &raw mut raw, libc::WNOHANG) };
        if result == 0 {
            return Ok(None);
        }
        if result == self.pid {
            let status = ExitStatus::from_raw(raw);
            self.status = Some(status);
            return Ok(Some(status));
        }
        Err(BrokerError::Process)
    }

    pub(super) fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<Option<ExitStatus>, BrokerError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(BrokerError::Timeout)?;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(super) fn reap_clean(&mut self) -> Result<(), BrokerError> {
        match self.wait_timeout(CHILD_WAIT)? {
            Some(status) if status.success() => Ok(()),
            Some(_) => Err(BrokerError::Process),
            None => Err(BrokerError::Timeout),
        }
    }

    pub(super) fn terminate_and_reap(&mut self) -> Result<(), BrokerError> {
        match self.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(_) => return Err(BrokerError::CleanupUncertain),
        }
        let _ = self.signal(libc::SIGTERM);
        match self.wait_timeout(Duration::from_millis(500)) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(_) => return Err(BrokerError::CleanupUncertain),
        }
        let _ = self.signal(libc::SIGKILL);
        match self.wait_timeout(CHILD_WAIT) {
            Ok(Some(_)) => Ok(()),
            Ok(None) | Err(_) => Err(BrokerError::CleanupUncertain),
        }
    }
}

impl std::fmt::Debug for OwnedChild {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnedChild")
            .field("pid", &"<redacted>")
            .field("reaped", &self.status.is_some())
            .finish()
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.status.is_none() {
            let _ = self.terminate_and_reap();
        }
    }
}

trait SuspendedChildOps {
    fn resume_suspended(&mut self) -> Result<(), BrokerError>;
    fn terminate_suspended(&mut self) -> Result<(), BrokerError>;
}

impl SuspendedChildOps for OwnedChild {
    fn resume_suspended(&mut self) -> Result<(), BrokerError> {
        self.resume()
    }

    fn terminate_suspended(&mut self) -> Result<(), BrokerError> {
        self.terminate_and_reap()
    }
}

fn complete_suspended_identity_gate<C: SuspendedChildOps>(
    child: &mut C,
    validation: Result<(), BrokerError>,
) -> Result<(), BrokerError> {
    if let Err(error) = validation {
        return match child.terminate_suspended() {
            Ok(()) => Err(error),
            Err(_) => Err(BrokerError::CleanupUncertain),
        };
    }
    if let Err(error) = child.resume_suspended() {
        return match child.terminate_suspended() {
            Ok(()) => Err(error),
            Err(_) => Err(BrokerError::CleanupUncertain),
        };
    }
    Ok(())
}

fn duplicate_stream(stream: UnixStream) -> Result<UnixStream, BrokerError> {
    // SAFETY: The source is live; success returns a fresh close-on-exec
    // descriptor above every fixed child destination.
    let descriptor =
        unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, MIN_SOURCE_FD) };
    if descriptor < 0 {
        return Err(BrokerError::Io(io::Error::last_os_error().kind()));
    }
    // SAFETY: `descriptor` is a fresh uniquely owned Unix-stream duplicate.
    Ok(UnixStream::from(unsafe {
        OwnedFd::from_raw_fd(descriptor)
    }))
}

fn open_null() -> Result<OwnedFd, BrokerError> {
    // SAFETY: Fixed NUL-terminated path; success is immediately owned.
    let descriptor = unsafe {
        libc::open(
            c"/dev/null".as_ptr(),
            libc::O_RDWR | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err(BrokerError::Io(io::Error::last_os_error().kind()));
    }
    // SAFETY: `descriptor` is a fresh successful open result.
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn identity_for_fd(descriptor: RawFd) -> Result<FileIdentity, BrokerError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `descriptor` is live and `stat` is writable.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return Err(BrokerError::Process);
    }
    // SAFETY: Successful fstat initialized the structure.
    let stat = unsafe { stat.assume_init() };
    Ok(FileIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
        link_count: u64::from(stat.st_nlink),
        uid: stat.st_uid,
        gid: stat.st_gid,
        mode: u32::from(stat.st_mode),
    })
}

fn identity_for_process_image(pid: libc::pid_t) -> Result<FileIdentity, BrokerError> {
    let mut image = MaybeUninit::<ProcRegionWithPathInfo>::uninit();
    let buffer_size = libc::c_int::try_from(std::mem::size_of::<ProcRegionWithPathInfo>())
        .map_err(|_| BrokerError::Process)?;
    // SAFETY: `image` is writable for `buffer_size`; flavor 8 returns the
    // first mapped region and its vnode identity for the owned suspended PID.
    let result = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDREGIONPATHINFO,
            0,
            image.as_mut_ptr().cast(),
            buffer_size,
        )
    };
    if result != buffer_size {
        return Err(BrokerError::Process);
    }
    // SAFETY: An exact-size successful proc_pidinfo result initialized every
    // field in the fixed ABI structure.
    let image = unsafe { image.assume_init() };
    validate_executable_region(&image.region)?;
    let stat = &image.vnode_path.vnode.stat;
    Ok(FileIdentity {
        device: u64::from(stat.device),
        inode: stat.inode,
        link_count: u64::from(stat.link_count),
        uid: stat.uid,
        gid: stat.gid,
        mode: u32::from(stat.mode),
    })
}

fn validate_executable_region(region: &ProcRegionInfo) -> Result<(), BrokerError> {
    let execute = u32::try_from(libc::VM_PROT_EXECUTE).map_err(|_| BrokerError::Process)?;
    if region.offset == 0
        && region.address != 0
        && region.size != 0
        && region.protection & execute != 0
    {
        Ok(())
    } else {
        Err(BrokerError::Process)
    }
}

fn validate_executable_identity(identity: FileIdentity) -> Result<(), BrokerError> {
    if identity.mode & u32::from(libc::S_IFMT) != u32::from(libc::S_IFREG)
        || identity.device == 0
        || identity.inode == 0
        || identity.link_count != 1
        || identity.uid != 0
        || identity.gid != 0
        || identity.mode & 0o7000 != 0
        || identity.mode & 0o022 != 0
        || identity.mode & 0o111 == 0
    {
        Err(BrokerError::Process)
    } else {
        Ok(())
    }
}

fn require_exact_identity(expected: FileIdentity, actual: FileIdentity) -> Result<(), BrokerError> {
    if actual == expected {
        Ok(())
    } else {
        Err(BrokerError::Process)
    }
}

struct SpawnAttributes {
    value: MaybeUninit<libc::posix_spawnattr_t>,
    initialized: bool,
}

impl SpawnAttributes {
    fn new() -> Result<Self, BrokerError> {
        let mut value = Self {
            value: MaybeUninit::uninit(),
            initialized: false,
        };
        // SAFETY: This wrapper supplies writable uninitialized attribute storage.
        cvt_spawn(unsafe { libc::posix_spawnattr_init(value.value.as_mut_ptr()) })?;
        value.initialized = true;
        Ok(value)
    }

    fn configure(&mut self) -> Result<(), BrokerError> {
        let mut defaults = MaybeUninit::<libc::sigset_t>::uninit();
        // SAFETY: Signal-set storage is writable.
        let empty_result = unsafe { libc::sigemptyset(defaults.as_mut_ptr()) };
        // SAFETY: A successful sigemptyset initialized the storage, and SIGPIPE is fixed.
        let add_result = unsafe { libc::sigaddset(defaults.as_mut_ptr(), libc::SIGPIPE) };
        if empty_result != 0 || add_result != 0 {
            return Err(BrokerError::Process);
        }
        // SAFETY: Attribute and initialized signal set remain live.
        cvt_spawn(unsafe {
            libc::posix_spawnattr_setsigdefault(self.value.as_mut_ptr(), defaults.as_ptr())
        })?;
        let flags = libc::POSIX_SPAWN_CLOEXEC_DEFAULT
            | libc::POSIX_SPAWN_START_SUSPENDED
            | libc::POSIX_SPAWN_SETSIGDEF;
        let flags = libc::c_short::try_from(flags).map_err(|_| BrokerError::Process)?;
        // SAFETY: This wrapper owns one initialized attribute object.
        cvt_spawn(unsafe { libc::posix_spawnattr_setflags(self.value.as_mut_ptr(), flags) })
    }

    fn as_ptr(&self) -> *const libc::posix_spawnattr_t {
        self.value.as_ptr()
    }
}

impl Drop for SpawnAttributes {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: This wrapper owns one initialized attribute object.
            let _ = unsafe { libc::posix_spawnattr_destroy(self.value.as_mut_ptr()) };
        }
    }
}

struct SpawnFileActions {
    value: MaybeUninit<libc::posix_spawn_file_actions_t>,
    initialized: bool,
}

impl SpawnFileActions {
    fn new() -> Result<Self, BrokerError> {
        let mut value = Self {
            value: MaybeUninit::uninit(),
            initialized: false,
        };
        // SAFETY: This wrapper supplies writable uninitialized action storage.
        cvt_spawn(unsafe { libc::posix_spawn_file_actions_init(value.value.as_mut_ptr()) })?;
        value.initialized = true;
        Ok(value)
    }

    fn duplicate(&mut self, source: RawFd, destination: RawFd) -> Result<(), BrokerError> {
        // SAFETY: The action object is initialized and the integer descriptors
        // are interpreted by the child-side spawn implementation.
        cvt_spawn(unsafe {
            libc::posix_spawn_file_actions_adddup2(self.value.as_mut_ptr(), source, destination)
        })
    }

    fn close(&mut self, descriptor: RawFd) -> Result<(), BrokerError> {
        // SAFETY: The action object is initialized; the descriptor is evaluated
        // only in the child spawn context.
        cvt_spawn(unsafe {
            libc::posix_spawn_file_actions_addclose(self.value.as_mut_ptr(), descriptor)
        })
    }

    fn as_ptr(&self) -> *const libc::posix_spawn_file_actions_t {
        self.value.as_ptr()
    }
}

impl Drop for SpawnFileActions {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: This wrapper owns one initialized action object.
            let _ = unsafe { libc::posix_spawn_file_actions_destroy(self.value.as_mut_ptr()) };
        }
    }
}

fn cvt_spawn(result: libc::c_int) -> Result<(), BrokerError> {
    if result == 0 {
        Ok(())
    } else {
        Err(BrokerError::Process)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeSuspendedChild {
        resume_result: Result<(), BrokerError>,
        terminate_result: Result<(), BrokerError>,
        events: Vec<&'static str>,
    }

    impl SuspendedChildOps for FakeSuspendedChild {
        fn resume_suspended(&mut self) -> Result<(), BrokerError> {
            self.events.push("resume");
            self.resume_result
        }

        fn terminate_suspended(&mut self) -> Result<(), BrokerError> {
            self.events.push("terminate-reap");
            self.terminate_result
        }
    }

    fn identity() -> FileIdentity {
        FileIdentity {
            device: 1,
            inode: 2,
            link_count: 1,
            uid: 0,
            gid: 0,
            mode: u32::from(libc::S_IFREG) | 0o555,
        }
    }

    #[test]
    fn executable_identity_rejects_nonroot_or_writable_images() {
        let base = identity();
        assert_eq!(validate_executable_identity(base), Ok(()));
        assert_eq!(
            validate_executable_identity(FileIdentity { device: 0, ..base }),
            Err(BrokerError::Process)
        );
        assert_eq!(
            validate_executable_identity(FileIdentity { inode: 0, ..base }),
            Err(BrokerError::Process)
        );
        assert_eq!(
            validate_executable_identity(FileIdentity { uid: 501, ..base }),
            Err(BrokerError::Process)
        );
        assert_eq!(
            validate_executable_identity(FileIdentity { gid: 20, ..base }),
            Err(BrokerError::Process)
        );
        assert_eq!(
            validate_executable_identity(FileIdentity {
                link_count: 2,
                ..base
            }),
            Err(BrokerError::Process)
        );
        assert_eq!(
            validate_executable_identity(FileIdentity {
                mode: base.mode | 0o020,
                ..base
            }),
            Err(BrokerError::Process)
        );
        assert_eq!(
            validate_executable_identity(FileIdentity {
                mode: base.mode | 0o4000,
                ..base
            }),
            Err(BrokerError::Process)
        );
    }

    #[test]
    fn child_image_requires_every_pinned_identity_field() {
        let expected = identity();
        assert_eq!(require_exact_identity(expected, expected), Ok(()));
        for actual in [
            FileIdentity {
                device: 9,
                ..expected
            },
            FileIdentity {
                inode: 9,
                ..expected
            },
            FileIdentity {
                link_count: 2,
                ..expected
            },
            FileIdentity {
                uid: 501,
                ..expected
            },
            FileIdentity {
                gid: 20,
                ..expected
            },
            FileIdentity {
                mode: expected.mode | 0o200,
                ..expected
            },
        ] {
            assert_eq!(
                require_exact_identity(expected, actual),
                Err(BrokerError::Process)
            );
        }
    }

    #[test]
    fn mapped_image_abi_and_executable_region_are_fail_closed() {
        assert_eq!(std::mem::size_of::<ProcRegionInfo>(), 96);
        assert_eq!(std::mem::size_of::<VinfoStat>(), 136);
        assert_eq!(std::mem::size_of::<ProcRegionWithPathInfo>(), 1272);

        let executable = ProcRegionInfo {
            protection: u32::try_from(libc::VM_PROT_READ | libc::VM_PROT_EXECUTE)
                .expect("protection should fit"),
            _maximum_protection: 0,
            _inheritance: 0,
            _flags: 0,
            offset: 0,
            _metrics: [0; 14],
            address: 0x1_0000_0000,
            size: 16_384,
        };
        assert_eq!(validate_executable_region(&executable), Ok(()));
        for invalid in [
            ProcRegionInfo {
                protection: u32::try_from(libc::VM_PROT_READ).expect("protection should fit"),
                ..executable
            },
            ProcRegionInfo {
                offset: 1,
                ..executable
            },
            ProcRegionInfo {
                address: 0,
                ..executable
            },
            ProcRegionInfo {
                size: 0,
                ..executable
            },
        ] {
            assert_eq!(
                validate_executable_region(&invalid),
                Err(BrokerError::Process)
            );
        }
    }

    #[test]
    fn suspended_gate_reaps_before_resume_failure_can_escape() {
        let mut mismatch = FakeSuspendedChild {
            resume_result: Ok(()),
            terminate_result: Ok(()),
            events: Vec::new(),
        };
        assert_eq!(
            complete_suspended_identity_gate(&mut mismatch, Err(BrokerError::Process)),
            Err(BrokerError::Process)
        );
        assert_eq!(mismatch.events, ["terminate-reap"]);

        let mut resume_failure = FakeSuspendedChild {
            resume_result: Err(BrokerError::Process),
            terminate_result: Ok(()),
            events: Vec::new(),
        };
        assert_eq!(
            complete_suspended_identity_gate(&mut resume_failure, Ok(())),
            Err(BrokerError::Process)
        );
        assert_eq!(resume_failure.events, ["resume", "terminate-reap"]);

        let mut success = FakeSuspendedChild {
            resume_result: Ok(()),
            terminate_result: Ok(()),
            events: Vec::new(),
        };
        assert_eq!(
            complete_suspended_identity_gate(&mut success, Ok(())),
            Ok(())
        );
        assert_eq!(success.events, ["resume"]);

        let mut uncertain_cleanup = FakeSuspendedChild {
            resume_result: Ok(()),
            terminate_result: Err(BrokerError::CleanupUncertain),
            events: Vec::new(),
        };
        assert_eq!(
            complete_suspended_identity_gate(&mut uncertain_cleanup, Err(BrokerError::Process)),
            Err(BrokerError::CleanupUncertain)
        );
        assert_eq!(uncertain_cleanup.events, ["terminate-reap"]);
    }

    #[test]
    fn child_and_executable_debug_are_redacted() {
        let child = OwnedChild {
            pid: 42,
            status: Some(ExitStatus::from_raw(0)),
        };
        assert!(!format!("{child:?}").contains("42"));
    }
}
