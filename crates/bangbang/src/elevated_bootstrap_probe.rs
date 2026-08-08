use std::env;
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

use bangbang_session::elevated_probe::{
    BOOTSTRAP_RECORD_BYTES, ProbeBootstrap, ProbeErrorCategory, ProbeResult, ProbeStage,
    READY_RECORD, RESULT_RECORD_BYTES, ROOT_FD, WORKER_ACTIVATION,
};
use bangbang_session::{ObjectIdentity, SESSION_ENV_KEY, SESSION_ENV_VALUE, SESSION_FD};

#[derive(Clone, Copy)]
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
}
