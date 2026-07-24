//! Concurrent VMM-side ownership for one connected pager session.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::{
    CancelReason, PageAccess, PagerError, PagerFrame, PagerFrameKind, PagerGeneration, PagerLimits,
    PagerRegionId, PagerRequestId, PagerTransport, PagerVmmState, VmmSession,
};

const CLIENT_THREAD_NAME: &str = "bangbang-pager-client";

/// Exact content returned by one validated page response.
#[derive(Clone, PartialEq, Eq)]
pub enum PagerClientPage {
    /// One complete data page.
    Data(Vec<u8>),
    /// One complete all-zero page.
    Zero,
}

impl fmt::Debug for PagerClientPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PagerClientPage(<redacted>)")
    }
}

/// Coarse lifecycle of one connected VMM-side client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerClientState {
    /// Page and removal operations may be submitted.
    Active,
    /// Requested cancellation was sent and its acknowledgement is pending.
    AwaitCancelled,
    /// Drained shutdown was sent and its acknowledgement is pending.
    AwaitShutdownAck,
    /// Requested cancellation or orderly shutdown completed.
    Closed,
    /// One protocol, transport, or worker failure ended the client.
    Terminal,
}

/// Nonblocking observer for the first terminal client failure.
///
/// Implementations must not call back into the same client or perform
/// unbounded work. The observer may run on either a request caller or the
/// receive worker.
pub trait PagerClientTerminalObserver: fmt::Debug + Send + Sync {
    /// Records the stable first terminal failure.
    fn terminal(&self, failure: PagerError);
}

#[derive(Debug)]
struct IgnoreTerminal;

impl PagerClientTerminalObserver for IgnoreTerminal {
    fn terminal(&self, _failure: PagerError) {}
}

enum PendingCompletion {
    Page(SyncSender<Result<PagerClientPage, PagerError>>),
    Removal(SyncSender<Result<(), PagerError>>),
}

impl PendingCompletion {
    fn fail(self, failure: PagerError) {
        match self {
            Self::Page(sender) => {
                let _ = sender.send(Err(failure));
            }
            Self::Removal(sender) => {
                let _ = sender.send(Err(failure));
            }
        }
    }
}

struct PendingOperation {
    deadline: Instant,
    completion: PendingCompletion,
}

enum PendingControl {
    Cancel {
        deadline: Instant,
        completion: SyncSender<Result<(), PagerError>>,
    },
    Shutdown {
        deadline: Instant,
        completion: SyncSender<Result<(), PagerError>>,
    },
}

impl PendingControl {
    const fn deadline(&self) -> Instant {
        match self {
            Self::Cancel { deadline, .. } | Self::Shutdown { deadline, .. } => *deadline,
        }
    }

    fn fail(self, failure: PagerError) {
        match self {
            Self::Cancel { completion, .. } | Self::Shutdown { completion, .. } => {
                let _ = completion.send(Err(failure));
            }
        }
    }
}

struct ClientState {
    vmm: VmmSession,
    status: PagerClientState,
    pending: BTreeMap<PagerRequestId, PendingOperation>,
    control: Option<PendingControl>,
    terminal: Option<PagerError>,
}

struct ClientInner {
    state: Mutex<ClientState>,
    outbound: Mutex<PagerTransport>,
    wakeup_writer: Mutex<UnixStream>,
    shutdown_stream: UnixStream,
    timeout: Duration,
    observer: Arc<dyn PagerClientTerminalObserver>,
}

