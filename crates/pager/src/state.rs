use std::collections::BTreeMap;
use std::fmt;

use crate::frame::{PagerMessage, PagerPageRequest, PagerPageResponse, PagerRemoveRequest};
use crate::{
    CancelReason, PageAccess, PagerError, PagerFrame, PagerGeneration, PagerLimits, PagerRegion,
    PagerRegionId, PagerRequestId, PagerSessionId, TerminalCode,
};

/// VMM-observed monotonic protocol phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerVmmState {
    /// No frame has been emitted.
    New,
    /// Hello was emitted and the exact peer selection is pending.
    AwaitHelloAck,
    /// Region records are being emitted.
    ConfigureRegions,
    /// Start was emitted and Ready is pending.
    AwaitReady,
    /// Page and removal work may be issued.
    Active,
    /// Terminal cancellation was emitted and its acknowledgement is pending.
    AwaitCancelled,
    /// Drained shutdown was emitted and its acknowledgement is pending.
    AwaitShutdownAck,
    /// The peer acknowledged cancellation or orderly shutdown.
    Closed,
    /// An explicit terminal frame or invalid peer transition ended the session.
    Terminal,
}

/// External peer-observed monotonic protocol phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagerPeerState {
    /// The peer has not received Hello.
    AwaitHello,
    /// Hello was accepted and a limit selection must be returned.
    SelectLimits,
    /// Exact region records and Start are pending.
    ConfigureRegions,
    /// Complete configuration is waiting for local Ready.
    ReadyToConfirm,
    /// Page and removal work may be received.
    Active,
    /// Cancellation was received and must be acknowledged.
    ReadyToCancel,
    /// Drained shutdown was received and must be acknowledged.
    ReadyToShutdown,
    /// Cancellation or shutdown was acknowledged.
    Closed,
    /// An explicit terminal frame or invalid peer transition ended the session.
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingRequest {
    Page(PagerPageRequest),
    Remove(PagerRemoveRequest),
}

/// VMM-side identity, negotiation, region, request and lifecycle enforcement.
pub struct VmmSession {
    session: PagerSessionId,
    offered: PagerLimits,
    selected: Option<PagerLimits>,
    regions: Vec<PagerRegion>,
    next_region: usize,
    state: PagerVmmState,
    next_request: u64,
    outstanding: BTreeMap<PagerRequestId, PendingRequest>,
}

impl VmmSession {
    /// Validates one complete local region offer before any frame is emitted.
    pub fn new(
        session: PagerSessionId,
        offered: PagerLimits,
        regions: Vec<PagerRegion>,
    ) -> Result<Self, PagerError> {
        validate_regions(offered, &regions)?;
        Ok(Self {
            session,
            offered,
            selected: None,
            regions,
            next_region: 0,
            state: PagerVmmState::New,
            next_request: 1,
            outstanding: BTreeMap::new(),
        })
    }

    /// Returns the bound session identity.
    #[must_use]
    pub const fn session(&self) -> PagerSessionId {
        self.session
    }

    /// Returns the current coarse lifecycle phase.
    #[must_use]
    pub const fn state(&self) -> PagerVmmState {
        self.state
    }

    /// Returns the offered limits.
    #[must_use]
    pub const fn offered_limits(&self) -> PagerLimits {
        self.offered
    }

    /// Returns peer-selected limits after a valid HelloAck.
    #[must_use]
    pub const fn selected_limits(&self) -> Option<PagerLimits> {
        self.selected
    }

    /// Returns the combined number of outstanding page and removal requests.
    #[must_use]
    pub fn outstanding_count(&self) -> usize {
        self.outstanding.len()
    }

    /// Creates the sole Hello frame.
    pub fn hello(&mut self) -> Result<PagerFrame, PagerError> {
        if self.state != PagerVmmState::New {
            return Err(PagerError::InvalidLifecycle);
        }
        self.state = PagerVmmState::AwaitHelloAck;
        Ok(self.frame(PagerMessage::Hello(self.offered)))
    }

    /// Creates the next configured Region frame in caller-supplied order.
    pub fn next_region(&mut self) -> Result<PagerFrame, PagerError> {
        if self.state != PagerVmmState::ConfigureRegions {
            return Err(PagerError::InvalidLifecycle);
        }
        let region = self
            .regions
            .get(self.next_region)
            .copied()
            .ok_or(PagerError::InvalidLifecycle)?;
        self.next_region = self
            .next_region
            .checked_add(1)
            .ok_or(PagerError::InvalidLifecycle)?;
        Ok(self.frame(PagerMessage::Region(region)))
    }

