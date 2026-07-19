//! Test-bundle-only exercise of committed startup grant authority.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bangbang_hvf::{HvfBackend, HvfMemoryPermissions};
use bangbang_runtime::VmBackend;
use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryBacking, aarch64};
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
    probe_args(args)
        .and_then(|args| args.first())
        .is_some_and(|argument| argument == OPTION)
}

pub(crate) fn run(
    session: &mut ContainedSession,
    args: &[OsString],
) -> Result<(), ContainedSessionError> {
    let probe = ProbeCase::parse(probe_args(args).ok_or(ContainedSessionError)?)?;
    session.verify_launch_policy(probe.expected_no_file, probe.expected_file_size, false)?;
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
    if probe.exhausts_no_file() {
        verify_no_file_enforcement(read.as_raw_fd(), probe.expected_no_file)?;
    }
    if probe.exhausts_file_size() {
        trigger_file_size_enforcement(
            write.as_raw_fd(),
            probe.expected_file_size.ok_or(ContainedSessionError)?,
        )?;
    }
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

    if probe.verifies_shared_memory() {
        verify_shared_guest_memory_in_containment()?;
    }

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

fn verify_shared_guest_memory_in_containment() -> Result<(), ContainedSessionError> {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no pointer arguments.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).map_err(|_| ContainedSessionError)?;
    let layout = aarch64::dram_layout(page_size).map_err(|_| ContainedSessionError)?;
    let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
        .map_err(|_| ContainedSessionError)?;
    let guest_start = GuestAddress::new(aarch64::DRAM_MEM_START);
    let export = memory
        .regions()
        .first()
        .ok_or(ContainedSessionError)?
        .try_clone_shared_backing()
        .map_err(|_| ContainedSessionError)?
        .ok_or(ContainedSessionError)?;

    let from_memory = [0x11_u8, 0x22, 0x33, 0x44];
    memory
        .write_slice(&from_memory, guest_start)
        .map_err(|_| ContainedSessionError)?;
    let mut descriptor_read = [0_u8; 4];
    // SAFETY: the exported descriptor and output buffer remain live for this
    // exact synchronous read from the validated region offset.
    let read = unsafe {
        libc::pread(
            export.as_fd().as_raw_fd(),
            descriptor_read.as_mut_ptr().cast(),
            descriptor_read.len(),
            0,
        )
    };
    if usize::try_from(read).ok() != Some(descriptor_read.len()) || descriptor_read != from_memory {
        return Err(ContainedSessionError);
    }

    let from_descriptor = [0xaa_u8, 0xbb, 0xcc, 0xdd];
    // SAFETY: the exported descriptor and input bytes remain live for this
    // exact synchronous write within the validated shared object.
    let written = unsafe {
        libc::pwrite(
            export.as_fd().as_raw_fd(),
            from_descriptor.as_ptr().cast(),
            from_descriptor.len(),
            8,
        )
    };
    if usize::try_from(written).ok() != Some(from_descriptor.len()) {
        return Err(ContainedSessionError);
    }
    let mut memory_read = [0_u8; 4];
    memory
        .read_slice(
            &mut memory_read,
            guest_start.checked_add(8).ok_or(ContainedSessionError)?,
        )
        .map_err(|_| ContainedSessionError)?;
    if memory_read != from_descriptor {
        return Err(ContainedSessionError);
    }

    let mut backend = HvfBackend::new();
    backend.create_vm().map_err(|_| ContainedSessionError)?;
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .map_err(|_| ContainedSessionError)?;
    backend
        .unmap_guest_memory()
        .map_err(|_| ContainedSessionError)?;
    backend.destroy_vm().map_err(|_| ContainedSessionError)
}

fn verify_no_file_enforcement(source: RawFd, limit: u64) -> Result<(), ContainedSessionError> {
    let maximum = usize::try_from(limit).map_err(|_| ContainedSessionError)?;
    let mut duplicates = Vec::with_capacity(maximum);
    loop {
        // SAFETY: `source` remains live for the synchronous duplication. Each
        // successful result is a fresh close-on-exec descriptor.
        let descriptor = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 0) };
        if descriptor >= 0 {
            // SAFETY: Ownership of the fresh descriptor transfers exactly once.
            duplicates.push(unsafe { OwnedFd::from_raw_fd(descriptor) });
            if duplicates.len() > maximum {
                return Err(ContainedSessionError);
            }
            continue;
        }
        return if std::io::Error::last_os_error().raw_os_error() == Some(libc::EMFILE)
            && !duplicates.is_empty()
        {
            Ok(())
        } else {
            Err(ContainedSessionError)
        };
    }
}

