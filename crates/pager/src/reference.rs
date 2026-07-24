//! Bounded support-only peer used by protocol and containment tests.
//!
//! This helper serves one already-connected `bangbang-pager-v1` stream. It is
//! neither a daemon nor an implementation of Firecracker or Linux UFFD wire
//! compatibility.

use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::{
    PagerError, PagerFrameKind, PagerPeerState, PagerRegionId, PagerRemoveRequest, PagerTransport,
    PeerSession, TerminalCode,
};

/// Deterministic nonzero payload byte returned for even-numbered source pages.
pub const REFERENCE_PAGE_BYTE: u8 = 0x5a;

/// Closed completion observed by the bounded reference peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferencePeerTermination {
    /// The VMM cancelled the session and received `Cancelled`.
    Cancelled,
    /// The VMM sent one terminal category that requires no acknowledgement.
    Terminal(TerminalCode),
    /// The VMM drained work and received `ShutdownAck`.
    Shutdown,
}

/// Redacted counts from one reference-peer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferencePeerReport {
    page_data: u32,
    page_zero: u32,
    removals: u32,
    termination: ReferencePeerTermination,
}

impl ReferencePeerReport {
    /// Returns the number of exact page-data responses.
    #[must_use]
    pub const fn page_data(self) -> u32 {
        self.page_data
    }

    /// Returns the number of all-zero page responses.
    #[must_use]
    pub const fn page_zero(self) -> u32 {
        self.page_zero
    }

    /// Returns the number of acknowledged removals.
    #[must_use]
    pub const fn removals(self) -> u32 {
        self.removals
    }

    /// Returns the terminal lifecycle outcome.
    #[must_use]
    pub const fn termination(self) -> ReferencePeerTermination {
        self.termination
    }
}

/// One operation-bounded deterministic test/support peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferencePeer {
    max_operations: u32,
}

#[derive(Clone, Copy)]
struct RemovedRange {
    region: PagerRegionId,
    start: u64,
    end: u64,
}

impl ReferencePeer {
    /// Creates a peer with a positive bound on active page/removal operations.
    pub fn new(max_operations: u32) -> Result<Self, PagerError> {
        if max_operations == 0 {
            return Err(PagerError::InvalidConfiguration);
        }
        Ok(Self { max_operations })
    }

    /// Serves one complete session over an already-connected stream.
    pub fn serve(
        self,
        stream: UnixStream,
        timeout: Duration,
    ) -> Result<ReferencePeerReport, PagerError> {
        let mut transport = PagerTransport::new(stream, timeout)?;
        let mut peer = PeerSession::new();
        let hello = peer.receive(transport.receive()?)?;
        if hello.kind() != PagerFrameKind::Hello {
            return Err(PagerError::InvalidPeerState);
        }
        let selected = peer.offered_limits().ok_or(PagerError::InvalidLifecycle)?;
        transport.send(&peer.hello_ack(selected)?)?;

        loop {
            let frame = peer.receive(transport.receive()?)?;
            match frame.kind() {
                PagerFrameKind::Region => {}
                PagerFrameKind::Start => break,
                PagerFrameKind::Cancel => {
                    transport.send(&peer.cancelled()?)?;
                    return Ok(ReferencePeerReport {
                        page_data: 0,
                        page_zero: 0,
                        removals: 0,
                        termination: ReferencePeerTermination::Cancelled,
                    });
                }
                PagerFrameKind::Terminal => {
                    return Ok(ReferencePeerReport {
                        page_data: 0,
                        page_zero: 0,
                        removals: 0,
                        termination: ReferencePeerTermination::Terminal(
                            frame.terminal_code().ok_or(PagerError::InvalidFrame)?,
                        ),
                    });
                }
                _ => return Err(PagerError::InvalidPeerState),
            }
        }
        transport.send(&peer.ready()?)?;

        let mut page_data = 0_u32;
        let mut page_zero = 0_u32;
        let mut removals = 0_u32;
        let mut operations = 0_u32;
        let mut removed_ranges = Vec::<RemovedRange>::new();
        loop {
            let frame = peer.receive(transport.receive()?)?;
            match frame.kind() {
                PagerFrameKind::PageRequest => {
                    operations = bounded_increment(operations, self.max_operations)?;
                    let request = frame.page_request().ok_or(PagerError::InvalidFrame)?;
                    let page = request.offset() / u64::from(request.length());
                    if removed_ranges
                        .iter()
                        .any(|removed| page_overlaps_removal(request, *removed))
                    {
                        transport.send(&peer.page_zero(request.request())?)?;
                        page_zero = page_zero.checked_add(1).ok_or(PagerError::LimitExceeded)?;
                    } else if page.is_multiple_of(2) {
                        let length = usize::try_from(request.length())
                            .map_err(|_| PagerError::InvalidConfiguration)?;
                        transport.send(
                            &peer
                                .page_data(request.request(), vec![REFERENCE_PAGE_BYTE; length])?,
                        )?;
                        page_data = page_data.checked_add(1).ok_or(PagerError::LimitExceeded)?;
                    } else {
                        transport.send(&peer.page_zero(request.request())?)?;
                        page_zero = page_zero.checked_add(1).ok_or(PagerError::LimitExceeded)?;
                    }
                }
                PagerFrameKind::Remove => {
                    operations = bounded_increment(operations, self.max_operations)?;
                    let removal = frame.remove_request().ok_or(PagerError::InvalidFrame)?;
                    record_removed_range(&mut removed_ranges, removal)?;
                    transport.send(&peer.removed(removal.request())?)?;
                    removals = removals.checked_add(1).ok_or(PagerError::LimitExceeded)?;
                }
                PagerFrameKind::Cancel => {
                    transport.send(&peer.cancelled()?)?;
                    return Ok(ReferencePeerReport {
                        page_data,
                        page_zero,
                        removals,
                        termination: ReferencePeerTermination::Cancelled,
                    });
                }
                PagerFrameKind::Terminal => {
                    return Ok(ReferencePeerReport {
                        page_data,
                        page_zero,
                        removals,
                        termination: ReferencePeerTermination::Terminal(
                            frame.terminal_code().ok_or(PagerError::InvalidFrame)?,
                        ),
                    });
                }
                PagerFrameKind::Shutdown => {
                    transport.send(&peer.shutdown_ack()?)?;
                    debug_assert_eq!(peer.state(), PagerPeerState::Closed);
                    return Ok(ReferencePeerReport {
                        page_data,
                        page_zero,
                        removals,
                        termination: ReferencePeerTermination::Shutdown,
                    });
                }
                _ => return Err(PagerError::InvalidPeerState),
            }
        }
    }
}