    /// Ends region configuration after every exact record was emitted.
    pub fn start(&mut self) -> Result<PagerFrame, PagerError> {
        if self.state != PagerVmmState::ConfigureRegions || self.next_region != self.regions.len() {
            return Err(PagerError::InvalidLifecycle);
        }
        self.state = PagerVmmState::AwaitReady;
        Ok(self.frame(PagerMessage::Start))
    }

    /// Creates one bounded page request with a new strictly increasing ID.
    pub fn request_page(
        &mut self,
        region: PagerRegionId,
        generation: PagerGeneration,
        offset: u64,
        access: PageAccess,
    ) -> Result<PagerFrame, PagerError> {
        self.require_active_capacity()?;
        let selected = self.selected.ok_or(PagerError::InvalidLifecycle)?;
        let configured = self.find_region(region)?;
        let length = selected.page_size();
        if !page_range_is_valid(configured, offset, length, selected.page_size()) {
            return Err(PagerError::InvalidConfiguration);
        }
        let request = PagerPageRequest {
            request: self.allocate_request_id()?,
            region,
            access,
            generation,
            offset,
            length,
        };
        if self
            .outstanding
            .insert(request.request(), PendingRequest::Page(request))
            .is_some()
        {
            return Err(PagerError::InvalidLifecycle);
        }
        Ok(self.frame(PagerMessage::PageRequest(request)))
    }

    /// Creates one bounded removal notification with a new increasing ID.
    pub fn remove(
        &mut self,
        region: PagerRegionId,
        generation: PagerGeneration,
        offset: u64,
        length: u64,
    ) -> Result<PagerFrame, PagerError> {
        self.require_active_capacity()?;
        let selected = self.selected.ok_or(PagerError::InvalidLifecycle)?;
        let configured = self.find_region(region)?;
        if !remove_range_is_valid(configured, offset, length, selected.page_size()) {
            return Err(PagerError::InvalidConfiguration);
        }
        let request = PagerRemoveRequest {
            request: self.allocate_request_id()?,
            region,
            generation,
            offset,
            length,
        };
        if self
            .outstanding
            .insert(request.request(), PendingRequest::Remove(request))
            .is_some()
        {
            return Err(PagerError::InvalidLifecycle);
        }
        Ok(self.frame(PagerMessage::Remove(request)))
    }

    /// Abandons all work and creates terminal, session-wide cancellation.
    pub fn cancel(&mut self, reason: CancelReason) -> Result<PagerFrame, PagerError> {
        if !matches!(
            self.state,
            PagerVmmState::AwaitHelloAck
                | PagerVmmState::ConfigureRegions
                | PagerVmmState::AwaitReady
                | PagerVmmState::Active
        ) {
            return Err(PagerError::InvalidLifecycle);
        }
        self.outstanding.clear();
        self.state = PagerVmmState::AwaitCancelled;
        Ok(self.frame(PagerMessage::Cancel(reason)))
    }

    /// Creates orderly shutdown only after all work has drained.
    pub fn shutdown(&mut self) -> Result<PagerFrame, PagerError> {
        if self.state != PagerVmmState::Active || !self.outstanding.is_empty() {
            return Err(PagerError::InvalidLifecycle);
        }
        self.state = PagerVmmState::AwaitShutdownAck;
        Ok(self.frame(PagerMessage::Shutdown))
    }

    /// Ends any established live phase with one string-free category.
    pub fn terminal(&mut self, code: TerminalCode) -> Result<PagerFrame, PagerError> {
        if matches!(
            self.state,
            PagerVmmState::New | PagerVmmState::Closed | PagerVmmState::Terminal
        ) {
            return Err(PagerError::InvalidLifecycle);
        }
        self.outstanding.clear();
        self.state = PagerVmmState::Terminal;
        Ok(self.frame(PagerMessage::Terminal(code)))
    }

    /// Validates and applies one peer-originated frame.
    ///
    /// Any invalid peer input terminates local protocol state. Callers retain
    /// the validated frame so page bytes can be consumed without another copy.
    pub fn receive(&mut self, frame: PagerFrame) -> Result<PagerFrame, PagerError> {
        let result = self.receive_inner(&frame);
        if result.is_err() {
            self.outstanding.clear();
            self.state = PagerVmmState::Terminal;
        }
        result.map(|()| frame)
    }