impl ClientInner {
    fn lock_state(&self) -> Result<MutexGuard<'_, ClientState>, PagerError> {
        self.state.lock().map_err(|_| PagerError::Poisoned)
    }

    fn lock_outbound(&self) -> Result<MutexGuard<'_, PagerTransport>, PagerError> {
        self.outbound.lock().map_err(|_| PagerError::Poisoned)
    }

    fn deadline(&self) -> Result<Instant, PagerError> {
        Instant::now()
            .checked_add(self.timeout)
            .ok_or(PagerError::InvalidConfiguration)
    }

    fn wake_worker(&self) -> Result<(), PagerError> {
        let mut writer = self
            .wakeup_writer
            .lock()
            .map_err(|_| PagerError::Poisoned)?;
        match writer.write(&[1]) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(PagerError::Io(error.kind())),
        }
    }

    fn terminalize(&self, failure: PagerError) {
        let (pending, control) = {
            let mut state = match self.state.lock() {
                Ok(state) => state,
                Err(error) => error.into_inner(),
            };
            if matches!(
                state.status,
                PagerClientState::Closed | PagerClientState::Terminal
            ) {
                return;
            }
            state.status = PagerClientState::Terminal;
            state.terminal = Some(failure);
            (std::mem::take(&mut state.pending), state.control.take())
        };
        let observer = Arc::clone(&self.observer);
        let _ = catch_unwind(AssertUnwindSafe(|| observer.terminal(failure)));
        for operation in pending.into_values() {
            operation.completion.fail(failure);
        }
        if let Some(control) = control {
            control.fail(failure);
        }
        let _ = self.wake_worker();
    }

    fn request_timeout(&self) -> Option<Duration> {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                self.terminalize(PagerError::Poisoned);
                return Some(Duration::ZERO);
            }
        };
        if matches!(
            state.status,
            PagerClientState::Closed | PagerClientState::Terminal
        ) {
            return None;
        }
        let deadline = state
            .pending
            .values()
            .map(|operation| operation.deadline)
            .chain(state.control.as_ref().map(PendingControl::deadline))
            .min();
        deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    fn should_stop(&self) -> bool {
        match self.state.lock() {
            Ok(state) => matches!(
                state.status,
                PagerClientState::Closed | PagerClientState::Terminal
            ),
            Err(_) => true,
        }
    }

    fn process_frame(&self, frame: PagerFrame) -> Result<(), PagerError> {
        let completion = {
            let mut state = self.lock_state()?;
            let frame = state.vmm.receive(frame)?;
            match frame.kind() {
                PagerFrameKind::PageData | PagerFrameKind::PageZero => {
                    let response = frame.page_response().ok_or(PagerError::InvalidPeerState)?;
                    let operation = state
                        .pending
                        .remove(&response.request())
                        .ok_or(PagerError::InvalidPeerState)?;
                    let PendingCompletion::Page(sender) = operation.completion else {
                        return Err(PagerError::InvalidPeerState);
                    };
                    let result = if frame.kind() == PagerFrameKind::PageData {
                        Ok(PagerClientPage::Data(
                            frame
                                .page_data()
                                .ok_or(PagerError::InvalidPeerState)?
                                .to_vec(),
                        ))
                    } else {
                        Ok(PagerClientPage::Zero)
                    };
                    FrameCompletion::Page(sender, result)
                }
                PagerFrameKind::Removed => {
                    let removal = frame.remove_request().ok_or(PagerError::InvalidPeerState)?;
                    let operation = state
                        .pending
                        .remove(&removal.request())
                        .ok_or(PagerError::InvalidPeerState)?;
                    let PendingCompletion::Removal(sender) = operation.completion else {
                        return Err(PagerError::InvalidPeerState);
                    };
                    FrameCompletion::Removal(sender)
                }
                PagerFrameKind::Cancelled => {
                    if state.status != PagerClientState::AwaitCancelled
                        || state.vmm.state() != PagerVmmState::Closed
                    {
                        return Err(PagerError::InvalidPeerState);
                    }
                    let Some(PendingControl::Cancel { completion, .. }) = state.control.take()
                    else {
                        return Err(PagerError::InvalidPeerState);
                    };
                    state.status = PagerClientState::Closed;
                    FrameCompletion::Control(completion)
                }
                PagerFrameKind::ShutdownAck => {
                    if state.status != PagerClientState::AwaitShutdownAck
                        || state.vmm.state() != PagerVmmState::Closed
                    {
                        return Err(PagerError::InvalidPeerState);
                    }
                    let Some(PendingControl::Shutdown { completion, .. }) = state.control.take()
                    else {
                        return Err(PagerError::InvalidPeerState);
                    };
                    state.status = PagerClientState::Closed;
                    FrameCompletion::Control(completion)
                }
                PagerFrameKind::Terminal => return Err(PagerError::PeerTerminal),
                _ => return Err(PagerError::InvalidPeerState),
            }
        };
        completion.complete();
        Ok(())
    }
}

