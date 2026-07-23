use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use bangbang_pager::{
    CancelReason, MAX_FRAME_BYTES, MIN_PAGE_SIZE, PageAccess, PagerFrameKind, PagerGeneration,
    PagerLimits, PagerOperations, PagerPeerState, PagerRegion, PagerRegionId, PagerSessionId,
    PagerTransport, PagerVmmState, PeerSession, VmmSession,
};

const CHILD_FD_ENV: &str = "BANGBANG_PAGER_TEST_FD";
const CHILD_MODE_ENV: &str = "BANGBANG_PAGER_TEST_MODE";
const MODE_COMPLETE: &str = "complete";
const MODE_CANCEL: &str = "cancel";

#[test]
fn inherited_connected_stream_completes_data_zero_removal_and_shutdown() {
    let (mut vmm, mut transport, child) =
        spawn_peer(MODE_COMPLETE).expect("external peer should spawn");
    establish(&mut vmm, &mut transport).expect("session should establish");

    let generation = PagerGeneration::new(1).expect("generation should be nonzero");
    let first = vmm
        .request_page(
            PagerRegionId::new(1).expect("region should be nonzero"),
            generation,
            0,
            PageAccess::Read,
        )
        .expect("first request should build");
    transport.send(&first).expect("first request should send");
    let second = vmm
        .request_page(
            PagerRegionId::new(1).expect("region should be nonzero"),
            generation,
            u64::from(MIN_PAGE_SIZE),
            PageAccess::Write,
        )
        .expect("second request should build");
    transport.send(&second).expect("second request should send");

    let zero = vmm
        .receive(transport.receive().expect("zero response should arrive"))
        .expect("zero response should validate");
    assert_eq!(zero.kind(), PagerFrameKind::PageZero);
    let data = vmm
        .receive(transport.receive().expect("data response should arrive"))
        .expect("data response should validate");
    assert_eq!(data.kind(), PagerFrameKind::PageData);
    assert_eq!(
        data.page_data().expect("page data should exist"),
        vec![0x5a; MIN_PAGE_SIZE as usize]
    );

    let removal = vmm
        .remove(
            PagerRegionId::new(1).expect("region should be nonzero"),
            generation,
            0,
            u64::from(MIN_PAGE_SIZE),
        )
        .expect("removal should build");
    transport.send(&removal).expect("removal should send");
    let removed = vmm
        .receive(transport.receive().expect("removal ack should arrive"))
        .expect("removal ack should validate");
    assert_eq!(removed.kind(), PagerFrameKind::Removed);

    let shutdown = vmm.shutdown().expect("drained shutdown should build");
    transport.send(&shutdown).expect("shutdown should send");
    vmm.receive(transport.receive().expect("shutdown ack should arrive"))
        .expect("shutdown ack should validate");
    assert_eq!(vmm.state(), PagerVmmState::Closed);
    assert_child_success(child).expect("external peer should succeed");
}

#[test]
fn inherited_connected_stream_completes_terminal_cancellation() {
    let (mut vmm, mut transport, child) =
        spawn_peer(MODE_CANCEL).expect("external peer should spawn");
    establish(&mut vmm, &mut transport).expect("session should establish");

    let request = vmm
        .request_page(
            PagerRegionId::new(1).expect("region should be nonzero"),
            PagerGeneration::new(1).expect("generation should be nonzero"),
            0,
            PageAccess::Read,
        )
        .expect("request should build");
    transport.send(&request).expect("request should send");
    let cancel = vmm
        .cancel(CancelReason::Requested)
        .expect("cancellation should build");
    transport.send(&cancel).expect("cancellation should send");
    vmm.receive(transport.receive().expect("cancel ack should arrive"))
        .expect("cancel ack should validate");
    assert_eq!(vmm.state(), PagerVmmState::Closed);
    assert_eq!(vmm.outstanding_count(), 0);
    assert_child_success(child).expect("external peer should succeed");
}

#[test]
fn pager_peer_child() {
    let Ok(raw_fd) = std::env::var(CHILD_FD_ENV) else {
        return;
    };
    let descriptor: RawFd = raw_fd.parse().expect("child descriptor should be numeric");
    let mode = std::env::var(CHILD_MODE_ENV).expect("child mode should be present");
    // SAFETY: The parent deliberately clears close-on-exec for exactly this
    // connected endpoint, passes its live numeric descriptor, and drops its
    // own copy after spawn. This child adopts that one inherited copy once.
    let stream = unsafe { UnixStream::from_raw_fd(descriptor) };
    let mut transport =
        PagerTransport::new(stream, Duration::from_secs(5)).expect("transport should initialize");
    let mut peer = PeerSession::new();

    peer.receive(transport.receive().expect("hello should arrive"))
        .expect("hello should validate");
    let selected = peer.offered_limits().expect("hello should carry limits");
    transport
        .send(&peer.hello_ack(selected).expect("hello ack should build"))
        .expect("hello ack should send");
    peer.receive(transport.receive().expect("region should arrive"))
        .expect("region should validate");
    peer.receive(transport.receive().expect("start should arrive"))
        .expect("start should validate");
    transport
        .send(&peer.ready().expect("ready should build"))
        .expect("ready should send");

    match mode.as_str() {
        MODE_COMPLETE => {
            run_complete_peer(&mut peer, &mut transport).expect("complete exchange should succeed");
        }
        MODE_CANCEL => {
            run_cancel_peer(&mut peer, &mut transport).expect("cancel exchange should succeed");
        }
        _ => panic!("unexpected child mode"),
    }
}