    fn receive_inner(&mut self, frame: &PagerFrame) -> Result<(), PagerError> {
        if frame.session() != self.session {
            return Err(PagerError::InvalidPeerState);
        }
        match (self.state, &frame.message) {
            (PagerVmmState::AwaitHelloAck, PagerMessage::HelloAck(selected))
                if selected.is_selection_for(self.offered) =>
            {
                self.selected = Some(*selected);
                self.state = PagerVmmState::ConfigureRegions;
            }
            (PagerVmmState::AwaitReady, PagerMessage::Ready) => {
                self.state = PagerVmmState::Active;
            }
            (PagerVmmState::Active, PagerMessage::PageData(response, data)) => {
                self.complete_page(*response, Some(data))?;
            }
            (PagerVmmState::Active, PagerMessage::PageZero(response)) => {
                self.complete_page(*response, None)?;
            }
            (PagerVmmState::Active, PagerMessage::Removed(response)) => {
                self.complete_remove(*response)?;
            }
            (PagerVmmState::AwaitCancelled, PagerMessage::Cancelled) => {
                self.state = PagerVmmState::Closed;
            }
            (PagerVmmState::AwaitShutdownAck, PagerMessage::ShutdownAck) => {
                self.state = PagerVmmState::Closed;
            }
            (
                PagerVmmState::AwaitHelloAck
                | PagerVmmState::ConfigureRegions
                | PagerVmmState::AwaitReady
                | PagerVmmState::Active
                | PagerVmmState::AwaitCancelled
                | PagerVmmState::AwaitShutdownAck,
                PagerMessage::Terminal(_),
            ) => {
                self.outstanding.clear();
                self.state = PagerVmmState::Terminal;
            }
            _ => return Err(PagerError::InvalidLifecycle),
        }
        Ok(())
    }

    fn complete_page(
        &mut self,
        response: PagerPageResponse,
        data: Option<&[u8]>,
    ) -> Result<(), PagerError> {
        let expected = self
            .outstanding
            .get(&response.request())
            .copied()
            .ok_or(PagerError::InvalidPeerState)?;
        if expected != PendingRequest::Page(response.request_metadata())
            || data.is_some_and(|bytes| {
                usize::try_from(response.length())
                    .map(|length| bytes.len() != length)
                    .unwrap_or(true)
            })
        {
            return Err(PagerError::InvalidPeerState);
        }
        self.outstanding
            .remove(&response.request())
            .ok_or(PagerError::InvalidPeerState)?;
        Ok(())
    }

    fn complete_remove(&mut self, response: PagerRemoveRequest) -> Result<(), PagerError> {
        if self.outstanding.get(&response.request()).copied()
            != Some(PendingRequest::Remove(response))
        {
            return Err(PagerError::InvalidPeerState);
        }
        self.outstanding
            .remove(&response.request())
            .ok_or(PagerError::InvalidPeerState)?;
        Ok(())
    }

    fn require_active_capacity(&self) -> Result<(), PagerError> {
        if self.state != PagerVmmState::Active {
            return Err(PagerError::InvalidLifecycle);
        }
        let limit = self
            .selected
            .ok_or(PagerError::InvalidLifecycle)?
            .max_in_flight();
        if self.outstanding.len() >= usize::from(limit) {
            Err(PagerError::LimitExceeded)
        } else {
            Ok(())
        }
    }

    fn find_region(&self, id: PagerRegionId) -> Result<PagerRegion, PagerError> {
        self.regions
            .iter()
            .copied()
            .find(|region| region.id() == id)
            .ok_or(PagerError::InvalidConfiguration)
    }

    fn allocate_request_id(&mut self) -> Result<PagerRequestId, PagerError> {
        let next = self
            .next_request
            .checked_add(1)
            .ok_or(PagerError::LimitExceeded)?;
        let request =
            PagerRequestId::new(self.next_request).map_err(|_| PagerError::LimitExceeded)?;
        self.next_request = next;
        Ok(request)
    }

    fn frame(&self, message: PagerMessage) -> PagerFrame {
        PagerFrame::new(self.session, message)
    }
}

impl fmt::Debug for VmmSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmmSession")
            .field("session", &"<redacted>")
            .field("state", &self.state)
            .field("limits", &"<redacted>")
            .field("regions", &"<redacted>")
            .field("outstanding_count", &self.outstanding.len())
            .finish()
    }
}

/// External-peer identity, negotiation, region, request and lifecycle enforcement.
pub struct PeerSession {
    session: Option<PagerSessionId>,
    offered: Option<PagerLimits>,
    selected: Option<PagerLimits>,
    regions: BTreeMap<PagerRegionId, PagerRegion>,
    state: PagerPeerState,
    last_request: u64,
    outstanding: BTreeMap<PagerRequestId, PendingRequest>,
}