enum FrameCompletion {
    Page(
        SyncSender<Result<PagerClientPage, PagerError>>,
        Result<PagerClientPage, PagerError>,
    ),
    Removal(SyncSender<Result<(), PagerError>>),
    Control(SyncSender<Result<(), PagerError>>),
}

impl FrameCompletion {
    fn complete(self) {
        match self {
            Self::Page(sender, result) => {
                let _ = sender.send(result);
            }
            Self::Removal(sender) | Self::Control(sender) => {
                let _ = sender.send(Ok(()));
            }
        }
    }
}

/// Thread-safe owner for one connected VMM-side `bangbang-pager-v1` session.
pub struct PagerClient {
    inner: Arc<ClientInner>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl PagerClient {
    /// Establishes a session and starts the bounded receive owner.
    pub fn connect(
        session: VmmSession,
        stream: UnixStream,
        timeout: Duration,
    ) -> Result<Self, PagerError> {
        Self::connect_observed(session, stream, timeout, Arc::new(IgnoreTerminal))
    }

    /// Establishes a session with a nonblocking first-terminal observer.
    pub fn connect_observed(
        mut session: VmmSession,
        stream: UnixStream,
        timeout: Duration,
        observer: Arc<dyn PagerClientTerminalObserver>,
    ) -> Result<Self, PagerError> {
        let outbound_stream = stream
            .try_clone()
            .map_err(|error| PagerError::Io(error.kind()))?;
        let shutdown_stream = stream
            .try_clone()
            .map_err(|error| PagerError::Io(error.kind()))?;
        let mut inbound = PagerTransport::new(stream, timeout)?;
        establish(&mut session, &mut inbound)?;
        let outbound = PagerTransport::new(outbound_stream, timeout)?;
        let (wakeup_reader, wakeup_writer) =
            UnixStream::pair().map_err(|error| PagerError::Io(error.kind()))?;
        wakeup_reader
            .set_nonblocking(true)
            .map_err(|error| PagerError::Io(error.kind()))?;
        wakeup_writer
            .set_nonblocking(true)
            .map_err(|error| PagerError::Io(error.kind()))?;

        let inner = Arc::new(ClientInner {
            state: Mutex::new(ClientState {
                vmm: session,
                status: PagerClientState::Active,
                pending: BTreeMap::new(),
                control: None,
                terminal: None,
            }),
            outbound: Mutex::new(outbound),
            wakeup_writer: Mutex::new(wakeup_writer),
            shutdown_stream,
            timeout,
            observer,
        });
        let worker_inner = Arc::clone(&inner);
        let worker = thread::Builder::new()
            .name(CLIENT_THREAD_NAME.to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    receive_loop(&worker_inner, &mut inbound, wakeup_reader)
                }));
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => worker_inner.terminalize(error),
                    Err(_) => worker_inner.terminalize(PagerError::Poisoned),
                }
            })
            .map_err(|error| PagerError::Io(error.kind()))?;
        Ok(Self {
            inner,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Requests one exact page and waits for its validated response.
    pub fn page(
        &self,
        region: PagerRegionId,
        generation: PagerGeneration,
        offset: u64,
        access: PageAccess,
    ) -> Result<PagerClientPage, PagerError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.submit(
            |vmm| vmm.request_page(region, generation, offset, access),
            PendingCompletion::Page(sender),
        )?;
        receiver.recv().map_err(|_| PagerError::Poisoned)?
    }

    /// Sends one exact removal and waits for its validated acknowledgement.
    pub fn remove(
        &self,
        region: PagerRegionId,
        generation: PagerGeneration,
        offset: u64,
        length: u64,
    ) -> Result<(), PagerError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.submit(
            |vmm| vmm.remove(region, generation, offset, length),
            PendingCompletion::Removal(sender),
        )?;
        receiver.recv().map_err(|_| PagerError::Poisoned)?
    }

    /// Cancels all active work and waits for the peer acknowledgement.
    pub fn cancel(&self, reason: CancelReason) -> Result<(), PagerError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let cancelled = {
            let mut outbound = self.inner.lock_outbound()?;
            let mut state = self.inner.lock_state()?;
            if state.status != PagerClientState::Active {
                return Err(state.terminal.unwrap_or(PagerError::InvalidLifecycle));
            }
            let frame = state.vmm.cancel(reason)?;
            let cancelled = std::mem::take(&mut state.pending);
            state.status = PagerClientState::AwaitCancelled;
            state.control = Some(PendingControl::Cancel {
                deadline: self.inner.deadline()?,
                completion: sender,
            });
            if let Err(error) = outbound.send(&frame) {
                drop(state);
                drop(outbound);
                for operation in cancelled.into_values() {
                    operation.completion.fail(PagerError::Cancelled);
                }
                self.inner.terminalize(error);
                return Err(error);
            }
            cancelled
        };
        for operation in cancelled.into_values() {
            operation.completion.fail(PagerError::Cancelled);
        }
        if let Err(error) = self.inner.wake_worker() {
            self.inner.terminalize(error);
        }
        receiver.recv().map_err(|_| PagerError::Poisoned)?
    }

    /// Drains an idle active session through orderly shutdown.
    pub fn shutdown(&self) -> Result<(), PagerError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        {
            let mut outbound = self.inner.lock_outbound()?;
            let mut state = self.inner.lock_state()?;
            if state.status != PagerClientState::Active {
                return Err(state.terminal.unwrap_or(PagerError::InvalidLifecycle));
            }
            let frame = state.vmm.shutdown()?;
            state.status = PagerClientState::AwaitShutdownAck;
            state.control = Some(PendingControl::Shutdown {
                deadline: self.inner.deadline()?,
                completion: sender,
            });
            if let Err(error) = outbound.send(&frame) {
                drop(state);
                drop(outbound);
                self.inner.terminalize(error);
                return Err(error);
            }
        }
        if let Err(error) = self.inner.wake_worker() {
            self.inner.terminalize(error);
        }
        receiver.recv().map_err(|_| PagerError::Poisoned)?
    }

    /// Returns the current coarse client lifecycle.
    pub fn state(&self) -> Result<PagerClientState, PagerError> {
        Ok(self.inner.lock_state()?.status)
    }

    /// Returns the exact peer-selected session limits.
    pub fn selected_limits(&self) -> Result<PagerLimits, PagerError> {
        self.inner
            .lock_state()?
            .vmm
            .selected_limits()
            .ok_or(PagerError::InvalidLifecycle)
    }

    /// Returns the stable first terminal failure, when present.
    pub fn terminal_error(&self) -> Result<Option<PagerError>, PagerError> {
        Ok(self.inner.lock_state()?.terminal)
    }

    /// Returns the number of currently pending page and removal operations.
    pub fn pending_operations(&self) -> Result<usize, PagerError> {
        Ok(self.inner.lock_state()?.pending.len())
    }

    fn submit(
        &self,
        build: impl FnOnce(&mut VmmSession) -> Result<PagerFrame, PagerError>,
        completion: PendingCompletion,
    ) -> Result<(), PagerError> {
        {
            let mut outbound = self.inner.lock_outbound()?;
            let mut state = self.inner.lock_state()?;
            if state.status != PagerClientState::Active {
                return Err(state.terminal.unwrap_or(PagerError::InvalidLifecycle));
            }
            let frame = build(&mut state.vmm)?;
            let request = match frame.kind() {
                PagerFrameKind::PageRequest => frame
                    .page_request()
                    .map(|request| request.request())
                    .ok_or(PagerError::InvalidLifecycle)?,
                PagerFrameKind::Remove => frame
                    .remove_request()
                    .map(|request| request.request())
                    .ok_or(PagerError::InvalidLifecycle)?,
                _ => return Err(PagerError::InvalidLifecycle),
            };
            if state
                .pending
                .insert(
                    request,
                    PendingOperation {
                        deadline: self.inner.deadline()?,
                        completion,
                    },
                )
                .is_some()
            {
                return Err(PagerError::InvalidLifecycle);
            }
            if let Err(error) = outbound.send(&frame) {
                drop(state);
                drop(outbound);
                self.inner.terminalize(error);
                return Err(error);
            }
        }
        if let Err(error) = self.inner.wake_worker() {
            self.inner.terminalize(error);
            return Err(error);
        }
        Ok(())
    }
}