fn run_complete_peer(
    peer: &mut PeerSession,
    transport: &mut PagerTransport,
) -> Result<(), bangbang_pager::PagerError> {
    let first = peer.receive(transport.receive()?)?;
    let first_id = first
        .page_request()
        .ok_or(bangbang_pager::PagerError::InvalidFrame)?
        .request();
    let second = peer.receive(transport.receive()?)?;
    let second_id = second
        .page_request()
        .ok_or(bangbang_pager::PagerError::InvalidFrame)?
        .request();

    transport.send(&peer.page_zero(second_id)?)?;
    transport.send(&peer.page_data(first_id, vec![0x5a; MIN_PAGE_SIZE as usize])?)?;

    let removal = peer.receive(transport.receive()?)?;
    let removal_id = removal
        .remove_request()
        .ok_or(bangbang_pager::PagerError::InvalidFrame)?
        .request();
    transport.send(&peer.removed(removal_id)?)?;

    peer.receive(transport.receive()?)?;
    transport.send(&peer.shutdown_ack()?)?;
    assert_eq!(peer.state(), PagerPeerState::Closed);
    Ok(())
}

fn run_cancel_peer(
    peer: &mut PeerSession,
    transport: &mut PagerTransport,
) -> Result<(), bangbang_pager::PagerError> {
    peer.receive(transport.receive()?)?;
    assert_eq!(peer.outstanding_count(), 1);
    peer.receive(transport.receive()?)?;
    assert_eq!(peer.outstanding_count(), 0);
    transport.send(&peer.cancelled()?)?;
    assert_eq!(peer.state(), PagerPeerState::Closed);
    Ok(())
}

#[derive(Debug)]
struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn wait_with_output(mut self) -> io::Result<Output> {
        self.child
            .take()
            .ok_or_else(|| io::Error::other("pager child was already consumed"))?
            .wait_with_output()
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn spawn_peer(mode: &str) -> io::Result<(VmmSession, PagerTransport, ChildGuard)> {
    let limits = PagerLimits::new(
        MIN_PAGE_SIZE,
        1,
        4,
        u32::try_from(MAX_FRAME_BYTES).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
        PagerOperations::v1(),
    )
    .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let region_id =
        PagerRegionId::new(1).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let region = PagerRegion::new(region_id, 0, u64::from(MIN_PAGE_SIZE) * 2, MIN_PAGE_SIZE)
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let session = PagerSessionId::from_bytes([9; 32])
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let vmm = VmmSession::new(session, limits, vec![region])
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;

    let (parent, child) = UnixStream::pair()?;
    let descriptor = child.as_raw_fd();
    set_close_on_exec(descriptor, false)?;
    let spawn_result = Command::new(std::env::current_exe()?)
        .args(["--exact", "pager_peer_child", "--nocapture"])
        .env(CHILD_FD_ENV, descriptor.to_string())
        .env(CHILD_MODE_ENV, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let restore_result = set_close_on_exec(descriptor, true);
    let spawned = ChildGuard::new(spawn_result?);
    restore_result?;
    drop(child);
    let transport = PagerTransport::new(parent, Duration::from_secs(5))
        .map_err(|_| io::Error::other("pager transport initialization failed"))?;
    Ok((vmm, transport, spawned))
}

fn establish(
    vmm: &mut VmmSession,
    transport: &mut PagerTransport,
) -> Result<(), bangbang_pager::PagerError> {
    transport.send(&vmm.hello()?)?;
    vmm.receive(transport.receive()?)?;
    transport.send(&vmm.next_region()?)?;
    transport.send(&vmm.start()?)?;
    vmm.receive(transport.receive()?)?;
    assert_eq!(vmm.state(), PagerVmmState::Active);
    Ok(())
}

fn assert_child_success(child: ChildGuard) -> io::Result<()> {
    let output = child.wait_with_output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "external pager peer failed: {}\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn set_close_on_exec(descriptor: RawFd, enabled: bool) -> io::Result<()> {
    let current = retry_fcntl(descriptor, libc::F_GETFD, 0)?;
    let next = if enabled {
        current | libc::FD_CLOEXEC
    } else {
        current & !libc::FD_CLOEXEC
    };
    retry_fcntl(descriptor, libc::F_SETFD, next).map(|_| ())
}

fn retry_fcntl(descriptor: RawFd, command: libc::c_int, argument: libc::c_int) -> io::Result<i32> {
    loop {
        // SAFETY: The command is F_GETFD or F_SETFD with its documented
        // integer argument, and the borrowed descriptor remains live.
        let result = unsafe { libc::fcntl(descriptor, command, argument) };
        if result >= 0 {
            return Ok(result);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
