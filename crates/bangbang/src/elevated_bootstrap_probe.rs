use std::env;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

use bangbang_hvf::HvfBackend;
use bangbang_runtime::VmBackend;
use bangbang_session::elevated_probe::{
    BOOTSTRAP_RECORD_BYTES, ProbeBootstrap, ProbeErrorCategory, ProbeResult, ProbeStage,
    READY_RECORD, RESULT_RECORD_BYTES, ROOT_FD, WORKER_ACTIVATION,
};
use bangbang_session::{ObjectIdentity, SESSION_ENV_KEY, SESSION_ENV_VALUE, SESSION_FD};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProbeError {
    stage: ProbeStage,
    kind: io::ErrorKind,
}

pub(crate) fn is_requested() -> bool {
    env::args_os()
        .nth(1)
        .is_some_and(|argument| argument == OsStr::new(WORKER_ACTIVATION))
}

pub(crate) fn run() -> ExitCode {
    let (mut stream, bootstrap) = match probe_session() {
        Ok(session) => session,
        Err(_) => return ExitCode::FAILURE,
    };
    let outcome = execute(bootstrap);
    let result = match outcome {
        Ok(()) => ProbeResult::success(bootstrap.mode(), bootstrap.nonce()),
        Err(error) => ProbeResult::failure(
            bootstrap.mode(),
            bootstrap.nonce(),
            error.stage,
            ProbeErrorCategory::from_io_kind(error.kind),
        ),
    };
    let Ok(result) = result else {
        return ExitCode::FAILURE;
    };
    let encoded = result.encode();
    debug_assert_eq!(encoded.len(), RESULT_RECORD_BYTES);
    if stream.write_all(&encoded).is_err() {
        return ExitCode::FAILURE;
    }
    if outcome.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn probe_session() -> Result<(UnixStream, ProbeBootstrap), ProbeError> {
    let arguments = env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.as_slice() != [OsStr::new(WORKER_ACTIVATION)] {
        return Err(invalid(ProbeStage::InitialIdentity));
    }
    let value = env::var_os(SESSION_ENV_KEY).ok_or_else(|| invalid(ProbeStage::InitialIdentity))?;
    // SAFETY: This runs at process entry before application threads exist and
    // consumes the private marker before any later child could inherit it.
    unsafe { env::remove_var(SESSION_ENV_KEY) };
    if value != OsStr::new(SESSION_ENV_VALUE) {
        return Err(invalid(ProbeStage::InitialIdentity));
    }
    bangbang_session::macos::set_cloexec(SESSION_FD)
        .map_err(|error| with_kind(ProbeStage::InitialIdentity, error.kind()))?;
    // SAFETY: The validated production spawn contract transfers fixed fd 3
    // exactly once to this process.
    let owned = unsafe { OwnedFd::from_raw_fd(SESSION_FD) };
    let mut stream = UnixStream::from(owned);
    // SAFETY: `getppid` has no pointer or ownership contract.
    let parent = unsafe { libc::getppid() };
    bangbang_session::macos::verify_peer(stream.as_raw_fd(), parent)
        .map_err(|_| permission(ProbeStage::InitialIdentity))?;
    stream
        .write_all(&READY_RECORD)
        .map_err(|error| with_kind(ProbeStage::InitialIdentity, error.kind()))?;
    let mut encoded = [0_u8; BOOTSTRAP_RECORD_BYTES];
    stream
        .read_exact(&mut encoded)
        .map_err(|error| with_kind(ProbeStage::InitialIdentity, error.kind()))?;
    let bootstrap =
        ProbeBootstrap::decode(&encoded).map_err(|_| invalid(ProbeStage::InitialIdentity))?;
    Ok((stream, bootstrap))
}

fn execute(config: ProbeBootstrap) -> Result<(), ProbeError> {
    validate_initial_identity()?;
    bangbang_session::macos::set_cloexec(ROOT_FD)
        .map_err(|error| with_kind(ProbeStage::TakeRoot, error.kind()))?;
    // SAFETY: The feature-gated production spawn contract transfers fixed fd 8
    // exactly once to this process.
    let root = unsafe { OwnedFd::from_raw_fd(ROOT_FD) };
    validate_root(root.as_raw_fd(), config.root())?;
    match config.mode() {
        bangbang_session::elevated_probe::ProbeMode::HvfControl => {
            drop(root);
            return run_hvf_control();
        }
        bangbang_session::elevated_probe::ProbeMode::InheritedRoot => {
            validate_inherited_root(config.root())?;
            drop(root);
            validate_sandbox_chroot_control()?;
            return run_hvf_control();
        }
        bangbang_session::elevated_probe::ProbeMode::Drop
        | bangbang_session::elevated_probe::ProbeMode::RetainRoot
        | bangbang_session::elevated_probe::ProbeMode::UnmappedSyscall => {}
        bangbang_session::elevated_probe::ProbeMode::Control => {
            return Err(invalid(ProbeStage::InitialIdentity));
        }
    }
    // SAFETY: `root` is the live, validated private directory descriptor.
    syscall(ProbeStage::EnterRoot, unsafe {
        libc::fchdir(root.as_raw_fd())
    })?;
    // SAFETY: The current directory is the retained exact root and the fixed
    // relative path contains no attacker-controlled bytes.
    syscall(ProbeStage::Chroot, unsafe { libc::chroot(c".".as_ptr()) })?;
    // SAFETY: The process has entered the private root and the fixed absolute
    // path is NUL-terminated.
    syscall(ProbeStage::ChangeDirectory, unsafe {
        libc::chdir(c"/".as_ptr())
    })?;
    drop(root);
    Err(ProbeError {
        stage: ProbeStage::UnexpectedContinuation,
        kind: io::ErrorKind::Other,
    })
}

fn validate_inherited_root(expected: ObjectIdentity) -> Result<(), ProbeError> {
    let slash = open_directory(c"/")?;
    validate_root(slash.as_raw_fd(), expected).map_err(|error| ProbeError {
        stage: ProbeStage::InheritedRoot,
        kind: error.kind,
    })?;
    let cwd = open_directory(c".")?;
    validate_root(cwd.as_raw_fd(), expected).map_err(|error| ProbeError {
        stage: ProbeStage::InheritedRoot,
        kind: error.kind,
    })
}

fn validate_sandbox_chroot_control() -> Result<(), ProbeError> {
    validate_sandbox_chroot_control_with(|| {
        // SAFETY: Cwd is the already inherited exact root and the fixed
        // relative string is NUL-terminated. Success would retain the same
        // root but violate the expected App Sandbox denial established by the
        // signed control.
        if unsafe { libc::chroot(c".".as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error().kind())
        }
    })
}

fn validate_sandbox_chroot_control_with<F>(chroot: F) -> Result<(), ProbeError>
where
    F: FnOnce() -> Result<(), io::ErrorKind>,
{
    match chroot() {
        Ok(()) => Err(ProbeError {
            stage: ProbeStage::UnexpectedContinuation,
            kind: io::ErrorKind::Other,
        }),
        Err(io::ErrorKind::PermissionDenied) => Ok(()),
        Err(kind) => Err(with_kind(ProbeStage::SandboxChrootControl, kind)),
    }
}

fn run_hvf_control() -> Result<(), ProbeError> {
    let mut backend = HvfBackend::new();
    run_hvf_control_with(&mut backend)
}

trait HvfControl {
    fn create(&mut self) -> Result<(), ()>;
    fn destroy(&mut self) -> Result<(), ()>;
}

impl HvfControl for HvfBackend {
    fn create(&mut self) -> Result<(), ()> {
        self.create_vm().map_err(|_| ())
    }

    fn destroy(&mut self) -> Result<(), ()> {
        self.destroy_vm().map_err(|_| ())
    }
}

fn run_hvf_control_with<B: HvfControl>(backend: &mut B) -> Result<(), ProbeError> {
    backend.create().map_err(|()| ProbeError {
        stage: ProbeStage::HvfCreate,
        kind: io::ErrorKind::Other,
    })?;
    backend.destroy().map_err(|()| ProbeError {
        stage: ProbeStage::HvfDestroy,
        kind: io::ErrorKind::Other,
    })
}

fn open_directory(path: &std::ffi::CStr) -> Result<OwnedFd, ProbeError> {
    // SAFETY: `path` is NUL-terminated, fixed by the caller, and no pointer is
    // retained by `open`.
    let descriptor = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        Err(last(ProbeStage::InheritedRoot))
    } else {
        // SAFETY: `descriptor` is a fresh successful result owned by this scope.
        Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
    }
}

