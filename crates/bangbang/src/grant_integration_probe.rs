//! Test-bundle-only exercise of committed startup grant authority.

use std::cell::Cell;
use std::ffi::{CString, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bangbang_hvf::{HvfBackend, HvfLazyHostFaultBridge, HvfLazyPager, HvfMemoryPermissions};
use bangbang_pager::{
    CancelReason, MAX_FRAME_BYTES, MIN_PAGE_SIZE, PageAccess, PagerClient, PagerClientPage,
    PagerError, PagerGeneration, PagerLimits, PagerOperations, PagerRegion, PagerRegionId,
    PagerSessionId, PagerTransport, PagerVmmState, REFERENCE_PAGE_BYTE, TerminalCode, VmmSession,
};
use bangbang_runtime::VmBackend;
use bangbang_runtime::block::{BlockFileBacking, PreparedBlockDevice};
use bangbang_runtime::lazy_memory::{
    LazyGuestMemory, LazyGuestMemoryLimits, LazyGuestMemoryRegion,
};
use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryBacking, aarch64};
use bangbang_runtime::snapshot_device_v2::{
    NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2DeviceGraph, SnapshotV2DeviceKey,
};
use bangbang_runtime::snapshot_memory::write_snapshot_memory_image;
use bangbang_runtime::snapshot_restore::{
    SnapshotRestorePublicId, SnapshotRestoreResourceClass, SnapshotRestoreResourceKey,
};
use bangbang_runtime::virtio_queue::VirtqueueAvailableRing;
use bangbang_runtime::vsock::VsockBackendSelector;
use bangbang_session::{
    GrantAccess, GrantId, GrantObjectKind, ObjectIdentity, ResourceRole, VmnetBackendRoute,
};

use crate::contained_session::{
    ContainedSession, ContainedSessionError, ContainedSnapshotRestoreAuthority,
    ContainedSnapshotRestoreErrorKind,
};
use crate::host_network::remote_vmnet::{ProcessVmnetBackendSource, RemoteVmnetProviderSource};
use crate::host_network::vmnet::{
    StartedVmnetPacketIoBackend, VmnetInterfaceConfig, VmnetPacketAvailableCallback,
    VmnetPacketIoBackend, VmnetReadPacket, VmnetWritePacket,
};
use crate::snapshot_restore_resources::{
    PreparedSnapshotRootRestoreCompletion, RequestedSnapshotRestoreResource,
    RequestedSnapshotRestoreResources,
};
use crate::vsock_restore::{
    ActiveVsockRestoreGuard, PreparedVsockRestoreResource, RequestedVsockRestoreResource,
};

const OPTION: &str = "--bangbang-internal-grant-probe-v1";
const READY_LINE: &str = "status: grant integration probe ready";
const PAGER_READY_LINE: &str = "status: pager integration probe ready";
const PAGER_GRANT_REF: &str = "bangbang-grant:probe-pager";
const PAGER_TIMEOUT: Duration = Duration::from_secs(1);
const OUTSIDE_FILE: &str = "bangbang-grant-probe-outside";
const BLOCK_CONTROL_GRANT_REF: &str = "bangbang-grant:probe-block-control";
const BLOCK_CONTROL_INITIAL_MARKER: &[u8] = b"BANGBANG_BLOCK_CONTROL_INITIAL";
const BLOCK_CONTROL_WRITTEN_MARKER: &[u8] = b"BANGBANG_BLOCK_CONTROL_WRITTEN";
const BLOCK_CONTROL_WRITE_BLOCK: u64 = 8;
const SNAPSHOT_STAGING_HOLD_OPTION: &str = "--bangbang-internal-snapshot-staging-hold-v1";
const RESTORE_ROOT_ID: &str = "restore-root-1601";
const RESTORE_VSOCK_ID: &str = "restore-vsock-1601";
const RESTORE_ROOT_REF: &str = "bangbang-grant:restore-root-1601";
const RESTORE_VSOCK_REF: &str = "bangbang-grant:restore-vsock-1601/restore-1601.sock";
const RESTORE_ROOT_MARKER: &[u8] = b"BANGBANG_RESTORE_TRANSACTION_ROOT_1601\n";
const RESTORE_ACTIVE_READY_LINE: &str = "status: restore transaction active";
const RESTORE_PREPARED_READY_LINE: &str = "status: restore transaction prepared";
const RESTORE_REPLACE_READY_LINE: &str = "status: restore transaction awaiting replacement";
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
    let probe_args = probe_args(args).ok_or(ContainedSessionError)?;
    if let Some(restore) = RestoreProbeCase::parse(probe_args)? {
        session.verify_launch_policy(2048, None, false)?;
        return verify_restore_transaction_in_containment(session, restore);
    }
    if let Some(pager) = PagerProbeCase::parse(probe_args)? {
        session.verify_launch_policy(2048, None, false)?;
        return verify_pager_in_containment(session, pager);
    }
    if let Some(vmnet_provider) = VmnetProviderProbeCase::parse(probe_args)? {
        session.verify_vmnet_provider_launch_policy()?;
        return verify_vmnet_provider_in_containment(session, vmnet_provider);
    }
    let probe = ProbeCase::parse(probe_args)?;
    session.verify_launch_policy(probe.expected_no_file, probe.expected_file_size, false)?;
    let authority = session.grant_authority().ok_or(ContainedSessionError)?;
    if probe.verifies_block_control() {
        return verify_block_control_in_containment(&authority);
    }

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
    if probe.verifies_target_runtime() {
        create_validate_remove_child(
            directory.anchor_fd(),
            &format!("bangbang-grant-{}.out", probe.name),
            expected_write.as_bytes(),
        )?;
    } else {
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
    }

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

