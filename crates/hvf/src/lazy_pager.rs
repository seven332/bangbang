//! Pager-backed content and removal source for lazy HVF memory.

use std::error::Error;
use std::fmt;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use bangbang_pager::{
    CancelReason, PagerClient, PagerClientPage, PagerClientState, PagerClientTerminalObserver,
    PagerError, PagerSessionId, VmmSession,
};
use bangbang_runtime::lazy_memory::{
    LazyGuestMemory, LazyGuestMemoryError, LazyGuestMemoryTerminalReason,
};

use crate::lazy_host_fault::{
    HvfLazyPageContents, HvfLazyPageRemovalRequest, HvfLazyPageRequest, HvfLazyPageSource,
    HvfLazyPageSourceError,
};

/// Construction or lifecycle failure for one pager-backed lazy source.
pub enum HvfLazyPagerError {
    /// The lazy-memory owner could not provide or accept coordinator state.
    Coordinator {
        /// Backend-neutral coordinator failure.
        source: LazyGuestMemoryError,
    },
    /// The connected pager protocol or transport failed.
    Pager {
        /// Stable pager failure.
        source: PagerError,
    },
}

impl fmt::Debug for HvfLazyPagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coordinator { .. } => {
                formatter.write_str("HvfLazyPagerError::Coordinator(<redacted>)")
            }
            Self::Pager { .. } => formatter.write_str("HvfLazyPagerError::Pager(<redacted>)"),
        }
    }
}

impl fmt::Display for HvfLazyPagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coordinator { .. } => {
                formatter.write_str("lazy pager coordinator operation failed")
            }
            Self::Pager { .. } => formatter.write_str("lazy pager peer operation failed"),
        }
    }
}

impl Error for HvfLazyPagerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Coordinator { source } => Some(source),
            Self::Pager { source } => Some(source),
        }
    }
}

#[derive(Debug)]
struct CoordinatorTerminalObserver {
    memory: Arc<LazyGuestMemory>,
}

impl PagerClientTerminalObserver for CoordinatorTerminalObserver {
    fn terminal(&self, _failure: PagerError) {
        let _ = self
            .memory
            .signal_terminal(LazyGuestMemoryTerminalReason::PeerFailure);
    }
}

/// One connected pager session bound to an exact lazy-memory owner.
pub struct HvfLazyPager {
    memory: Arc<LazyGuestMemory>,
    client: PagerClient,
}

impl HvfLazyPager {
    /// Negotiates one random session over an already-connected stream.
    pub fn connect(
        memory: Arc<LazyGuestMemory>,
        stream: UnixStream,
        timeout: Duration,
    ) -> Result<Self, HvfLazyPagerError> {
        let session = PagerSessionId::generate().map_err(|source| {
            let _ = memory.signal_terminal(LazyGuestMemoryTerminalReason::TransitionFailure);
            HvfLazyPagerError::Pager { source }
        })?;
        let regions = memory.pager_regions().map_err(|source| {
            let _ = memory.signal_terminal(LazyGuestMemoryTerminalReason::TransitionFailure);
            HvfLazyPagerError::Coordinator { source }
        })?;
        let vmm = VmmSession::new(session, memory.pager_limits(), regions).map_err(|source| {
            let _ = memory.signal_terminal(LazyGuestMemoryTerminalReason::TransitionFailure);
            HvfLazyPagerError::Pager { source }
        })?;
        let observer = Arc::new(CoordinatorTerminalObserver {
            memory: Arc::clone(&memory),
        });
        let client =
            PagerClient::connect_observed(vmm, stream, timeout, observer).map_err(|source| {
                let _ = memory.signal_terminal(LazyGuestMemoryTerminalReason::PeerFailure);
                HvfLazyPagerError::Pager { source }
            })?;
        let selected = client.selected_limits().map_err(|source| {
            let _ = memory.signal_terminal(LazyGuestMemoryTerminalReason::PeerFailure);
            HvfLazyPagerError::Pager { source }
        })?;
        if selected.max_in_flight() != memory.pager_limits().max_in_flight() {
            let _ = memory.signal_terminal(LazyGuestMemoryTerminalReason::TransitionFailure);
            return Err(HvfLazyPagerError::Pager {
                source: PagerError::InvalidConfiguration,
            });
        }
        Ok(Self { memory, client })
    }

    /// Requests cancellation and releases all current pager work.
    pub fn cancel(&self) -> Result<(), HvfLazyPagerError> {
        let coordinator = self
            .memory
            .signal_terminal(LazyGuestMemoryTerminalReason::Requested)
            .map_err(|source| HvfLazyPagerError::Coordinator { source });
        let pager = self
            .client
            .cancel(CancelReason::Requested)
            .map_err(|source| HvfLazyPagerError::Pager { source });
        coordinator.and(pager)
    }