impl PeerSession {
    /// Creates an unbound external peer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            session: None,
            offered: None,
            selected: None,
            regions: BTreeMap::new(),
            state: PagerPeerState::AwaitHello,
            last_request: 0,
            outstanding: BTreeMap::new(),
        }
    }

    /// Returns the identity after a valid Hello.
    #[must_use]
    pub const fn session(&self) -> Option<PagerSessionId> {
        self.session
    }

    /// Returns the current coarse lifecycle phase.
    #[must_use]
    pub const fn state(&self) -> PagerPeerState {
        self.state
    }

    /// Returns the VMM offer after Hello.
    #[must_use]
    pub const fn offered_limits(&self) -> Option<PagerLimits> {
        self.offered
    }

    /// Returns the selected limits after HelloAck.
    #[must_use]
    pub const fn selected_limits(&self) -> Option<PagerLimits> {
        self.selected
    }

    /// Returns the combined number of requests still awaiting a response.
    #[must_use]
    pub fn outstanding_count(&self) -> usize {
        self.outstanding.len()
    }

    /// Selects limits bounded by the exact VMM offer.
    pub fn hello_ack(&mut self, selected: PagerLimits) -> Result<PagerFrame, PagerError> {
        let offered = self.offered.ok_or(PagerError::InvalidLifecycle)?;
        if self.state != PagerPeerState::SelectLimits || !selected.is_selection_for(offered) {
            return Err(PagerError::InvalidConfiguration);
        }
        self.selected = Some(selected);
        self.state = PagerPeerState::ConfigureRegions;
        self.frame(PagerMessage::HelloAck(selected))
    }

    /// Confirms a complete exact region configuration.
    pub fn ready(&mut self) -> Result<PagerFrame, PagerError> {
        if self.state != PagerPeerState::ReadyToConfirm {
            return Err(PagerError::InvalidLifecycle);
        }
        self.state = PagerPeerState::Active;
        self.frame(PagerMessage::Ready)
    }

    /// Returns exact bytes for one outstanding page request.
    pub fn page_data(
        &mut self,
        request: PagerRequestId,
        data: Vec<u8>,
    ) -> Result<PagerFrame, PagerError> {
        if self.state != PagerPeerState::Active {
            return Err(PagerError::InvalidLifecycle);
        }
        let pending = self
            .outstanding
            .get(&request)
            .copied()
            .ok_or(PagerError::InvalidPeerState)?;
        let PendingRequest::Page(metadata) = pending else {
            return Err(PagerError::InvalidPeerState);
        };
        let expected_length =
            usize::try_from(metadata.length()).map_err(|_| PagerError::InvalidConfiguration)?;
        if data.len() != expected_length {
            return Err(PagerError::InvalidConfiguration);
        }
        self.outstanding
            .remove(&request)
            .ok_or(PagerError::InvalidPeerState)?;
        self.frame(PagerMessage::PageData(PagerPageResponse(metadata), data))
    }

    /// Returns an all-zero result for one outstanding page request.
    pub fn page_zero(&mut self, request: PagerRequestId) -> Result<PagerFrame, PagerError> {
        if self.state != PagerPeerState::Active {
            return Err(PagerError::InvalidLifecycle);
        }
        let pending = self
            .outstanding
            .get(&request)
            .copied()
            .ok_or(PagerError::InvalidPeerState)?;
        let PendingRequest::Page(metadata) = pending else {
            return Err(PagerError::InvalidPeerState);
        };
        self.outstanding
            .remove(&request)
            .ok_or(PagerError::InvalidPeerState)?;
        self.frame(PagerMessage::PageZero(PagerPageResponse(metadata)))
    }

    /// Acknowledges one exact outstanding removal.
    pub fn removed(&mut self, request: PagerRequestId) -> Result<PagerFrame, PagerError> {
        if self.state != PagerPeerState::Active {
            return Err(PagerError::InvalidLifecycle);
        }
        let pending = self
            .outstanding
            .get(&request)
            .copied()
            .ok_or(PagerError::InvalidPeerState)?;
        let PendingRequest::Remove(metadata) = pending else {
            return Err(PagerError::InvalidPeerState);
        };
        self.outstanding
            .remove(&request)
            .ok_or(PagerError::InvalidPeerState)?;
        self.frame(PagerMessage::Removed(metadata))
    }

    /// Acknowledges terminal session-wide cancellation.
    pub fn cancelled(&mut self) -> Result<PagerFrame, PagerError> {
        if self.state != PagerPeerState::ReadyToCancel {
            return Err(PagerError::InvalidLifecycle);
        }
        self.state = PagerPeerState::Closed;
        self.frame(PagerMessage::Cancelled)
    }

    /// Acknowledges an orderly drained shutdown.
    pub fn shutdown_ack(&mut self) -> Result<PagerFrame, PagerError> {
        if self.state != PagerPeerState::ReadyToShutdown {
            return Err(PagerError::InvalidLifecycle);
        }
        self.state = PagerPeerState::Closed;
        self.frame(PagerMessage::ShutdownAck)
    }

    /// Ends any established live phase with one string-free category.
    pub fn terminal(&mut self, code: TerminalCode) -> Result<PagerFrame, PagerError> {
        if matches!(
            self.state,
            PagerPeerState::AwaitHello | PagerPeerState::Closed | PagerPeerState::Terminal
        ) {
            return Err(PagerError::InvalidLifecycle);
        }
        self.outstanding.clear();
        self.state = PagerPeerState::Terminal;
        self.frame(PagerMessage::Terminal(code))
    }

    /// Validates and applies one VMM-originated frame.
    ///
    /// Any invalid peer input terminates local protocol state.
    pub fn receive(&mut self, frame: PagerFrame) -> Result<PagerFrame, PagerError> {
        let result = self.receive_inner(&frame);
        if result.is_err() {
            self.outstanding.clear();
            self.state = PagerPeerState::Terminal;
        }
        result.map(|()| frame)
    }

    fn receive_inner(&mut self, frame: &PagerFrame) -> Result<(), PagerError> {
        if let Some(session) = self.session
            && frame.session() != session
        {
            return Err(PagerError::InvalidPeerState);
        }
        match (self.state, &frame.message) {
            (PagerPeerState::AwaitHello, PagerMessage::Hello(offered)) => {
                self.session = Some(frame.session());
                self.offered = Some(*offered);
                self.state = PagerPeerState::SelectLimits;
            }
            (PagerPeerState::ConfigureRegions, PagerMessage::Region(region)) => {
                self.accept_region(*region)?;
            }
            (PagerPeerState::ConfigureRegions, PagerMessage::Start) => {
                let expected = usize::from(
                    self.selected
                        .ok_or(PagerError::InvalidLifecycle)?
                        .region_count(),
                );
                if self.regions.len() != expected {
                    return Err(PagerError::InvalidPeerState);
                }
                self.state = PagerPeerState::ReadyToConfirm;
            }
            (PagerPeerState::Active, PagerMessage::PageRequest(request)) => {
                self.accept_page(*request)?;
            }
            (PagerPeerState::Active, PagerMessage::Remove(request)) => {
                self.accept_remove(*request)?;
            }
            (
                PagerPeerState::SelectLimits
                | PagerPeerState::ConfigureRegions
                | PagerPeerState::ReadyToConfirm
                | PagerPeerState::Active,
                PagerMessage::Cancel(_),
            ) => {
                self.outstanding.clear();
                self.state = PagerPeerState::ReadyToCancel;
            }
            (PagerPeerState::Active, PagerMessage::Shutdown) if self.outstanding.is_empty() => {
                self.state = PagerPeerState::ReadyToShutdown;
            }
            (
                PagerPeerState::SelectLimits
                | PagerPeerState::ConfigureRegions
                | PagerPeerState::ReadyToConfirm
                | PagerPeerState::Active
                | PagerPeerState::ReadyToCancel
                | PagerPeerState::ReadyToShutdown,
                PagerMessage::Terminal(_),
            ) => {
                self.outstanding.clear();
                self.state = PagerPeerState::Terminal;
            }
            _ => return Err(PagerError::InvalidLifecycle),
        }
        Ok(())
    }

    fn accept_region(&mut self, region: PagerRegion) -> Result<(), PagerError> {
        let selected = self.selected.ok_or(PagerError::InvalidLifecycle)?;
        if !region.is_valid_for(selected.page_size())
            || self.regions.len() >= usize::from(selected.region_count())
            || self.regions.contains_key(&region.id())
            || self
                .regions
                .values()
                .copied()
                .any(|existing| source_ranges_overlap(existing, region))
        {
            return Err(PagerError::InvalidPeerState);
        }
        self.regions.insert(region.id(), region);
        Ok(())
    }

    fn accept_page(&mut self, request: PagerPageRequest) -> Result<(), PagerError> {
        self.accept_request_id_and_capacity(request.request())?;
        let selected = self.selected.ok_or(PagerError::InvalidLifecycle)?;
        let region = self
            .regions
            .get(&request.region())
            .copied()
            .ok_or(PagerError::InvalidPeerState)?;
        if !page_range_is_valid(
            region,
            request.offset(),
            request.length(),
            selected.page_size(),
        ) {
            return Err(PagerError::InvalidPeerState);
        }
        self.last_request = request.request().get();
        self.outstanding
            .insert(request.request(), PendingRequest::Page(request));
        Ok(())
    }

    fn accept_remove(&mut self, request: PagerRemoveRequest) -> Result<(), PagerError> {
        self.accept_request_id_and_capacity(request.request())?;
        let selected = self.selected.ok_or(PagerError::InvalidLifecycle)?;
        let region = self
            .regions
            .get(&request.region())
            .copied()
            .ok_or(PagerError::InvalidPeerState)?;
        if !remove_range_is_valid(
            region,
            request.offset(),
            request.length(),
            selected.page_size(),
        ) {
            return Err(PagerError::InvalidPeerState);
        }
        self.last_request = request.request().get();
        self.outstanding
            .insert(request.request(), PendingRequest::Remove(request));
        Ok(())
    }

    fn accept_request_id_and_capacity(&self, request: PagerRequestId) -> Result<(), PagerError> {
        let selected = self.selected.ok_or(PagerError::InvalidLifecycle)?;
        if request.get() <= self.last_request {
            return Err(PagerError::InvalidPeerState);
        }
        if self.outstanding.len() >= usize::from(selected.max_in_flight()) {
            return Err(PagerError::LimitExceeded);
        }
        Ok(())
    }

    fn frame(&self, message: PagerMessage) -> Result<PagerFrame, PagerError> {
        Ok(PagerFrame::new(
            self.session.ok_or(PagerError::InvalidLifecycle)?,
            message,
        ))
    }
}

