//! Test-bundle-only exercise of committed startup grant authority.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bangbang_session::{GrantAccess, GrantId, ResourceRole};

use crate::contained_session::{ContainedSession, ContainedSessionError};

const OPTION: &str = "--bangbang-internal-grant-probe-v1";
const READY_LINE: &str = "status: grant integration probe ready";
const OUTSIDE_FILE: &str = "bangbang-grant-probe-outside";
const SNAPSHOT_STAGING_HOLD_OPTION: &str = "--bangbang-internal-snapshot-staging-hold-v1";
static SNAPSHOT_STAGING_HOLD: AtomicBool = AtomicBool::new(false);

pub(crate) fn configure_snapshot_staging_hold(args: &mut Vec<OsString>) {
    if args
        .first()
        .is_some_and(|argument| argument == SNAPSHOT_STAGING_HOLD_OPTION)
    {
        args.remove(0);
        SNAPSHOT_STAGING_HOLD.store(true, Ordering::Release);
    }
}

pub(crate) fn hold_after_snapshot_staging_record() {
    if SNAPSHOT_STAGING_HOLD.swap(false, Ordering::AcqRel) {
        loop {
            std::thread::park();
        }
    }
}

pub(crate) fn is_requested(args: &[OsString]) -> bool {
    args.first().is_some_and(|argument| argument == OPTION)
}

pub(crate) fn run(
    session: &mut ContainedSession,
    args: &[OsString],
) -> Result<(), ContainedSessionError> {
    let probe = ProbeCase::parse(args)?;
    let authority = session.grant_authority().ok_or(ContainedSessionError)?;

    let read_id =
        GrantId::parse(&format!("probe-read-{}", probe.name)).map_err(|_| ContainedSessionError)?;
    let write_id = GrantId::parse(&format!("probe-write-{}", probe.name))
        .map_err(|_| ContainedSessionError)?;
    let directory_id =
        GrantId::parse(&format!("probe-dir-{}", probe.name)).map_err(|_| ContainedSessionError)?;
    let (read, write) = authority.with_registry(|registry| {
        let read = registry
            .take_file(&read_id, ResourceRole::KernelImage, GrantAccess::ReadOnly)
            .map_err(|_| ContainedSessionError)?;
        let write = registry
            .take_file(&write_id, ResourceRole::LoggerSink, GrantAccess::WriteOnly)
            .map_err(|_| ContainedSessionError)?;
        Ok((read, write))
    })?;
    let directory = session.with_directory_grants(|registry| {
        registry
            .take_scoped_directory(&directory_id, ResourceRole::ApiSocketDirectory)
            .map_err(|_| ContainedSessionError)
    })?;
    let expected_read = format!("bangbang-grant-read-{}\n", probe.name);
    let mut actual_read = vec![0_u8; expected_read.len()];
    // SAFETY: The buffer is writable for its exact length and the registry owns
    // the live descriptor throughout the synchronous read.
    let read_length = unsafe {
        libc::pread(
            read.as_raw_fd(),
            actual_read.as_mut_ptr().cast(),
            actual_read.len(),
            0,
        )
    };
    if usize::try_from(read_length).ok() != Some(actual_read.len())
        || actual_read != expected_read.as_bytes()
    {
        return Err(ContainedSessionError);
    }
    // SAFETY: This deliberately probes the kernel-enforced read-only access
    // mode without changing ownership or exposing any content.
    let denied_write = unsafe { libc::pwrite(read.as_raw_fd(), b"x".as_ptr().cast(), 1, 0) };
    if denied_write != -1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EBADF) {
        return Err(ContainedSessionError);
    }

    let expected_write = format!("bangbang-grant-write-{}\n", probe.name);
    // SAFETY: The source bytes remain live and the registry owns the exact
    // write-only descriptor for the synchronous fixed-offset write.
    let write_length = unsafe {
        libc::pwrite(
            write.as_raw_fd(),
            expected_write.as_ptr().cast(),
            expected_write.len(),
            0,
        )
    };
    if usize::try_from(write_length).ok() != Some(expected_write.len()) {
        return Err(ContainedSessionError);
    }
    let mut denied_byte = 0_u8;
    // SAFETY: This deliberately probes the kernel-enforced write-only access
    // mode using one valid writable output byte.
    let denied_read =
        unsafe { libc::pread(write.as_raw_fd(), (&raw mut denied_byte).cast(), 1, 0) };
    if denied_read != -1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EBADF) {
        return Err(ContainedSessionError);
    }

    let parent = directory.path().parent().ok_or(ContainedSessionError)?;
    match File::open(parent.join(OUTSIDE_FILE)) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
        Ok(_) | Err(_) => return Err(ContainedSessionError),
    }
    let child = directory
        .path()
        .join(format!("bangbang-grant-{}.out", probe.name));
    let mut output = OpenOptions::new();
    output
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW_ANY);
    output
        .open(child)
        .and_then(|mut file| file.write_all(expected_write.as_bytes()))
        .map_err(|_| ContainedSessionError)?;

    if probe.hold {
        println!("{READY_LINE}");
        std::io::stdout()
            .flush()
            .map_err(|_| ContainedSessionError)?;
        loop {
            match session.shutdown_requested() {
                Ok(false) => std::thread::park_timeout(Duration::from_millis(10)),
                Ok(true) => return Ok(()),
                Err(_) => return Err(ContainedSessionError),
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ProbeCase {
    name: &'static str,
    hold: bool,
}

impl ProbeCase {
    fn parse(args: &[OsString]) -> Result<Self, ContainedSessionError> {
        let [option, value] = args else {
            return Err(ContainedSessionError);
        };
        if option != OPTION {
            return Err(ContainedSessionError);
        }
        match value.to_str() {
            Some("single") => Ok(Self {
                name: "single",
                hold: false,
            }),
            Some("alpha") => Ok(Self {
                name: "alpha",
                hold: false,
            }),
            Some("beta") => Ok(Self {
                name: "beta",
                hold: false,
            }),
            Some("hold") => Ok(Self {
                name: "hold",
                hold: true,
            }),
            Some("hold-alpha") => Ok(Self {
                name: "alpha",
                hold: true,
            }),
            Some("hold-beta") => Ok(Self {
                name: "beta",
                hold: true,
            }),
            _ => Err(ContainedSessionError),
        }
    }
}