    /// Performs drain-only orderly pager shutdown.
    pub fn shutdown(&self) -> Result<(), HvfLazyPagerError> {
        self.client
            .shutdown()
            .map_err(|source| HvfLazyPagerError::Pager { source })
    }

    /// Returns the current pager client lifecycle.
    pub fn state(&self) -> Result<PagerClientState, HvfLazyPagerError> {
        self.client
            .state()
            .map_err(|source| HvfLazyPagerError::Pager { source })
    }

    fn peer_failure<T>(&self, _source: PagerError) -> Result<T, HvfLazyPageSourceError> {
        let _ = self
            .memory
            .signal_terminal(LazyGuestMemoryTerminalReason::PeerFailure);
        Err(HvfLazyPageSourceError::peer_failure())
    }
}

impl HvfLazyPageSource for HvfLazyPager {
    fn page(
        &self,
        request: HvfLazyPageRequest,
    ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
        match self.client.page(
            request.region(),
            request.generation(),
            request.offset(),
            request.access(),
        ) {
            Ok(PagerClientPage::Data(data)) => Ok(HvfLazyPageContents::data(data)),
            Ok(PagerClientPage::Zero) => Ok(HvfLazyPageContents::zero()),
            Err(source) => self.peer_failure(source),
        }
    }

    fn remove(&self, request: HvfLazyPageRemovalRequest) -> Result<(), HvfLazyPageSourceError> {
        self.client
            .remove(
                request.region(),
                request.generation(),
                request.offset(),
                request.length(),
            )
            .or_else(|source| self.peer_failure(source))
    }
}

impl fmt::Debug for HvfLazyPager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HvfLazyPager(<redacted>)")
    }
}

impl Drop for HvfLazyPager {
    fn drop(&mut self) {
        let _ = self
            .memory
            .signal_terminal(LazyGuestMemoryTerminalReason::Teardown);
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use bangbang_pager::{
        MAX_FRAME_BYTES, PageAccess, PagerFrameKind, PagerGeneration, PagerLimits, PagerOperations,
        PagerRegionId, PagerTransport, PeerSession, ReferencePeer, ReferencePeerTermination,
    };
    use bangbang_runtime::lazy_memory::{LazyGuestMemoryLimits, LazyGuestMemoryRegion};
    use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange};

    use super::*;

    fn page_size() -> u32 {
        u32::try_from(crate::memory::host_page_size().expect("host page size should resolve"))
            .expect("host page size should fit")
    }

    fn memory() -> Arc<LazyGuestMemory> {
        let page_size = page_size();
        let pager = PagerLimits::new(
            page_size,
            1,
            4,
            u32::try_from(MAX_FRAME_BYTES).expect("maximum frame size should fit"),
            PagerOperations::v1(),
        )
        .expect("pager limits should validate");
        let limits =
            LazyGuestMemoryLimits::new(pager, 2, 4).expect("lazy-memory limits should validate");
        let region = LazyGuestMemoryRegion::new(
            PagerRegionId::new(1).expect("region id should validate"),
            GuestMemoryRange::new(GuestAddress::new(0x8000_0000), u64::from(page_size) * 2)
                .expect("guest range should validate"),
            0,
            page_size,
        )
        .expect("lazy region should validate");
        Arc::new(
            LazyGuestMemory::new(limits, vec![region]).expect("lazy guest memory should construct"),
        )
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

    #[test]
    fn pager_adapter_completes_remove_refault_and_drained_shutdown() {
        let (client_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            ReferencePeer::new(3)
                .expect("reference bound should validate")
                .serve(peer_stream, Duration::from_secs(1))
        });
        let memory = memory();
        let pager =
            HvfLazyPager::connect(Arc::clone(&memory), client_stream, Duration::from_secs(1))
                .expect("lazy pager should connect");
        let region = PagerRegionId::new(1).expect("region should validate");
        assert!(matches!(
            pager.client.page(
                region,
                PagerGeneration::new(1).expect("generation should validate"),
                0,
                PageAccess::Read,
            ),
            Ok(PagerClientPage::Data(_))
        ));
        pager
            .client
            .remove(
                region,
                PagerGeneration::new(2).expect("generation should validate"),
                0,
                u64::from(page_size()),
            )
            .expect("removal should complete");
        assert_eq!(
            pager.client.page(
                region,
                PagerGeneration::new(3).expect("generation should validate"),
                0,
                PageAccess::Read,
            ),
            Ok(PagerClientPage::Zero)
        );
        pager.shutdown().expect("pager should drain");
        assert_eq!(
            pager.state().expect("pager state should resolve"),
            PagerClientState::Closed
        );
        let report = peer
            .join()
            .expect("reference peer should join")
            .expect("reference peer should succeed");
        assert_eq!(report.page_data(), 1);
        assert_eq!(report.page_zero(), 1);
        assert_eq!(report.removals(), 1);
        assert_eq!(report.termination(), ReferencePeerTermination::Shutdown);
        assert_eq!(
            memory
                .terminal_reason()
                .expect("terminal reason should resolve"),
            None
        );
    }