impl Default for PeerSession {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for PeerSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerSession")
            .field("session", &"<redacted>")
            .field("state", &self.state)
            .field("limits", &"<redacted>")
            .field("regions", &"<redacted>")
            .field("outstanding_count", &self.outstanding.len())
            .finish()
    }
}

fn validate_regions(limits: PagerLimits, regions: &[PagerRegion]) -> Result<(), PagerError> {
    if regions.len() != usize::from(limits.region_count())
        || regions
            .iter()
            .copied()
            .any(|region| !region.is_valid_for(limits.page_size()))
    {
        return Err(PagerError::InvalidConfiguration);
    }
    let mut identities = BTreeMap::new();
    for region in regions {
        if identities.insert(region.id(), *region).is_some() {
            return Err(PagerError::InvalidConfiguration);
        }
    }
    for (index, region) in regions.iter().copied().enumerate() {
        if regions
            .get(index.saturating_add(1)..)
            .unwrap_or_default()
            .iter()
            .copied()
            .any(|other| source_ranges_overlap(region, other))
        {
            return Err(PagerError::InvalidConfiguration);
        }
    }
    Ok(())
}

fn source_ranges_overlap(first: PagerRegion, second: PagerRegion) -> bool {
    let first_end = first.source_offset().saturating_add(first.length());
    let second_end = second.source_offset().saturating_add(second.length());
    first.source_offset() < second_end && second.source_offset() < first_end
}