fn record_removed_range(
    ranges: &mut Vec<RemovedRange>,
    removal: PagerRemoveRequest,
) -> Result<(), PagerError> {
    let mut start = removal.offset();
    let mut end = start
        .checked_add(removal.length())
        .ok_or(PagerError::InvalidPeerState)?;
    let mut index = 0;
    while index < ranges.len() {
        let existing = ranges
            .get(index)
            .copied()
            .ok_or(PagerError::InvalidLifecycle)?;
        if existing.region == removal.region() && start <= existing.end && existing.start <= end {
            start = start.min(existing.start);
            end = end.max(existing.end);
            ranges.swap_remove(index);
        } else {
            index += 1;
        }
    }
    ranges
        .try_reserve(1)
        .map_err(|_| PagerError::LimitExceeded)?;
    ranges.push(RemovedRange {
        region: removal.region(),
        start,
        end,
    });
    Ok(())
}

fn page_overlaps_removal(page: crate::PagerPageRequest, removal: RemovedRange) -> bool {
    if page.region() != removal.region {
        return false;
    }
    let Some(page_end) = page.offset().checked_add(u64::from(page.length())) else {
        return false;
    };
    page.offset() < removal.end && removal.start < page_end
}

fn bounded_increment(current: u32, maximum: u32) -> Result<u32, PagerError> {
    let next = current.checked_add(1).ok_or(PagerError::LimitExceeded)?;
    if next > maximum {
        Err(PagerError::LimitExceeded)
    } else {
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use crate::{
        CancelReason, MAX_FRAME_BYTES, MIN_PAGE_SIZE, PageAccess, PagerGeneration, PagerLimits,
        PagerOperations, PagerRegion, PagerRegionId, PagerSessionId, PagerTransport, PagerVmmState,
        VmmSession,
    };

    use super::*;

    fn vmm() -> VmmSession {
        let limits = PagerLimits::new(
            MIN_PAGE_SIZE,
            1,
            4,
            u32::try_from(MAX_FRAME_BYTES).expect("maximum frame size should fit"),
            PagerOperations::v1(),
        )
        .expect("limits should validate");
        VmmSession::new(
            PagerSessionId::from_bytes([41; 32]).expect("session should validate"),
            limits,
            vec![
                PagerRegion::new(
                    PagerRegionId::new(1).expect("region should validate"),
                    0,
                    u64::from(MIN_PAGE_SIZE) * 3,
                    MIN_PAGE_SIZE,
                )
                .expect("region should validate"),
            ],
        )
        .expect("VMM should validate")
    }

    fn establish(vmm: &mut VmmSession, transport: &mut PagerTransport) -> Result<(), PagerError> {
        transport.send(&vmm.hello()?)?;
        vmm.receive(transport.receive()?)?;
        transport.send(&vmm.next_region()?)?;
        transport.send(&vmm.start()?)?;
        vmm.receive(transport.receive()?)?;
        Ok(())
    }

    #[test]
    fn support_peer_serves_data_zero_removal_and_shutdown() {
        let (vmm_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            ReferencePeer::new(3)
                .expect("bound should validate")
                .serve(peer_stream, Duration::from_secs(1))
        });
        let mut transport =
            PagerTransport::new(vmm_stream, Duration::from_secs(1)).expect("transport should open");
        let mut vmm = vmm();
        establish(&mut vmm, &mut transport).expect("session should establish");
        let region = PagerRegionId::new(1).expect("region should validate");
        let generation = PagerGeneration::new(1).expect("generation should validate");
        for offset in [0, u64::from(MIN_PAGE_SIZE)] {
            let request = vmm
                .request_page(region, generation, offset, PageAccess::Read)
                .expect("request should build");
            transport.send(&request).expect("request should send");
            vmm.receive(transport.receive().expect("response should arrive"))
                .expect("response should validate");
        }
        let removal = vmm
            .remove(region, generation, 0, u64::from(MIN_PAGE_SIZE))
            .expect("removal should build");
        transport.send(&removal).expect("removal should send");
        vmm.receive(transport.receive().expect("removal ack should arrive"))
            .expect("removal ack should validate");
        transport
            .send(&vmm.shutdown().expect("shutdown should build"))
            .expect("shutdown should send");
        vmm.receive(transport.receive().expect("shutdown ack should arrive"))
            .expect("shutdown ack should validate");
        assert_eq!(vmm.state(), PagerVmmState::Closed);

        let report = peer
            .join()
            .expect("peer thread should join")
            .expect("peer should succeed");
        assert_eq!(report.page_data(), 1);
        assert_eq!(report.page_zero(), 1);
        assert_eq!(report.removals(), 1);
        assert_eq!(report.termination(), ReferencePeerTermination::Shutdown);
    }

    #[test]
    fn support_peer_acknowledges_cancellation_and_enforces_bound() {
        let (vmm_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            ReferencePeer::new(1)
                .expect("bound should validate")
                .serve(peer_stream, Duration::from_secs(1))
        });
        let mut transport =
            PagerTransport::new(vmm_stream, Duration::from_secs(1)).expect("transport should open");
        let mut vmm = vmm();
        establish(&mut vmm, &mut transport).expect("session should establish");
        transport
            .send(
                &vmm.cancel(CancelReason::Requested)
                    .expect("cancel should build"),
            )
            .expect("cancel should send");
        vmm.receive(transport.receive().expect("cancel ack should arrive"))
            .expect("cancel ack should validate");
        assert_eq!(vmm.state(), PagerVmmState::Closed);
        assert_eq!(
            peer.join()
                .expect("peer thread should join")
                .expect("peer should succeed")
                .termination(),
            ReferencePeerTermination::Cancelled
        );
        assert_eq!(ReferencePeer::new(0), Err(PagerError::InvalidConfiguration));
        assert_eq!(bounded_increment(1, 1), Err(PagerError::LimitExceeded));
    }

    #[test]
    fn support_peer_returns_zero_after_removal_without_changing_adjacent_data() {
        let (vmm_stream, peer_stream) = UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            ReferencePeer::new(4)
                .expect("bound should validate")
                .serve(peer_stream, Duration::from_secs(1))
        });
        let mut transport =
            PagerTransport::new(vmm_stream, Duration::from_secs(1)).expect("transport should open");
        let mut vmm = vmm();
        establish(&mut vmm, &mut transport).expect("session should establish");
        let region = PagerRegionId::new(1).expect("region should validate");

        transport
            .send(
                &vmm.request_page(
                    region,
                    PagerGeneration::new(1).expect("generation should validate"),
                    0,
                    PageAccess::Read,
                )
                .expect("initial request should build"),
            )
            .expect("initial request should send");
        assert!(
            vmm.receive(transport.receive().expect("initial response should arrive"))
                .expect("initial response should validate")
                .page_data()
                .is_some()
        );

        transport
            .send(
                &vmm.remove(
                    region,
                    PagerGeneration::new(2).expect("generation should validate"),
                    0,
                    u64::from(MIN_PAGE_SIZE),
                )
                .expect("removal should build"),
            )
            .expect("removal should send");
        vmm.receive(transport.receive().expect("removal response should arrive"))
            .expect("removal response should validate");

        transport
            .send(
                &vmm.request_page(
                    region,
                    PagerGeneration::new(3).expect("generation should validate"),
                    0,
                    PageAccess::Read,
                )
                .expect("refault should build"),
            )
            .expect("refault should send");
        assert_eq!(
            vmm.receive(transport.receive().expect("refault response should arrive"))
                .expect("refault response should validate")
                .kind(),
            PagerFrameKind::PageZero
        );

        transport
            .send(
                &vmm.request_page(
                    region,
                    PagerGeneration::new(4).expect("generation should validate"),
                    u64::from(MIN_PAGE_SIZE) * 2,
                    PageAccess::Read,
                )
                .expect("adjacent request should build"),
            )
            .expect("adjacent request should send");
        assert_eq!(
            vmm.receive(
                transport
                    .receive()
                    .expect("adjacent response should arrive")
            )
            .expect("adjacent response should validate")
            .kind(),
            PagerFrameKind::PageData
        );

        transport
            .send(&vmm.shutdown().expect("shutdown should build"))
            .expect("shutdown should send");
        vmm.receive(transport.receive().expect("shutdown ack should arrive"))
            .expect("shutdown ack should validate");
        let report = peer
            .join()
            .expect("peer should join")
            .expect("peer should succeed");
        assert_eq!(report.page_data(), 2);
        assert_eq!(report.page_zero(), 1);
        assert_eq!(report.removals(), 1);
        assert_eq!(report.termination(), ReferencePeerTermination::Shutdown);
    }
}