impl fmt::Debug for PagerClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PagerClient(<redacted>)")
    }
}

impl Drop for PagerClient {
    fn drop(&mut self) {
        self.inner.terminalize(PagerError::Poisoned);
        let _ = self
            .inner
            .shutdown_stream
            .shutdown(std::net::Shutdown::Both);
        let worker = match self.worker.lock() {
            Ok(mut worker) => worker.take(),
            Err(error) => error.into_inner().take(),
        };
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

fn establish(session: &mut VmmSession, transport: &mut PagerTransport) -> Result<(), PagerError> {
    transport.send(&session.hello()?)?;
    session.receive(transport.receive()?)?;
    for _ in 0..session.offered_limits().region_count() {
        transport.send(&session.next_region()?)?;
    }
    transport.send(&session.start()?)?;
    session.receive(transport.receive()?)?;
    if session.state() != PagerVmmState::Active {
        return Err(PagerError::InvalidPeerState);
    }
    Ok(())
}

fn receive_loop(
    inner: &ClientInner,
    transport: &mut PagerTransport,
    mut wakeup: UnixStream,
) -> Result<(), PagerError> {
    loop {
        if inner.should_stop() {
            return Ok(());
        }
        let timeout = inner.request_timeout();
        let timeout_millis = timeout.map_or(-1, poll_timeout_millis);
        let mut poll_fds = [
            libc::pollfd {
                fd: transport.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wakeup.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `poll_fds` is a live initialized array for both entries and
        // remains exclusively borrowed for the synchronous poll call.
        let result = unsafe { libc::poll(poll_fds.as_mut_ptr(), 2, timeout_millis) };
        if result == 0 {
            return Err(PagerError::Timeout);
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(PagerError::Io(error.kind()));
        }

        let wakeup_events = poll_fds
            .get(1)
            .map(|poll_fd| poll_fd.revents)
            .ok_or(PagerError::InvalidLifecycle)?;
        if wakeup_events & libc::POLLIN != 0 {
            drain_wakeup(&mut wakeup)?;
        }
        if wakeup_events & libc::POLLNVAL != 0 {
            return Err(PagerError::Io(io::ErrorKind::InvalidInput));
        }

        let socket_events = poll_fds
            .first()
            .map(|poll_fd| poll_fd.revents)
            .ok_or(PagerError::InvalidLifecycle)?;
        if socket_events & libc::POLLIN != 0 {
            inner.process_frame(transport.receive()?)?;
            continue;
        }
        if socket_events & libc::POLLNVAL != 0 {
            return Err(PagerError::Io(io::ErrorKind::InvalidInput));
        }
        if socket_events & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err(PagerError::Disconnected);
        }
    }
}

fn poll_timeout_millis(duration: Duration) -> i32 {
    if duration.is_zero() {
        return 0;
    }
    let whole = duration.as_millis();
    let rounded = if duration.subsec_nanos().is_multiple_of(1_000_000) {
        whole
    } else {
        whole.saturating_add(1)
    };
    i32::try_from(rounded).unwrap_or(i32::MAX).max(1)
}

fn drain_wakeup(stream: &mut UnixStream) -> Result<(), PagerError> {
    let mut buffer = [0_u8; 64];
    loop {
        match stream.read(&mut buffer) {
            Ok(0) => return Err(PagerError::Disconnected),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(PagerError::Io(error.kind())),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use crate::{
        MAX_FRAME_BYTES, MIN_PAGE_SIZE, PagerLimits, PagerOperations, PagerRegion, PagerSessionId,
        PeerSession, TerminalCode,
    };

    use super::*;

    fn session(max_in_flight: u16) -> VmmSession {
        let limits = PagerLimits::new(
            MIN_PAGE_SIZE,
            1,
            max_in_flight,
            u32::try_from(MAX_FRAME_BYTES).expect("maximum frame length should fit"),
            PagerOperations::v1(),
        )
        .expect("limits should validate");
        VmmSession::new(
            PagerSessionId::from_bytes([0x61; 32]).expect("session should validate"),
            limits,
            vec![
                PagerRegion::new(
                    PagerRegionId::new(1).expect("region should validate"),
                    0,
                    u64::from(MIN_PAGE_SIZE) * 2,
                    MIN_PAGE_SIZE,
                )
                .expect("region should validate"),
            ],
        )
        .expect("session should construct")
    }

    fn establish_peer(stream: UnixStream) -> (PeerSession, PagerTransport) {
        let mut transport =
            PagerTransport::new(stream, Duration::from_secs(1)).expect("transport should open");
        let mut peer = PeerSession::new();
        let hello = peer
            .receive(transport.receive().expect("hello should arrive"))
            .expect("hello should validate");
        assert_eq!(hello.kind(), PagerFrameKind::Hello);
        let limits = peer
            .offered_limits()
            .expect("hello should contain offered limits");
        transport
            .send(&peer.hello_ack(limits).expect("hello ack should build"))
            .expect("hello ack should send");
        let region = peer
            .receive(transport.receive().expect("region should arrive"))
            .expect("region should validate");
        assert_eq!(region.kind(), PagerFrameKind::Region);
        let start = peer
            .receive(transport.receive().expect("start should arrive"))
            .expect("start should validate");
        assert_eq!(start.kind(), PagerFrameKind::Start);
        transport
            .send(&peer.ready().expect("ready should build"))
            .expect("ready should send");
        (peer, transport)
    }

    #[derive(Debug)]
    struct RecordingObserver {
        count: AtomicUsize,
        sender: Mutex<SyncSender<PagerError>>,
    }

    impl PagerClientTerminalObserver for RecordingObserver {
        fn terminal(&self, failure: PagerError) {
            self.count.fetch_add(1, Ordering::Relaxed);
            if let Ok(sender) = self.sender.lock() {
                let _ = sender.send(failure);
            }
        }
    }

    #[test]
    fn concurrent_page_and_removal_complete_out_of_order() {
        let (client_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let (page_seen_sender, page_seen_receiver) = mpsc::sync_channel(1);
        let peer = thread::spawn(move || {
            let (mut peer, mut transport) = establish_peer(peer_stream);
            let page = peer
                .receive(transport.receive().expect("page request should arrive"))
                .expect("page request should validate");
            assert_eq!(page.kind(), PagerFrameKind::PageRequest);
            page_seen_sender
                .send(())
                .expect("page observation should publish");
            let removal = peer
                .receive(transport.receive().expect("removal should arrive"))
                .expect("removal should validate");
            assert_eq!(removal.kind(), PagerFrameKind::Remove);
            let removal = removal
                .remove_request()
                .expect("removal metadata should exist");
            transport
                .send(
                    &peer
                        .removed(removal.request())
                        .expect("removed response should build"),
                )
                .expect("removed response should send");
            let page = page.page_request().expect("page metadata should exist");
            transport
                .send(
                    &peer
                        .page_data(
                            page.request(),
                            vec![0x93; usize::try_from(page.length()).expect("length should fit")],
                        )
                        .expect("page response should build"),
                )
                .expect("page response should send");
            let shutdown = peer
                .receive(transport.receive().expect("shutdown should arrive"))
                .expect("shutdown should validate");
            assert_eq!(shutdown.kind(), PagerFrameKind::Shutdown);
            transport
                .send(&peer.shutdown_ack().expect("shutdown ack should build"))
                .expect("shutdown ack should send");
        });

        let client = Arc::new(
            PagerClient::connect(session(2), client_stream, Duration::from_secs(1))
                .expect("client should connect"),
        );
        let page_client = Arc::clone(&client);
        let page = thread::spawn(move || {
            page_client.page(
                PagerRegionId::new(1).expect("region should validate"),
                PagerGeneration::new(1).expect("generation should validate"),
                0,
                PageAccess::Read,
            )
        });
        page_seen_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("peer should observe page request");
        let removal_client = Arc::clone(&client);
        let removal = thread::spawn(move || {
            removal_client.remove(
                PagerRegionId::new(1).expect("region should validate"),
                PagerGeneration::new(2).expect("generation should validate"),
                0,
                u64::from(MIN_PAGE_SIZE),
            )
        });

        removal
            .join()
            .expect("removal caller should join")
            .expect("removal should complete");
        assert_eq!(
            page.join()
                .expect("page caller should join")
                .expect("page should complete"),
            PagerClientPage::Data(vec![0x93; MIN_PAGE_SIZE as usize])
        );
        client.shutdown().expect("client should shut down");
        assert_eq!(
            client.state().expect("client state should resolve"),
            PagerClientState::Closed
        );
        peer.join().expect("peer should join");
    }

    #[test]
    fn explicit_terminal_fans_out_once_and_rejects_later_work() {
        let (client_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let peer = thread::spawn(move || {
            let (mut peer, mut transport) = establish_peer(peer_stream);
            transport
                .send(
                    &peer
                        .terminal(TerminalCode::Internal)
                        .expect("terminal should build"),
                )
                .expect("terminal should send");
            release_receiver
                .recv()
                .expect("terminal client should release the peer");
        });
        let (terminal_sender, terminal_receiver) = mpsc::sync_channel(1);
        let observer = Arc::new(RecordingObserver {
            count: AtomicUsize::new(0),
            sender: Mutex::new(terminal_sender),
        });
        let client = PagerClient::connect_observed(
            session(2),
            client_stream,
            Duration::from_secs(1),
            observer.clone(),
        )
        .expect("client should connect");
        assert_eq!(
            terminal_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("terminal observer should fire"),
            PagerError::PeerTerminal
        );
        assert_eq!(observer.count.load(Ordering::Relaxed), 1);
        assert_eq!(
            client.page(
                PagerRegionId::new(1).expect("region should validate"),
                PagerGeneration::new(1).expect("generation should validate"),
                0,
                PageAccess::Read,
            ),
            Err(PagerError::PeerTerminal)
        );
        assert_eq!(
            client
                .terminal_error()
                .expect("terminal state should resolve"),
            Some(PagerError::PeerTerminal)
        );
        release_sender
            .send(())
            .expect("terminal peer should be released");
        peer.join().expect("peer should join");
    }

    #[test]
    fn timeout_releases_every_pending_operation_with_one_terminal_notification() {
        let (client_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let peer = thread::spawn(move || {
            let (mut peer, mut transport) = establish_peer(peer_stream);
            for _ in 0..2 {
                let request = peer
                    .receive(transport.receive().expect("request should arrive"))
                    .expect("request should validate");
                assert_eq!(request.kind(), PagerFrameKind::PageRequest);
            }
            release_receiver
                .recv()
                .expect("timed-out client should release the peer");
        });
        let (terminal_sender, terminal_receiver) = mpsc::sync_channel(1);
        let observer = Arc::new(RecordingObserver {
            count: AtomicUsize::new(0),
            sender: Mutex::new(terminal_sender),
        });
        let client = Arc::new(
            PagerClient::connect_observed(
                session(2),
                client_stream,
                Duration::from_millis(20),
                observer.clone(),
            )
            .expect("client should connect"),
        );
        let mut callers = Vec::new();
        for (index, offset) in [0, u64::from(MIN_PAGE_SIZE)].into_iter().enumerate() {
            let client = Arc::clone(&client);
            callers.push(thread::spawn(move || {
                client.page(
                    PagerRegionId::new(1).expect("region should validate"),
                    PagerGeneration::new(u64::try_from(index).expect("index should fit") + 1)
                        .expect("generation should validate"),
                    offset,
                    PageAccess::Read,
                )
            }));
        }
        for caller in callers {
            assert_eq!(
                caller.join().expect("page caller should join"),
                Err(PagerError::Timeout)
            );
        }
        assert_eq!(
            terminal_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("terminal observer should fire"),
            PagerError::Timeout
        );
        assert_eq!(observer.count.load(Ordering::Relaxed), 1);
        release_sender
            .send(())
            .expect("timed-out peer should be released");
        peer.join().expect("peer should join");
    }
}