fn validate_initial_identity() -> Result<(), ProbeError> {
    // SAFETY: Credential getters have no pointer or ownership contract.
    let identities = unsafe {
        (
            libc::getuid(),
            libc::geteuid(),
            libc::getgid(),
            libc::getegid(),
        )
    };
    if identities == (0, 0, 0, 0) {
        Ok(())
    } else {
        Err(permission(ProbeStage::InitialIdentity))
    }
}

fn validate_root(descriptor: libc::c_int, expected: ObjectIdentity) -> Result<(), ProbeError> {
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    // SAFETY: `descriptor` is live and `stat` is writable for one result.
    if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
        return Err(last(ProbeStage::ValidateRoot));
    }
    // SAFETY: Successful `fstat` initialized the complete value.
    let stat = unsafe { stat.assume_init() };
    validate_root_stat(&stat, expected)
}

fn validate_root_stat(stat: &libc::stat, expected: ObjectIdentity) -> Result<(), ProbeError> {
    let actual = ObjectIdentity {
        device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
        inode: stat.st_ino,
    };
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR
        || stat.st_mode & 0o7777 != 0o700
        || stat.st_uid != 0
        || stat.st_gid != 0
        || stat.st_nlink < 2
        || actual != expected
    {
        return Err(permission(ProbeStage::ValidateRoot));
    }
    Ok(())
}