fn page_range_is_valid(region: PagerRegion, offset: u64, length: u32, page_size: u32) -> bool {
    length == page_size
        && offset.is_multiple_of(u64::from(page_size))
        && offset
            .checked_add(u64::from(length))
            .is_some_and(|end| end <= region.length())
}

fn remove_range_is_valid(region: PagerRegion, offset: u64, length: u64, page_size: u32) -> bool {
    let alignment = u64::from(page_size);
    length != 0
        && offset.is_multiple_of(alignment)
        && length.is_multiple_of(alignment)
        && offset
            .checked_add(length)
            .is_some_and(|end| end <= region.length())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MAX_FRAME_BYTES, MIN_PAGE_SIZE, PagerFrameKind, PagerOperations};

    fn session(byte: u8) -> PagerSessionId {
        PagerSessionId::from_bytes([byte; 32]).expect("session should be nonzero")
    }

    fn limits(in_flight: u16) -> PagerLimits {
        PagerLimits::new(
            MIN_PAGE_SIZE,
            2,
            in_flight,
            u32::try_from(MAX_FRAME_BYTES).expect("maximum should fit"),
            PagerOperations::v1(),
        )
        .expect("limits should be valid")
    }

    fn region(id: u32, source_offset: u64) -> PagerRegion {
        PagerRegion::new(
            PagerRegionId::new(id).expect("region should be nonzero"),
            source_offset,
            u64::from(MIN_PAGE_SIZE) * 2,
            MIN_PAGE_SIZE,
        )
        .expect("region should be valid")
    }

    fn active(in_flight: u16) -> (VmmSession, PeerSession) {
        let offered = limits(in_flight);
        let regions = vec![region(1, 0), region(2, u64::from(MIN_PAGE_SIZE) * 2)];
        let mut vmm = VmmSession::new(session(1), offered, regions).expect("VMM should initialize");
        let mut peer = PeerSession::new();
        peer.receive(vmm.hello().expect("hello should build"))
            .expect("hello should validate");
        vmm.receive(peer.hello_ack(offered).expect("ack should build"))
            .expect("ack should validate");
        for _ in 0..offered.region_count() {
            peer.receive(vmm.next_region().expect("region should build"))
                .expect("region should validate");
        }
        peer.receive(vmm.start().expect("start should build"))
            .expect("start should validate");
        vmm.receive(peer.ready().expect("ready should build"))
            .expect("ready should validate");
        (vmm, peer)
    }

    #[test]
    fn handshake_page_zero_removal_and_shutdown_complete() {
        let (mut vmm, mut peer) = active(4);
        let generation = PagerGeneration::new(1).expect("generation should be nonzero");
        let first = vmm
            .request_page(
                PagerRegionId::new(1).expect("region should be nonzero"),
                generation,
                0,
                PageAccess::Read,
            )
            .expect("request should build");
        let first_id = first
            .page_request()
            .expect("request metadata should exist")
            .request();
        peer.receive(first).expect("request should validate");

        let second = vmm
            .request_page(
                PagerRegionId::new(1).expect("region should be nonzero"),
                generation,
                u64::from(MIN_PAGE_SIZE),
                PageAccess::Write,
            )
            .expect("request should build");
        let second_id = second
            .page_request()
            .expect("request metadata should exist")
            .request();
        peer.receive(second).expect("request should validate");

        vmm.receive(peer.page_zero(second_id).expect("zero should build"))
            .expect("out-of-order zero should validate");
        vmm.receive(
            peer.page_data(first_id, vec![9; MIN_PAGE_SIZE as usize])
                .expect("data should build"),
        )
        .expect("data should validate");

        let removal = vmm
            .remove(
                PagerRegionId::new(1).expect("region should be nonzero"),
                generation,
                0,
                u64::from(MIN_PAGE_SIZE),
            )
            .expect("removal should build");
        let removal_id = removal
            .remove_request()
            .expect("removal metadata should exist")
            .request();
        peer.receive(removal).expect("removal should validate");
        vmm.receive(peer.removed(removal_id).expect("ack should build"))
            .expect("removal ack should validate");

        peer.receive(vmm.shutdown().expect("shutdown should build"))
            .expect("shutdown should validate");
        vmm.receive(peer.shutdown_ack().expect("shutdown ack should build"))
            .expect("shutdown ack should validate");
        assert_eq!(vmm.state(), PagerVmmState::Closed);
        assert_eq!(peer.state(), PagerPeerState::Closed);
    }

    #[test]
    fn combined_bound_and_session_wide_cancellation_are_enforced() {
        let (mut vmm, mut peer) = active(1);
        let generation = PagerGeneration::new(1).expect("generation should be nonzero");
        let request = vmm
            .request_page(
                PagerRegionId::new(1).expect("region should be nonzero"),
                generation,
                0,
                PageAccess::Read,
            )
            .expect("request should build");
        peer.receive(request).expect("request should validate");
        assert_eq!(
            vmm.remove(
                PagerRegionId::new(1).expect("region should be nonzero"),
                generation,
                0,
                u64::from(MIN_PAGE_SIZE)
            ),
            Err(PagerError::LimitExceeded)
        );
        peer.receive(
            vmm.cancel(CancelReason::Requested)
                .expect("cancel should build"),
        )
        .expect("cancel should validate");
        assert_eq!(vmm.outstanding_count(), 0);
        assert_eq!(peer.outstanding_count(), 0);
        vmm.receive(peer.cancelled().expect("cancel ack should build"))
            .expect("cancel ack should validate");
        assert_eq!(vmm.state(), PagerVmmState::Closed);
    }

    #[test]
    fn duplicate_regions_replay_cross_session_and_mismatch_fail_terminal() {
        assert!(
            VmmSession::new(
                session(1),
                limits(2),
                vec![region(1, 0), region(1, u64::from(MIN_PAGE_SIZE) * 2)]
            )
            .is_err()
        );
        assert!(
            VmmSession::new(
                session(1),
                limits(2),
                vec![region(1, 0), region(2, u64::from(MIN_PAGE_SIZE))]
            )
            .is_err()
        );

        let (mut vmm, mut peer) = active(2);
        let generation = PagerGeneration::new(1).expect("generation should be nonzero");
        let request = vmm
            .request_page(
                PagerRegionId::new(1).expect("region should be nonzero"),
                generation,
                0,
                PageAccess::Read,
            )
            .expect("request should build");
        let replay = request.clone();
        peer.receive(request).expect("request should validate");
        assert_eq!(peer.receive(replay), Err(PagerError::InvalidPeerState));
        assert_eq!(peer.state(), PagerPeerState::Terminal);

        let (_, mut other_peer) = active(2);
        let wrong_session = PagerFrame::new(session(9), PagerMessage::Shutdown);
        assert_eq!(
            other_peer.receive(wrong_session),
            Err(PagerError::InvalidPeerState)
        );
        assert_eq!(other_peer.state(), PagerPeerState::Terminal);

        let (mut mismatch_vmm, mut mismatch_peer) = active(2);
        let request = mismatch_vmm
            .request_page(
                PagerRegionId::new(1).expect("region should be nonzero"),
                generation,
                0,
                PageAccess::Read,
            )
            .expect("request should build");
        let id = request
            .page_request()
            .expect("request metadata should exist")
            .request();
        mismatch_peer
            .receive(request)
            .expect("request should validate");
        let valid = mismatch_peer.page_zero(id).expect("response should build");
        let mut encoded = crate::encode_frame(&valid).expect("response should encode");
        let offset_byte = encoded
            .get_mut(crate::HEADER_BYTES + 63)
            .expect("offset byte should exist");
        *offset_byte = 1;
        let mismatched = crate::decode_frame(&encoded).expect("frame remains canonical");
        assert_eq!(
            mismatch_vmm.receive(mismatched),
            Err(PagerError::InvalidPeerState)
        );
        assert_eq!(mismatch_vmm.state(), PagerVmmState::Terminal);
    }

    #[test]
    fn invalid_selection_ranges_shutdown_and_request_overflow_fail() {
        let offered = limits(2);
        let mut vmm = VmmSession::new(
            session(1),
            offered,
            vec![region(1, 0), region(2, u64::from(MIN_PAGE_SIZE) * 2)],
        )
        .expect("VMM should initialize");
        let mut peer = PeerSession::new();
        peer.receive(vmm.hello().expect("hello should build"))
            .expect("hello should validate");
        let invalid = PagerLimits::new(
            MIN_PAGE_SIZE,
            2,
            3,
            offered.max_frame_bytes(),
            PagerOperations::v1(),
        )
        .expect("limits should be intrinsically valid");
        assert_eq!(
            peer.hello_ack(invalid),
            Err(PagerError::InvalidConfiguration)
        );

        let (mut active_vmm, mut active_peer) = active(2);
        assert!(
            active_vmm
                .request_page(
                    PagerRegionId::new(1).expect("region should be nonzero"),
                    PagerGeneration::new(1).expect("generation should be nonzero"),
                    1,
                    PageAccess::Read
                )
                .is_err()
        );
        let outstanding = active_vmm
            .request_page(
                PagerRegionId::new(1).expect("region should be nonzero"),
                PagerGeneration::new(1).expect("generation should be nonzero"),
                0,
                PageAccess::Read,
            )
            .expect("request should build");
        active_peer
            .receive(outstanding)
            .expect("request should validate");
        assert_eq!(active_vmm.shutdown(), Err(PagerError::InvalidLifecycle));

        let (mut overflow_vmm, _) = active(2);
        overflow_vmm.next_request = u64::MAX;
        assert_eq!(
            overflow_vmm.request_page(
                PagerRegionId::new(1).expect("region should be nonzero"),
                PagerGeneration::new(1).expect("generation should be nonzero"),
                0,
                PageAccess::Read
            ),
            Err(PagerError::LimitExceeded)
        );
    }

    #[test]
    fn terminal_is_accepted_from_every_established_live_phase() {
        let offered = limits(2);
        let mut vmm = VmmSession::new(
            session(1),
            offered,
            vec![region(1, 0), region(2, u64::from(MIN_PAGE_SIZE) * 2)],
        )
        .expect("VMM should initialize");
        let mut peer = PeerSession::new();
        peer.receive(vmm.hello().expect("hello should build"))
            .expect("hello should validate");
        let terminal = peer
            .terminal(TerminalCode::Internal)
            .expect("terminal should build");
        assert_eq!(terminal.kind(), PagerFrameKind::Terminal);
        vmm.receive(terminal).expect("terminal should validate");
        assert_eq!(vmm.state(), PagerVmmState::Terminal);
    }
}