fn trigger_file_size_enforcement(
    descriptor: RawFd,
    limit: u64,
) -> Result<(), ContainedSessionError> {
    let length = libc::off_t::try_from(limit).map_err(|_| ContainedSessionError)?;
    // SAFETY: The granted descriptor is writable and retained for both fixed
    // synchronous operations. Extending exactly to the installed limit is
    // valid; the following one-byte write must raise SIGXFSZ and cannot return.
    if unsafe { libc::ftruncate(descriptor, length) } != 0 {
        return Err(ContainedSessionError);
    }
    // SAFETY: The source byte remains live and `length` is the first forbidden
    // offset under the exact RLIMIT_FSIZE policy. A return means enforcement
    // failed or produced an unexpected recoverable result.
    let _ = unsafe { libc::pwrite(descriptor, b"x".as_ptr().cast(), 1, length) };
    Err(ContainedSessionError)
}

#[derive(Debug, Clone, Copy)]
struct ProbeCase {
    name: &'static str,
    hold: bool,
    expected_no_file: u64,
    expected_file_size: Option<u64>,
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
                expected_no_file: 2048,
                expected_file_size: None,
            }),
            Some("alpha") => Ok(Self {
                name: "alpha",
                hold: false,
                expected_no_file: 2048,
                expected_file_size: None,
            }),
            Some("beta") => Ok(Self {
                name: "beta",
                hold: false,
                expected_no_file: 2048,
                expected_file_size: None,
            }),
            Some("hold") => Ok(Self {
                name: "hold",
                hold: true,
                expected_no_file: 2048,
                expected_file_size: None,
            }),
            Some("hold-alpha") => Ok(Self {
                name: "alpha",
                hold: true,
                expected_no_file: 2048,
                expected_file_size: None,
            }),
            Some("hold-beta") => Ok(Self {
                name: "beta",
                hold: true,
                expected_no_file: 2048,
                expected_file_size: None,
            }),
            Some("policy-default") => Ok(Self {
                name: "policy-default",
                hold: false,
                expected_no_file: 2048,
                expected_file_size: None,
            }),
            Some("policy-explicit") => Ok(Self {
                name: "policy-explicit",
                hold: false,
                expected_no_file: 1024,
                expected_file_size: Some(4096),
            }),
            Some("policy-last") => Ok(Self {
                name: "policy-last",
                hold: false,
                expected_no_file: 2048,
                expected_file_size: Some(4096),
            }),
            Some("policy-nofile-exhaustion") => Ok(Self {
                name: "policy-nofile-exhaustion",
                hold: false,
                expected_no_file: 1024,
                expected_file_size: None,
            }),
            Some("policy-fsize-exhaustion") => Ok(Self {
                name: "policy-fsize-exhaustion",
                hold: false,
                expected_no_file: 1024,
                expected_file_size: Some(4096),
            }),
            Some("shared-memory") => Ok(Self {
                name: "shared-memory",
                hold: false,
                expected_no_file: 2048,
                expected_file_size: None,
            }),
            _ => Err(ContainedSessionError),
        }
    }

    fn exhausts_no_file(self) -> bool {
        self.name == "policy-nofile-exhaustion"
    }

    fn exhausts_file_size(self) -> bool {
        self.name == "policy-fsize-exhaustion"
    }

    fn verifies_shared_memory(self) -> bool {
        self.name == "shared-memory"
    }
}

fn probe_args(args: &[OsString]) -> Option<&[OsString]> {
    if args.first().is_some_and(|argument| argument == OPTION) {
        return Some(args);
    }
    let [id, _, start, _, start_cpu, _, parent_cpu, _, rest @ ..] = args else {
        return None;
    };
    (id == "--id"
        && start == "--start-time-us"
        && start_cpu == "--start-time-cpu-us"
        && parent_cpu == "--parent-cpu-time-us")
        .then_some(rest)
}