fn create_validate_remove_child(
    directory: RawFd,
    name: &str,
    expected: &[u8],
) -> Result<(), ContainedSessionError> {
    let mut child = ExactGrantChild::create(directory, name)?;
    child
        .file
        .write_all(expected)
        .and_then(|()| child.file.sync_all())
        .map_err(|_| ContainedSessionError)?;
    let expected_identity = child.identity()?;

    // SAFETY: Same retained anchor and exact component; no path fallback.
    let descriptor = unsafe {
        libc::openat(
            directory,
            child.name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if descriptor < 0 {
        return Err(ContainedSessionError);
    }
    // SAFETY: `descriptor` is a fresh successful result owned by this scope.
    let mut input = File::from(unsafe { OwnedFd::from_raw_fd(descriptor) });
    let metadata = input.metadata().map_err(|_| ContainedSessionError)?;
    // SAFETY: Identity getters have no pointer or ownership contract.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    if !metadata.is_file()
        || metadata.permissions().mode() & 0o7777 != 0o600
        || metadata.uid() != uid
        || metadata.gid() != gid
        || metadata.nlink() != 1
        || (ObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        }) != expected_identity
    {
        return Err(ContainedSessionError);
    }
    let mut actual = Vec::new();
    input
        .read_to_end(&mut actual)
        .map_err(|_| ContainedSessionError)?;
    if actual != expected {
        return Err(ContainedSessionError);
    }
    drop(input);
    child.remove()
}

struct ExactGrantChild {
    directory: RawFd,
    name: CString,
    file: File,
    armed: bool,
}

impl ExactGrantChild {
    fn create(directory: RawFd, name: &str) -> Result<Self, ContainedSessionError> {
        let name = CString::new(name).map_err(|_| ContainedSessionError)?;
        // SAFETY: `directory` is the retained granted anchor, `name` is one
        // bounded component, and success transfers the fresh descriptor.
        let descriptor = unsafe {
            libc::openat(
                directory,
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(ContainedSessionError);
        }
        // SAFETY: `descriptor` is a fresh successful result owned by this scope.
        let file = File::from(unsafe { OwnedFd::from_raw_fd(descriptor) });
        Ok(Self {
            directory,
            name,
            file,
            armed: true,
        })
    }

    fn identity(&self) -> Result<ObjectIdentity, ContainedSessionError> {
        let metadata = self.file.metadata().map_err(|_| ContainedSessionError)?;
        Ok(ObjectIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    fn remove(&mut self) -> Result<(), ContainedSessionError> {
        self.remove_if_same()
    }

    fn remove_if_same(&mut self) -> Result<(), ContainedSessionError> {
        if !self.armed {
            return Ok(());
        }
        if !self.current_name_is_exact()? {
            return Err(ContainedSessionError);
        }
        // SAFETY: The exact live child identity was revalidated beneath the retained anchor.
        if unsafe { libc::unlinkat(self.directory, self.name.as_ptr(), 0) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(ContainedSessionError);
            }
        }
        self.armed = false;
        Ok(())
    }

    fn current_name_is_exact(&mut self) -> Result<bool, ContainedSessionError> {
        let mut source = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: The guard owns the live source descriptor and stat is writable.
        if unsafe { libc::fstat(self.file.as_raw_fd(), source.as_mut_ptr()) } != 0 {
            return Err(ContainedSessionError);
        }
        // SAFETY: Successful fstat initialized the complete result.
        let source = unsafe { source.assume_init() };
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: The retained anchor and bounded name remain live; stat is writable.
        if unsafe {
            libc::fstatat(
                self.directory,
                self.name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                self.armed = false;
                return Ok(true);
            }
            return Err(ContainedSessionError);
        }
        // SAFETY: Successful fstatat initialized the complete result.
        let stat = unsafe { stat.assume_init() };
        if source.st_mode & libc::S_IFMT != libc::S_IFREG
            || source.st_mode & 0o7777 != 0o600
            || source.st_nlink != 1
            || stat.st_dev != source.st_dev
            || stat.st_ino != source.st_ino
            || stat.st_mode & libc::S_IFMT != libc::S_IFREG
            || stat.st_mode & 0o7777 != 0o600
            || stat.st_uid != source.st_uid
            || stat.st_gid != source.st_gid
            || stat.st_nlink != 1
        {
            return Ok(false);
        }
        Ok(true)
    }
}

impl Drop for ExactGrantChild {
    fn drop(&mut self) {
        let _ = self.remove_if_same();
    }
}

fn verify_pager_in_containment(
    session: &ContainedSession,
    probe: PagerProbeCase,
) -> Result<(), ContainedSessionError> {
    let authority = session
        .pager_grant_authority()
        .ok_or(ContainedSessionError)?;
    if !authority.is_active() {
        return Err(ContainedSessionError);
    }
    let claimed = authority
        .claim(Path::new(PAGER_GRANT_REF))
        .map_err(|_| ContainedSessionError)?
        .ok_or(ContainedSessionError)?;
    if claimed.source_identity().inode == 0 || claimed.peer().process_id() == 0 {
        return Err(ContainedSessionError);
    }
    let stream = claimed.into_stream();
    // SAFETY: F_GETFD observes the live claimed stream descriptor.
    let descriptor_flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFD) };
    if descriptor_flags < 0 || descriptor_flags & libc::FD_CLOEXEC == 0 {
        return Err(ContainedSessionError);
    }
    if probe == PagerProbeCase::Terminal {
        let mut transport =
            PagerTransport::new(stream, PAGER_TIMEOUT).map_err(|_| ContainedSessionError)?;
        let mut vmm = pager_vmm().map_err(|_| ContainedSessionError)?;
        establish_pager(&mut vmm, &mut transport).map_err(|_| ContainedSessionError)?;
        return transport
            .send(
                &vmm.terminal(TerminalCode::Internal)
                    .map_err(|_| ContainedSessionError)?,
            )
            .map_err(|_| ContainedSessionError);
    }
    if probe == PagerProbeCase::Consumer {
        return run_lazy_consumer_pager(stream);
    }
    let client = Arc::new(
        PagerClient::connect(
            pager_vmm().map_err(|_| ContainedSessionError)?,
            stream,
            PAGER_TIMEOUT,
        )
        .map_err(|_| ContainedSessionError)?,
    );
    match probe {
        PagerProbeCase::Complete => run_complete_pager(&client).map_err(|_| ContainedSessionError),
        PagerProbeCase::Cancel => client
            .cancel(CancelReason::Requested)
            .map_err(|_| ContainedSessionError),
        PagerProbeCase::Terminal | PagerProbeCase::Consumer => Err(ContainedSessionError),
        PagerProbeCase::Wait => {
            let waiting_client = Arc::clone(&client);
            let waiter = std::thread::spawn(move || {
                waiting_client
                    .page(
                        PagerRegionId::new(1).map_err(|_| ContainedSessionError)?,
                        PagerGeneration::new(1).map_err(|_| ContainedSessionError)?,
                        0,
                        PageAccess::Read,
                    )
                    .map_err(|_| ContainedSessionError)
            });
            let deadline = std::time::Instant::now() + PAGER_TIMEOUT;
            while client
                .pending_operations()
                .map_err(|_| ContainedSessionError)?
                == 0
            {
                if std::time::Instant::now() >= deadline {
                    return Err(ContainedSessionError);
                }
                std::thread::yield_now();
            }
            println!("{PAGER_READY_LINE}");
            std::io::stdout()
                .flush()
                .map_err(|_| ContainedSessionError)?;
            waiter
                .join()
                .map_err(|_| ContainedSessionError)?
                .map(|_| ())
        }
    }
}

fn verify_vmnet_provider_in_containment(
    session: &ContainedSession,
    probe: VmnetProviderProbeCase,
) -> Result<(), ContainedSessionError> {
    let (session_id, authority, route) = session.vmnet_session_authority()?;
    if route != VmnetBackendRoute::RemoteProvider
        || authority.is_denied()
        || !authority.allows_shared()
        || authority.max_interfaces() != Some(1)
    {
        return Err(ContainedSessionError);
    }
    let grant = session
        .vmnet_provider_grant_authority()
        .ok_or(ContainedSessionError)?;
    let source = RemoteVmnetProviderSource::new(session_id, authority, grant)
        .ok_or(ContainedSessionError)?;
    if probe == VmnetProviderProbeCase::Unused {
        return Ok(());
    }

    let config = VmnetInterfaceConfig::shared().with_mtu(Some(1500));
    let (mut backend, mut interface) = StartedVmnetPacketIoBackend::start(
        ProcessVmnetBackendSource::Remote(source).new_backend(),
        &config,
    )
    .map_err(|_| ContainedSessionError)?;
    if backend.parameters().effective_mtu() != 1500
        || backend.parameters().maximum_packet_size() != 2048
        || backend.parameters().read_max_packets() != Some(4)
        || backend.parameters().write_max_packets() != Some(4)
    {
        return Err(ContainedSessionError);
    }

    let readiness = Arc::new(AtomicBool::new(false));
    let published = Arc::clone(&readiness);
    backend
        .enable_packet_available_callback(VmnetPacketAvailableCallback::new(move |estimate| {
            if estimate == Some(1) {
                published.store(true, Ordering::Release);
            }
        }))
        .map_err(|_| ContainedSessionError)?;

    let write_bytes = [0x5a_u8; 60];
    let mut write = VmnetWritePacket::new(&write_bytes).map_err(|_| ContainedSessionError)?;
    backend
        .write_packet(&mut interface, &mut write)
        .map_err(|_| ContainedSessionError)?;

    let mut read_bytes = [0_u8; 2048];
    let mut read = VmnetReadPacket::new(&mut read_bytes).map_err(|_| ContainedSessionError)?;
    let read_length = backend
        .read_packet(&mut interface, &mut read)
        .map_err(|_| ContainedSessionError)?
        .ok_or(ContainedSessionError)?;
    drop(read);
    if read_length != 60
        || read_bytes.get(..read_length) != Some(&[0xa5_u8; 60][..])
        || !readiness.load(Ordering::Acquire)
    {
        return Err(ContainedSessionError);
    }
    backend.stop().map_err(|_| ContainedSessionError)
}

fn run_lazy_consumer_pager(
    stream: std::os::unix::net::UnixStream,
) -> Result<(), ContainedSessionError> {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no pointer arguments.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size = u32::try_from(page_size).map_err(|_| ContainedSessionError)?;
    let pager_limits = PagerLimits::new(
        page_size,
        1,
        4,
        u32::try_from(MAX_FRAME_BYTES).map_err(|_| ContainedSessionError)?,
        PagerOperations::v1(),
    )
    .map_err(|_| ContainedSessionError)?;
    let limits =
        LazyGuestMemoryLimits::new(pager_limits, 2, 4).map_err(|_| ContainedSessionError)?;
    let length = u64::from(page_size)
        .checked_mul(2)
        .ok_or(ContainedSessionError)?;
    let guest_range = bangbang_runtime::memory::GuestMemoryRange::new(
        GuestAddress::new(aarch64::DRAM_MEM_START),
        length,
    )
    .map_err(|_| ContainedSessionError)?;
    let region = LazyGuestMemoryRegion::new(
        PagerRegionId::new(1).map_err(|_| ContainedSessionError)?,
        guest_range,
        0,
        page_size,
    )
    .map_err(|_| ContainedSessionError)?;
    let memory =
        Arc::new(LazyGuestMemory::new(limits, vec![region]).map_err(|_| ContainedSessionError)?);
    let raw = memory
        .mapping_regions()
        .first()
        .ok_or(ContainedSessionError)?
        .host_address()
        .as_ptr()
        .cast::<u8>();
    let pager = Arc::new(
        HvfLazyPager::connect(Arc::clone(&memory), stream, PAGER_TIMEOUT)
            .map_err(|_| ContainedSessionError)?,
    );
    let bridge =
        HvfLazyHostFaultBridge::install(Arc::clone(&memory), Arc::<HvfLazyPager>::clone(&pager))
            .map_err(|_| ContainedSessionError)?;
    // SAFETY: the bridge above protects these exact mappings and remains live
    // until after this non-cloneable view, every local atomic lease, and all
    // synchronous consumer operations have been dropped.
    let mut consumer =
        unsafe { memory.claim_protected_consumer() }.map_err(|_| ContainedSessionError)?;
    let guest_base = GuestAddress::new(aarch64::DRAM_MEM_START);

    let mut first = [0_u8; 8];
    consumer
        .memory()
        .read_slice(&mut first, guest_base)
        .map_err(|_| ContainedSessionError)?;
    if first != [REFERENCE_PAGE_BYTE; 8] {
        return Err(ContainedSessionError);
    }
    // SAFETY: the bounded read above resolved and exposed the complete first
    // page while the protected view and bridge remain live.
    if unsafe { std::ptr::read_volatile(raw) } != REFERENCE_PAGE_BYTE {
        return Err(ContainedSessionError);
    }

    let second = guest_base
        .checked_add(u64::from(page_size))
        .ok_or(ContainedSessionError)?;
    consumer
        .memory_mut()
        .write_slice(&[0x5a; 8], second)
        .map_err(|_| ContainedSessionError)?;
    let mut second_bytes = [0_u8; 8];
    consumer
        .memory()
        .read_slice(&mut second_bytes, second)
        .map_err(|_| ContainedSessionError)?;
    if second_bytes != [0x5a; 8] {
        return Err(ContainedSessionError);
    }

    let atomic = consumer
        .memory()
        .atomic_u64(guest_base)
        .map_err(|_| ContainedSessionError)?;
    atomic
        .store_le(0x8877_6655_4433_2211)
        .map_err(|_| ContainedSessionError)?;
    if atomic.load_le() != 0x8877_6655_4433_2211 {
        return Err(ContainedSessionError);
    }
    drop(atomic);

    let descriptor_table = guest_base.checked_add(0x100).ok_or(ContainedSessionError)?;
    let available_ring = guest_base.checked_add(0x200).ok_or(ContainedSessionError)?;
    let queue = VirtqueueAvailableRing::new(descriptor_table, available_ring, 8)
        .map_err(|_| ContainedSessionError)?;
    let used_event = available_ring
        .checked_add(20)
        .ok_or(ContainedSessionError)?;
    consumer
        .memory_mut()
        .write_slice(&0x1234_u16.to_le_bytes(), used_event)
        .map_err(|_| ContainedSessionError)?;
    if queue
        .used_event(consumer.memory())
        .map_err(|_| ContainedSessionError)?
        != 0x1234
    {
        return Err(ContainedSessionError);
    }

    let mut snapshot = Cursor::new(Vec::new());
    let binding = write_snapshot_memory_image(consumer.memory(), &mut snapshot)
        .map_err(|_| ContainedSessionError)?;
    if binding.data_length() != length || snapshot.get_ref().is_empty() {
        return Err(ContainedSessionError);
    }
    if PreparedBlockDevice::preflight_vhost_user_memory(consumer.memory()).is_ok()
        || consumer.memory_mut().enable_dirty_tracking().is_ok()
        || consumer
            .memory_mut()
            .insert_region(
                bangbang_runtime::memory::GuestMemoryRange::new(
                    GuestAddress::new(aarch64::DRAM_MEM_START + length),
                    u64::from(page_size),
                )
                .map_err(|_| ContainedSessionError)?,
            )
            .is_ok()
        || consumer.memory().discard_range(guest_range).is_complete()
    {
        return Err(ContainedSessionError);
    }

    let resolver = bridge.resolver();
    resolver
        .remove_pages(
            PagerRegionId::new(1).map_err(|_| ContainedSessionError)?,
            0,
            u64::from(page_size),
        )
        .map_err(|_| ContainedSessionError)?;
    let mut removed = [0xff_u8; 8];
    consumer
        .memory()
        .read_slice(&mut removed, guest_base)
        .map_err(|_| ContainedSessionError)?;
    if removed != [0; 8] {
        return Err(ContainedSessionError);
    }

    drop(consumer);
    bridge.shutdown().map_err(|_| ContainedSessionError)?;
    pager.shutdown().map_err(|_| ContainedSessionError)
}

fn pager_vmm() -> Result<VmmSession, PagerError> {
    let limits = PagerLimits::new(
        MIN_PAGE_SIZE,
        1,
        4,
        u32::try_from(MAX_FRAME_BYTES).map_err(|_| PagerError::InvalidConfiguration)?,
        PagerOperations::v1(),
    )?;
    let region = PagerRegion::new(
        PagerRegionId::new(1)?,
        0,
        u64::from(MIN_PAGE_SIZE) * 2,
        MIN_PAGE_SIZE,
    )?;
    VmmSession::new(
        PagerSessionId::from_bytes([0x51; 32])?,
        limits,
        vec![region],
    )
}

fn establish_pager(vmm: &mut VmmSession, transport: &mut PagerTransport) -> Result<(), PagerError> {
    transport.send(&vmm.hello()?)?;
    vmm.receive(transport.receive()?)?;
    transport.send(&vmm.next_region()?)?;
    transport.send(&vmm.start()?)?;
    vmm.receive(transport.receive()?)?;
    if vmm.state() != PagerVmmState::Active || vmm.selected_limits() != Some(vmm.offered_limits()) {
        return Err(PagerError::InvalidPeerState);
    }
    Ok(())
}

fn run_complete_pager(client: &PagerClient) -> Result<(), PagerError> {
    let region = PagerRegionId::new(1)?;
    let first = client.page(region, PagerGeneration::new(1)?, 0, PageAccess::Read)?;
    if first != PagerClientPage::Data(vec![REFERENCE_PAGE_BYTE; MIN_PAGE_SIZE as usize]) {
        return Err(PagerError::InvalidPeerState);
    }
    if client.page(
        region,
        PagerGeneration::new(2)?,
        u64::from(MIN_PAGE_SIZE),
        PageAccess::Read,
    )? != PagerClientPage::Zero
    {
        return Err(PagerError::InvalidPeerState);
    }
    client.remove(
        region,
        PagerGeneration::new(3)?,
        0,
        u64::from(MIN_PAGE_SIZE),
    )?;
    if client.page(region, PagerGeneration::new(4)?, 0, PageAccess::Read)? != PagerClientPage::Zero
    {
        return Err(PagerError::InvalidPeerState);
    }
    client.shutdown()
}

fn verify_block_control_in_containment(
    authority: &crate::contained_session::GrantAuthority,
) -> Result<(), ContainedSessionError> {
    let grant_id = GrantId::parse("probe-block-control").map_err(|_| ContainedSessionError)?;
    let granted = authority.with_registry(|registry| {
        registry
            .duplicate_drive_backing(&grant_id, GrantAccess::ReadWrite)
            .map_err(|_| ContainedSessionError)
    })?;
    if granted.kind() != GrantObjectKind::BlockDevice {
        return Err(ContainedSessionError);
    }
    let authenticated = granted.block_device().ok_or(ContainedSessionError)?;
    let block_size =
        usize::try_from(authenticated.logical_block_size()).map_err(|_| ContainedSessionError)?;
    if block_size < BLOCK_CONTROL_INITIAL_MARKER.len()
        || block_size < BLOCK_CONTROL_WRITTEN_MARKER.len()
        || authenticated.capacity()
            < u64::from(authenticated.logical_block_size())
                .saturating_mul(BLOCK_CONTROL_WRITE_BLOCK.saturating_add(1))
    {
        return Err(ContainedSessionError);
    }
    let direct = File::from(granted.into_owned_fd());
    let mut initial = vec![0_u8; block_size];
    direct
        .read_exact_at(&mut initial, 0)
        .map_err(|_| ContainedSessionError)?;
    if initial.get(..BLOCK_CONTROL_INITIAL_MARKER.len()) != Some(BLOCK_CONTROL_INITIAL_MARKER)
        || initial
            .get(BLOCK_CONTROL_INITIAL_MARKER.len()..)
            .is_none_or(|remainder| remainder.iter().any(|byte| *byte != 0))
    {
        return Err(ContainedSessionError);
    }
    let mut written = vec![0_u8; block_size];
    written
        .get_mut(..BLOCK_CONTROL_WRITTEN_MARKER.len())
        .ok_or(ContainedSessionError)?
        .copy_from_slice(BLOCK_CONTROL_WRITTEN_MARKER);
    let write_offset = u64::from(authenticated.logical_block_size())
        .checked_mul(BLOCK_CONTROL_WRITE_BLOCK)
        .ok_or(ContainedSessionError)?;
    direct
        .write_all_at(&written, write_offset)
        .map_err(|_| ContainedSessionError)?;
    drop(direct);

    let mut claim = authority
        .prepare_drive_backing_claim(Path::new(BLOCK_CONTROL_GRANT_REF), GrantAccess::ReadWrite)
        .map_err(|_| ContainedSessionError)?
        .ok_or(ContainedSessionError)?;
    let backing = claim
        .take_backing(false)
        .map_err(|_| ContainedSessionError)?;
    let geometry = backing
        .kind()
        .block_geometry()
        .ok_or(ContainedSessionError)?;
    let block_size =
        usize::try_from(geometry.logical_block_size()).map_err(|_| ContainedSessionError)?;
    if geometry.logical_block_size() != authenticated.logical_block_size()
        || geometry.block_count() != authenticated.block_count()
        || backing.len() != authenticated.capacity()
    {
        return Err(ContainedSessionError);
    }

    let mut observed = vec![0_u8; block_size];
    backing
        .read_at(write_offset, &mut observed)
        .map_err(|_| ContainedSessionError)?;
    if observed != written {
        return Err(ContainedSessionError);
    }

    backing
        .snapshot_identity()
        .map_err(|_| ContainedSessionError)?;
    backing.flush().map_err(|_| ContainedSessionError)?;
    backing
        .snapshot_identity()
        .map_err(|_| ContainedSessionError)?;
    claim.commit();
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

fn verify_restore_transaction_in_containment(
    session: &ContainedSession,
    probe: RestoreProbeCase,
) -> Result<(), ContainedSessionError> {
    let authority = session
        .snapshot_restore_authority()?
        .ok_or(ContainedSessionError)?;
    match probe {
        RestoreProbeCase::LogicalMismatch => {
            verify_restore_logical_mismatch(&authority)?;
        }
        RestoreProbeCase::ReservationAbort => {
            verify_restore_reservation_abort(&authority)?;
        }
        RestoreProbeCase::Cancellation => {
            verify_restore_cancellation(&authority)?;
        }
        RestoreProbeCase::Success => {
            let (backing, active_guard) = consume_restore_transaction(session, &authority)?;
            drop(active_guard);
            drop(backing);
        }
        RestoreProbeCase::HoldActive => {
            let (backing, active_guard) = consume_restore_transaction(session, &authority)?;
            println!("{RESTORE_ACTIVE_READY_LINE}");
            std::io::stdout()
                .flush()
                .map_err(|_| ContainedSessionError)?;
            let outcome = wait_for_restore_shutdown(session);
            drop(active_guard);
            drop(backing);
            outcome?;
        }
        RestoreProbeCase::HoldPrepared => {
            let prepared = exact_restore_request(RESTORE_ROOT_REF, RESTORE_VSOCK_REF)?
                .prepare(Some(&authority), || session.was_cancelled())
                .map_err(redacted_restore_resource_error)?;
            println!("{RESTORE_PREPARED_READY_LINE}");
            std::io::stdout()
                .flush()
                .map_err(|_| ContainedSessionError)?;
            let outcome = wait_for_restore_shutdown(session);
            drop(prepared);
            outcome?;
        }
        RestoreProbeCase::WaitThenSuccess => {
            println!("{RESTORE_REPLACE_READY_LINE}");
            std::io::stdout()
                .flush()
                .map_err(|_| ContainedSessionError)?;
            let mut release = [0_u8; 1];
            std::io::stdin()
                .read_exact(&mut release)
                .map_err(|_| ContainedSessionError)?;
            let (backing, active_guard) = consume_restore_transaction(session, &authority)?;
            drop(active_guard);
            drop(backing);
        }
    }
    Ok(())
}

fn wait_for_restore_shutdown(session: &ContainedSession) -> Result<(), ContainedSessionError> {
    loop {
        match session.shutdown_requested() {
            Ok(false) => std::thread::park_timeout(Duration::from_millis(10)),
            Ok(true) => return Ok(()),
            Err(_) => return Err(ContainedSessionError),
        }
    }
}

fn consume_restore_transaction(
    session: &ContainedSession,
    authority: &ContainedSnapshotRestoreAuthority,
) -> Result<(BlockFileBacking, ActiveVsockRestoreGuard), ContainedSessionError> {
    let prepared = exact_restore_request(RESTORE_ROOT_REF, RESTORE_VSOCK_REF)?
        .prepare(Some(authority), || session.was_cancelled())
        .map_err(redacted_restore_resource_error)?;
    let (root, vsock) = prepared
        .into_root_and_optional_vsock()
        .map_err(redacted_restore_resource_error)?;
    let (backing, completion) = root.into_parts();
    let Some(vsock) = vsock else {
        drop(backing);
        let _ = completion.abort();
        return Err(ContainedSessionError);
    };

    let mut root_marker = vec![0_u8; RESTORE_ROOT_MARKER.len()];
    if backing.read_at(0, &mut root_marker).is_err() || root_marker != RESTORE_ROOT_MARKER {
        abort_taken_restore(backing, completion, Some(vsock));
        return Err(ContainedSessionError);
    }

    let adoption = vsock.adopt(|resource| {
        if resource.captured_selector().path() != Path::new(RESTORE_VSOCK_REF)
            || resource.destination_selector().path() != Path::new(RESTORE_VSOCK_REF)
        {
            return Err(());
        }
        resource.consume_for_test().map_err(|_| ())
    });
    let active_guard = match adoption {
        Ok(((), active_guard)) => active_guard,
        Err(source) => {
            let redacted = ensure_restore_diagnostic_redacted(&source);
            drop(backing);
            let _ = completion.abort();
            redacted?;
            return Err(ContainedSessionError);
        }
    };
    if let Err(source) = completion.commit() {
        let redacted = ensure_restore_diagnostic_redacted(&source);
        drop(active_guard);
        drop(backing);
        redacted?;
        return Err(ContainedSessionError);
    }

    match exact_restore_request(RESTORE_ROOT_REF, RESTORE_VSOCK_REF)?
        .prepare(Some(authority), || false)
    {
        Err(source) => {
            ensure_restore_diagnostic_redacted(&source)?;
        }
        Ok(unexpected) => {
            drop(unexpected);
            drop(active_guard);
            drop(backing);
            return Err(ContainedSessionError);
        }
    }
    Ok((backing, active_guard))
}

fn abort_taken_restore(
    backing: BlockFileBacking,
    completion: PreparedSnapshotRootRestoreCompletion,
    vsock: Option<PreparedVsockRestoreResource>,
) {
    if let Some(vsock) = vsock {
        let _ = vsock.abort();
    }
    drop(backing);
    let _ = completion.abort();
}

fn verify_restore_reservation_abort(
    authority: &ContainedSnapshotRestoreAuthority,
) -> Result<(), ContainedSessionError> {
    for _ in 0..2 {
        authority
            .prepare(
                Path::new(RESTORE_ROOT_REF),
                Some(Path::new(RESTORE_VSOCK_REF)),
                &|| false,
            )
            .map_err(redacted_contained_restore_error)?
            .abort()
            .map_err(redacted_contained_restore_error)?;
    }
    Ok(())
}

fn verify_restore_cancellation(
    authority: &ContainedSnapshotRestoreAuthority,
) -> Result<(), ContainedSessionError> {
    for cancellation_check in 1..=9 {
        let checks = Cell::new(0);
        let result = authority.prepare(
            Path::new(RESTORE_ROOT_REF),
            Some(Path::new(RESTORE_VSOCK_REF)),
            &|| {
                let next = checks.get() + 1;
                checks.set(next);
                next == cancellation_check
            },
        );
        let error = match result {
            Err(error) => error,
            Ok(unexpected) => {
                let _ = unexpected.abort();
                return Err(ContainedSessionError);
            }
        };
        ensure_restore_diagnostic_redacted(&error)?;
        if error.kind() != ContainedSnapshotRestoreErrorKind::Cancelled
            || error.is_terminal()
            || error.cleanup_failed()
        {
            return Err(ContainedSessionError);
        }
    }
    verify_restore_reservation_abort(authority)
}

fn verify_restore_logical_mismatch(
    authority: &ContainedSnapshotRestoreAuthority,
) -> Result<(), ContainedSessionError> {
    let device_key = restore_device_key()?;
    let root_key = restore_key(
        device_key,
        RESTORE_ROOT_ID,
        SnapshotRestoreResourceClass::BlockBacking,
    )?;
    let vsock_key = restore_key(
        device_key,
        RESTORE_VSOCK_ID,
        SnapshotRestoreResourceClass::VsockEndpoint,
    )?;

    let missing_root =
        RequestedSnapshotRestoreResources::try_from_exact_requests(vec![restore_vsock_owner(
            vsock_key.clone(),
            RESTORE_VSOCK_REF,
        )?]);
    if missing_root.is_ok() {
        return Err(ContainedSessionError);
    }

    let extra_root = RequestedSnapshotRestoreResources::try_from_exact_requests(vec![
        restore_root_owner(root_key.clone(), RESTORE_ROOT_REF),
        restore_root_owner(
            restore_key(
                device_key,
                "restore-extra-1601",
                SnapshotRestoreResourceClass::BlockBacking,
            )?,
            RESTORE_ROOT_REF,
        ),
        restore_vsock_owner(vsock_key.clone(), RESTORE_VSOCK_REF)?,
    ]);
    if extra_root.is_ok() {
        return Err(ContainedSessionError);
    }

    let duplicate_vsock = RequestedSnapshotRestoreResources::try_from_exact_requests(vec![
        restore_root_owner(root_key.clone(), RESTORE_ROOT_REF),
        restore_vsock_owner(vsock_key.clone(), RESTORE_VSOCK_REF)?,
        restore_vsock_owner(vsock_key.clone(), RESTORE_VSOCK_REF)?,
    ]);
    if duplicate_vsock.is_ok() {
        return Err(ContainedSessionError);
    }

    let class_swap = RequestedSnapshotRestoreResources::try_from_exact_requests(vec![
        restore_root_owner(
            restore_key(
                device_key,
                RESTORE_ROOT_ID,
                SnapshotRestoreResourceClass::VsockEndpoint,
            )?,
            RESTORE_ROOT_REF,
        ),
        restore_vsock_owner(
            restore_key(
                device_key,
                RESTORE_VSOCK_ID,
                SnapshotRestoreResourceClass::BlockBacking,
            )?,
            RESTORE_VSOCK_REF,
        )?,
    ]);
    if class_swap.is_ok() {
        return Err(ContainedSessionError);
    }

    let alias_root = restore_key(
        device_key,
        "restore-alias-1601",
        SnapshotRestoreResourceClass::BlockBacking,
    )?;
    let alias_vsock = restore_key(
        device_key,
        "restore-alias-1601",
        SnapshotRestoreResourceClass::VsockEndpoint,
    )?;
    let alias = RequestedSnapshotRestoreResources::try_from_exact_requests(vec![
        restore_root_owner(alias_root, RESTORE_ROOT_REF),
        restore_vsock_owner(alias_vsock, RESTORE_VSOCK_REF)?,
    ]);
    if alias.is_ok() {
        return Err(ContainedSessionError);
    }

    let substituted = exact_restore_request(
        "bangbang-grant:restore-substituted-root-1601",
        "bangbang-grant:restore-substituted-vsock-1601/restore-substituted-1601.sock",
    )?
    .prepare(Some(authority), || false);
    match substituted {
        Err(source) => ensure_restore_diagnostic_redacted(&source)?,
        Ok(unexpected) => {
            drop(unexpected);
            return Err(ContainedSessionError);
        }
    }

    verify_restore_reservation_abort(authority)
}

fn exact_restore_request(
    root_reference: &str,
    vsock_reference: &str,
) -> Result<RequestedSnapshotRestoreResources, ContainedSessionError> {
    let device_key = restore_device_key()?;
    let root = restore_root_owner(
        restore_key(
            device_key,
            RESTORE_ROOT_ID,
            SnapshotRestoreResourceClass::BlockBacking,
        )?,
        root_reference,
    );
    let vsock = restore_vsock_owner(
        restore_key(
            device_key,
            RESTORE_VSOCK_ID,
            SnapshotRestoreResourceClass::VsockEndpoint,
        )?,
        vsock_reference,
    )?;
    RequestedSnapshotRestoreResources::try_from_exact_requests(vec![root, vsock])
        .map_err(redacted_restore_resource_error)
}

fn restore_root_owner(
    key: SnapshotRestoreResourceKey,
    reference: &str,
) -> RequestedSnapshotRestoreResource {
    RequestedSnapshotRestoreResource::Root {
        key,
        selector: Path::new(reference).to_path_buf(),
    }
}

fn restore_vsock_owner(
    key: SnapshotRestoreResourceKey,
    reference: &str,
) -> Result<RequestedSnapshotRestoreResource, ContainedSessionError> {
    let selector = VsockBackendSelector::try_from_path(Path::new(reference))
        .map_err(|_| ContainedSessionError)?;
    let request = RequestedVsockRestoreResource::resolve(Some(&selector), None)
        .map_err(|source| {
            let _ = ensure_restore_diagnostic_redacted(&source);
            ContainedSessionError
        })?
        .ok_or(ContainedSessionError)?;
    Ok(RequestedSnapshotRestoreResource::Vsock { key, request })
}

fn restore_key(
    device_key: SnapshotV2DeviceKey,
    public_id: &str,
    resource_class: SnapshotRestoreResourceClass,
) -> Result<SnapshotRestoreResourceKey, ContainedSessionError> {
    let public_id =
        SnapshotRestorePublicId::try_from(public_id).map_err(|_| ContainedSessionError)?;
    Ok(SnapshotRestoreResourceKey::new(
        device_key,
        public_id,
        resource_class,
    ))
}

fn restore_device_key() -> Result<SnapshotV2DeviceKey, ContainedSessionError> {
    let fixture = include_str!("../../runtime/src/snapshot_device_v2/fixtures/mmio.hex");
    let hex = fixture.trim().as_bytes();
    if !hex.len().is_multiple_of(2) {
        return Err(ContainedSessionError);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(hex.len() / 2)
        .map_err(|_| ContainedSessionError)?;
    for pair in hex.as_chunks::<2>().0 {
        let pair = std::str::from_utf8(pair).map_err(|_| ContainedSessionError)?;
        bytes.push(u8::from_str_radix(pair, 16).map_err(|_| ContainedSessionError)?);
    }
    SnapshotV2DeviceGraph::decode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, &bytes)
        .map(|graph| graph.root_key())
        .map_err(|_| ContainedSessionError)
}

fn redacted_restore_resource_error(
    source: crate::snapshot_restore_resources::SnapshotRestoreResourceError,
) -> ContainedSessionError {
    let _ = ensure_restore_diagnostic_redacted(&source);
    ContainedSessionError
}

fn redacted_contained_restore_error(
    source: crate::contained_session::ContainedSnapshotRestoreError,
) -> ContainedSessionError {
    let _ = ensure_restore_diagnostic_redacted(&source);
    ContainedSessionError
}

fn ensure_restore_diagnostic_redacted(
    source: &(impl std::fmt::Debug + std::fmt::Display),
) -> Result<(), ContainedSessionError> {
    let diagnostic = format!("{source:?} {source}");
    let root_marker =
        std::str::from_utf8(RESTORE_ROOT_MARKER).map_err(|_| ContainedSessionError)?;
    for sensitive in [
        RESTORE_ROOT_ID,
        RESTORE_VSOCK_ID,
        RESTORE_ROOT_REF,
        RESTORE_VSOCK_REF,
        "restore-1601.sock",
        "restore-extra-1601",
        "restore-alias-1601",
        "restore-substituted-root-1601",
        "restore-substituted-vsock-1601",
        "restore-substituted-1601.sock",
        root_marker,
    ] {
        if diagnostic.contains(sensitive) {
            return Err(ContainedSessionError);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreProbeCase {
    LogicalMismatch,
    ReservationAbort,
    Cancellation,
    Success,
    HoldActive,
    HoldPrepared,
    WaitThenSuccess,
}

impl RestoreProbeCase {
    fn parse(args: &[OsString]) -> Result<Option<Self>, ContainedSessionError> {
        let [option, value] = args else {
            return Err(ContainedSessionError);
        };
        if option != OPTION {
            return Err(ContainedSessionError);
        }
        Ok(match value.to_str() {
            Some("restore-logical-mismatch") => Some(Self::LogicalMismatch),
            Some("restore-reservation-abort") => Some(Self::ReservationAbort),
            Some("restore-cancellation") => Some(Self::Cancellation),
            Some("restore-success") => Some(Self::Success),
            Some("restore-hold-active") => Some(Self::HoldActive),
            Some("restore-hold-prepared") => Some(Self::HoldPrepared),
            Some("restore-wait-then-success") => Some(Self::WaitThenSuccess),
            Some(value) if value.starts_with("restore-") => return Err(ContainedSessionError),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PagerProbeCase {
    Complete,
    Consumer,
    Cancel,
    Terminal,
    Wait,
}

impl PagerProbeCase {
    fn parse(args: &[OsString]) -> Result<Option<Self>, ContainedSessionError> {
        let [option, value] = args else {
            return Err(ContainedSessionError);
        };
        if option != OPTION {
            return Err(ContainedSessionError);
        }
        Ok(match value.to_str() {
            Some("pager-complete") => Some(Self::Complete),
            Some("pager-consumer") => Some(Self::Consumer),
            Some("pager-cancel") => Some(Self::Cancel),
            Some("pager-terminal") => Some(Self::Terminal),
            Some("pager-wait") => Some(Self::Wait),
            Some(value) if value.starts_with("pager-") => return Err(ContainedSessionError),
            _ => None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VmnetProviderProbeCase {
    Complete,
    Unused,
}

impl VmnetProviderProbeCase {
    fn parse(args: &[OsString]) -> Result<Option<Self>, ContainedSessionError> {
        let [option, value] = args else {
            return Err(ContainedSessionError);
        };
        if option != OPTION {
            return Err(ContainedSessionError);
        }
        Ok(match value.to_str() {
            Some("vmnet-provider-complete") => Some(Self::Complete),
            Some("vmnet-provider-unused") => Some(Self::Unused),
            Some(value) if value.starts_with("vmnet-provider-") => {
                return Err(ContainedSessionError);
            }
            _ => None,
        })
    }
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
            Some("block-control") => Ok(Self {
                name: "block-control",
                hold: false,
                expected_no_file: 2048,
                expected_file_size: None,
            }),
            Some("target-runtime") => Ok(Self {
                name: "target-runtime",
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

    fn verifies_block_control(self) -> bool {
        self.name == "block-control"
    }

    fn verifies_target_runtime(self) -> bool {
        self.name == "target-runtime"
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: std::path::PathBuf,
        anchor: File,
    }

    impl TestDirectory {
        fn new() -> Self {
            loop {
                let suffix = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
                let path = std::path::PathBuf::from("/tmp").join(format!(
                    "bangbang-exact-grant-child-{}-{suffix}",
                    std::process::id()
                ));
                match fs::create_dir(&path) {
                    Ok(()) => {
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                            .expect("test directory mode should set");
                        let anchor = File::open(&path).expect("test directory should open");
                        return Self { path, anchor };
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("test directory should create: {error}"),
                }
            }
        }

        fn child(&self) -> std::path::PathBuf {
            self.path.join("owned-child")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            if self.child().exists() {
                fs::remove_file(self.child()).expect("test child should clean");
            }
            fs::remove_dir(&self.path).expect("test directory should clean");
        }
    }

    #[test]
    fn exact_grant_child_removes_success_and_failure_paths() {
        let directory = TestDirectory::new();
        let mut child = ExactGrantChild::create(directory.anchor.as_raw_fd(), "owned-child")
            .expect("exact child should create");
        child
            .file
            .write_all(b"complete")
            .expect("exact child should write");
        let source = child.file.metadata().expect("source metadata should read");
        let named = fs::symlink_metadata(directory.child()).expect("named metadata should read");
        assert_eq!(
            (source.dev(), source.ino(), source.uid(), source.gid()),
            (named.dev(), named.ino(), named.uid(), named.gid())
        );
        assert_eq!(named.permissions().mode() & 0o7777, 0o600);
        assert_eq!(named.nlink(), 1);
        let mut source_stat = MaybeUninit::<libc::stat>::uninit();
        let mut named_stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: The source descriptor is live and the output is writable.
        let source_status =
            unsafe { libc::fstat(child.file.as_raw_fd(), source_stat.as_mut_ptr()) };
        assert_eq!(source_status, 0);
        // SAFETY: The anchor/name are live and the output is writable.
        let named_status = unsafe {
            libc::fstatat(
                directory.anchor.as_raw_fd(),
                child.name.as_ptr(),
                named_stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        assert_eq!(named_status, 0);
        // SAFETY: Both successful stat calls initialized their outputs.
        let (source_stat, named_stat) =
            unsafe { (source_stat.assume_init(), named_stat.assume_init()) };
        assert_eq!(
            (
                source_stat.st_dev,
                source_stat.st_ino,
                source_stat.st_mode & 0o7777,
                source_stat.st_uid,
                source_stat.st_gid,
                source_stat.st_nlink,
            ),
            (
                named_stat.st_dev,
                named_stat.st_ino,
                named_stat.st_mode & 0o7777,
                named_stat.st_uid,
                named_stat.st_gid,
                named_stat.st_nlink,
            )
        );
        assert!(
            child
                .current_name_is_exact()
                .expect("exact name validation should run")
        );
        child.remove().expect("exact child should remove");
        assert!(!directory.child().exists());

        let mut child = ExactGrantChild::create(directory.anchor.as_raw_fd(), "owned-child")
            .expect("failure-path child should create");
        child
            .file
            .write_all(b"partial")
            .expect("failure-path child should write");
        drop(child);
        assert!(!directory.child().exists());
    }

    #[test]
    fn exact_grant_child_drop_refuses_a_replacement() {
        let directory = TestDirectory::new();
        let child = ExactGrantChild::create(directory.anchor.as_raw_fd(), "owned-child")
            .expect("exact child should create");
        fs::remove_file(directory.child()).expect("original child should unlink");
        fs::write(directory.child(), b"replacement").expect("replacement should create");

        drop(child);

        assert_eq!(
            fs::read(directory.child()).expect("replacement should remain"),
            b"replacement"
        );
    }
}