fn syscall(stage: ProbeStage, status: libc::c_int) -> Result<(), ProbeError> {
    if status == 0 {
        Ok(())
    } else {
        Err(last(stage))
    }
}

fn last(stage: ProbeStage) -> ProbeError {
    with_kind(stage, io::Error::last_os_error().kind())
}

const fn with_kind(stage: ProbeStage, kind: io::ErrorKind) -> ProbeError {
    ProbeError { stage, kind }
}

const fn invalid(stage: ProbeStage) -> ProbeError {
    ProbeError {
        stage,
        kind: io::ErrorKind::InvalidInput,
    }
}

const fn permission(stage: ProbeStage) -> ProbeError {
    ProbeError {
        stage,
        kind: io::ErrorKind::PermissionDenied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeHvfControl {
        calls: Vec<&'static str>,
        create_result: Result<(), ()>,
        destroy_result: Result<(), ()>,
    }

    impl HvfControl for FakeHvfControl {
        fn create(&mut self) -> Result<(), ()> {
            self.calls.push("create");
            self.create_result
        }

        fn destroy(&mut self) -> Result<(), ()> {
            self.calls.push("destroy");
            self.destroy_result
        }
    }

    #[test]
    fn error_mapping_is_value_free() {
        for (kind, expected) in [
            (
                io::ErrorKind::PermissionDenied,
                ProbeErrorCategory::PermissionDenied,
            ),
            (
                io::ErrorKind::InvalidInput,
                ProbeErrorCategory::InvalidInput,
            ),
            (io::ErrorKind::NotFound, ProbeErrorCategory::Other),
        ] {
            assert_eq!(ProbeErrorCategory::from_io_kind(kind), expected);
        }
    }

    #[test]
    fn sandbox_chroot_control_accepts_only_permission_denied() {
        validate_sandbox_chroot_control_with(|| Err(io::ErrorKind::PermissionDenied))
            .expect("the signed denial control should pass");

        let continuation = validate_sandbox_chroot_control_with(|| Ok(()))
            .expect_err("unexpected chroot success should fail closed");
        assert_eq!(continuation.stage, ProbeStage::UnexpectedContinuation);
        assert_eq!(continuation.kind, io::ErrorKind::Other);

        let other = validate_sandbox_chroot_control_with(|| Err(io::ErrorKind::NotFound))
            .expect_err("a different failure class should remain distinct");
        assert_eq!(other.stage, ProbeStage::SandboxChrootControl);
        assert_eq!(other.kind, io::ErrorKind::NotFound);
    }

    #[test]
    fn inherited_root_stat_requires_the_exact_closed_identity_shape() {
        let root = std::fs::File::open("/").expect("test root descriptor should open");
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `root` is live and `stat` is writable for one result.
        let status = unsafe { libc::fstat(root.as_raw_fd(), stat.as_mut_ptr()) };
        assert_eq!(status, 0);
        // SAFETY: Successful `fstat` initialized the complete value.
        let mut stat = unsafe { stat.assume_init() };
        stat.st_mode = libc::S_IFDIR | 0o700;
        stat.st_uid = 0;
        stat.st_gid = 0;
        stat.st_nlink = 2;
        let expected = ObjectIdentity {
            device: u64::from(u32::from_ne_bytes(stat.st_dev.to_ne_bytes())),
            inode: stat.st_ino,
        };
        validate_root_stat(&stat, expected).expect("the exact inherited root should pass");

        let mut wrong_type = stat;
        wrong_type.st_mode = libc::S_IFREG | 0o700;
        let mut wrong_mode = stat;
        wrong_mode.st_mode = libc::S_IFDIR | 0o755;
        let mut wrong_uid = stat;
        wrong_uid.st_uid = 1;
        let mut wrong_gid = stat;
        wrong_gid.st_gid = 1;
        let mut wrong_links = stat;
        wrong_links.st_nlink = 1;
        for invalid in [wrong_type, wrong_mode, wrong_uid, wrong_gid, wrong_links] {
            assert_eq!(
                validate_root_stat(&invalid, expected),
                Err(permission(ProbeStage::ValidateRoot))
            );
        }
        assert_eq!(
            validate_root_stat(
                &stat,
                ObjectIdentity {
                    device: expected.device,
                    inode: expected.inode ^ 1,
                }
            ),
            Err(permission(ProbeStage::ValidateRoot))
        );
    }

    #[test]
    fn hvf_control_destroys_exactly_after_successful_create() {
        let mut success = FakeHvfControl {
            calls: Vec::new(),
            create_result: Ok(()),
            destroy_result: Ok(()),
        };
        run_hvf_control_with(&mut success).expect("create and destroy should succeed");
        assert_eq!(success.calls, ["create", "destroy"]);

        let mut create_failure = FakeHvfControl {
            calls: Vec::new(),
            create_result: Err(()),
            destroy_result: Ok(()),
        };
        let failure = run_hvf_control_with(&mut create_failure)
            .expect_err("create failure should stop the sequence");
        assert_eq!(failure.stage, ProbeStage::HvfCreate);
        assert_eq!(create_failure.calls, ["create"]);

        let mut destroy_failure = FakeHvfControl {
            calls: Vec::new(),
            create_result: Ok(()),
            destroy_result: Err(()),
        };
        let failure = run_hvf_control_with(&mut destroy_failure)
            .expect_err("destroy failure should be reported");
        assert_eq!(failure.stage, ProbeStage::HvfDestroy);
        assert_eq!(destroy_failure.calls, ["create", "destroy"]);
    }
}