    #[test]
    fn pager_peer_loss_closes_coordinator_once_as_peer_failure() {
        let (client_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            let (mut peer, mut transport) = establish_peer(peer_stream);
            let page = peer
                .receive(transport.receive().expect("page request should arrive"))
                .expect("page request should validate");
            assert_eq!(page.kind(), PagerFrameKind::PageRequest);
        });
        let memory = memory();
        let pager =
            HvfLazyPager::connect(Arc::clone(&memory), client_stream, Duration::from_secs(1))
                .expect("lazy pager should connect");
        assert!(
            pager
                .client
                .page(
                    PagerRegionId::new(1).expect("region should validate"),
                    PagerGeneration::new(1).expect("generation should validate"),
                    0,
                    PageAccess::Read,
                )
                .is_err()
        );
        peer.join().expect("peer should join");
        assert_eq!(
            pager.state().expect("pager state should resolve"),
            PagerClientState::Terminal
        );
        assert_eq!(
            memory
                .terminal_reason()
                .expect("terminal reason should resolve"),
            Some(LazyGuestMemoryTerminalReason::PeerFailure)
        );
        memory
            .signal_terminal(LazyGuestMemoryTerminalReason::Requested)
            .expect("repeat terminal signal should succeed");
        assert_eq!(
            memory
                .terminal_reason()
                .expect("terminal reason should resolve"),
            Some(LazyGuestMemoryTerminalReason::PeerFailure)
        );
    }

    #[test]
    fn requested_cancel_closes_peer_and_coordinator() {
        let (client_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            ReferencePeer::new(1)
                .expect("reference bound should validate")
                .serve(peer_stream, Duration::from_secs(1))
        });
        let memory = memory();
        let pager =
            HvfLazyPager::connect(Arc::clone(&memory), client_stream, Duration::from_secs(1))
                .expect("lazy pager should connect");

        pager
            .cancel()
            .expect("requested cancellation should complete");
        assert_eq!(
            pager.state().expect("pager state should resolve"),
            PagerClientState::Closed
        );
        assert_eq!(
            memory
                .terminal_reason()
                .expect("terminal reason should resolve"),
            Some(LazyGuestMemoryTerminalReason::Requested)
        );
        assert_eq!(
            peer.join()
                .expect("reference peer should join")
                .expect("reference peer should succeed")
                .termination(),
            ReferencePeerTermination::Cancelled
        );
    }

    #[test]
    fn reduced_in_flight_selection_fails_before_page_admission() {
        let (client_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            let mut transport =
                PagerTransport::new(peer_stream, Duration::from_secs(1)).expect("transport opens");
            let mut peer = PeerSession::new();
            let hello = peer
                .receive(transport.receive().expect("hello should arrive"))
                .expect("hello should validate");
            assert_eq!(hello.kind(), PagerFrameKind::Hello);
            let offered = peer
                .offered_limits()
                .expect("hello should contain offered limits");
            let selected = PagerLimits::new(
                offered.page_size(),
                offered.region_count(),
                offered.max_in_flight() - 1,
                offered.max_frame_bytes(),
                offered.operations(),
            )
            .expect("reduced selection should validate");
            transport
                .send(&peer.hello_ack(selected).expect("hello ack should build"))
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
            assert!(
                transport.receive().is_err(),
                "incompatible adapter construction should close the stream"
            );
        });
        let memory = memory();
        assert!(matches!(
            HvfLazyPager::connect(Arc::clone(&memory), client_stream, Duration::from_secs(1)),
            Err(HvfLazyPagerError::Pager {
                source: PagerError::InvalidConfiguration
            })
        ));
        assert_eq!(
            memory
                .terminal_reason()
                .expect("terminal reason should resolve"),
            Some(LazyGuestMemoryTerminalReason::TransitionFailure)
        );
        peer.join().expect("peer should join");
    }
}
