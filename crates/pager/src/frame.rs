use std::fmt;

use crate::PagerError;

/// Encoded `bangbang-pager-v1` header size.
pub const HEADER_BYTES: usize = 24;
/// Smallest page size admitted by v1.
pub const MIN_PAGE_SIZE: u32 = 4 * 1024;
/// Largest page size admitted by v1.
pub const MAX_PAGE_SIZE: u32 = 2 * 1024 * 1024;
/// Largest region count admitted by v1.
pub const MAX_REGIONS: u16 = 128;
/// Largest combined page/removal request count admitted by v1.
pub const MAX_IN_FLIGHT: u16 = 256;
/// Largest encoded v1 frame, including one maximum-sized page payload.
pub const MAX_FRAME_BYTES: usize = HEADER_BYTES + PAGE_METADATA_BYTES + MAX_PAGE_SIZE as usize;

const MAGIC: [u8; 8] = *b"BBPAGER\0";
const VERSION: u16 = 1;
const SESSION_BYTES: usize = 32;
const LIMITS_BODY_BYTES: usize = SESSION_BYTES + 24;
const REGION_BODY_BYTES: usize = SESSION_BYTES + 24;
const PAGE_METADATA_BYTES: usize = SESSION_BYTES + 40;
const REMOVE_BODY_BYTES: usize = SESSION_BYTES + 40;
const CANCEL_BODY_BYTES: usize = SESSION_BYTES + 8;
const TERMINAL_BODY_BYTES: usize = SESSION_BYTES + 8;
const MAX_BUFFER_BYTES: usize = MAX_FRAME_BYTES * 2;

const OP_PAGE_DATA: u32 = 1 << 0;
const OP_PAGE_ZERO: u32 = 1 << 1;
const OP_REMOVE: u32 = 1 << 2;
const OP_CANCEL: u32 = 1 << 3;
const OP_SHUTDOWN: u32 = 1 << 4;
const V1_OPERATIONS: u32 = OP_PAGE_DATA | OP_PAGE_ZERO | OP_REMOVE | OP_CANCEL | OP_SHUTDOWN;

/// Random, nonzero identity bound to every frame in one pager session.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PagerSessionId([u8; SESSION_BYTES]);

impl PagerSessionId {
    /// Generates a cryptographically random nonzero session identity.
    pub fn generate() -> Result<Self, PagerError> {
        loop {
            let mut bytes = [0_u8; SESSION_BYTES];
            getrandom::fill(&mut bytes).map_err(|_| PagerError::Randomness)?;
            if bytes != [0; SESSION_BYTES] {
                return Ok(Self(bytes));
            }
        }
    }

    /// Validates exact protocol identity bytes.
    pub fn from_bytes(bytes: [u8; SESSION_BYTES]) -> Result<Self, PagerError> {
        if bytes == [0; SESSION_BYTES] {
            Err(PagerError::InvalidConfiguration)
        } else {
            Ok(Self(bytes))
        }
    }

    /// Returns the exact identity bytes used by the wire codec.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; SESSION_BYTES] {
        &self.0
    }

    fn decode(bytes: [u8; SESSION_BYTES]) -> Result<Self, PagerError> {
        Self::from_bytes(bytes).map_err(|_| PagerError::InvalidFrame)
    }
}

impl fmt::Debug for PagerSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PagerSessionId(<redacted>)")
    }
}

macro_rules! nonzero_identifier {
    ($name:ident, $inner:ty, $label:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name($inner);

        impl $name {
            /// Constructs a validated nonzero identity.
            pub const fn new(value: $inner) -> Result<Self, PagerError> {
                if value == 0 {
                    Err(PagerError::InvalidConfiguration)
                } else {
                    Ok(Self(value))
                }
            }

            /// Returns the exact numeric wire value.
            #[must_use]
            pub const fn get(self) -> $inner {
                self.0
            }

            fn decode(value: $inner) -> Result<Self, PagerError> {
                Self::new(value).map_err(|_| PagerError::InvalidFrame)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!($label, "(<redacted>)"))
            }
        }
    };
}

nonzero_identifier!(PagerRegionId, u32, "PagerRegionId");
nonzero_identifier!(PagerRequestId, u64, "PagerRequestId");
nonzero_identifier!(PagerGeneration, u64, "PagerGeneration");

/// Complete closed operation set required by `bangbang-pager-v1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagerOperations(u32);

impl PagerOperations {
    /// Returns the complete operation set required by v1.
    #[must_use]
    pub const fn v1() -> Self {
        Self(V1_OPERATIONS)
    }

    /// Validates a wire operation mask.
    pub const fn from_bits(bits: u32) -> Result<Self, PagerError> {
        if bits == V1_OPERATIONS {
            Ok(Self(bits))
        } else {
            Err(PagerError::InvalidConfiguration)
        }
    }

    /// Returns the canonical wire mask.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    fn decode(bits: u32) -> Result<Self, PagerError> {
        Self::from_bits(bits).map_err(|_| PagerError::InvalidFrame)
    }
}

/// Exact limits offered or selected during the v1 handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagerLimits {
    page_size: u32,
    region_count: u16,
    max_in_flight: u16,
    max_frame_bytes: u32,
    operations: PagerOperations,
}

impl PagerLimits {
    /// Constructs one canonical v1 limit set.
    pub fn new(
        page_size: u32,
        region_count: u16,
        max_in_flight: u16,
        max_frame_bytes: u32,
        operations: PagerOperations,
    ) -> Result<Self, PagerError> {
        let required_frame = u32::try_from(HEADER_BYTES)
            .ok()
            .and_then(|header| header.checked_add(u32::try_from(PAGE_METADATA_BYTES).ok()?))
            .and_then(|metadata| metadata.checked_add(page_size))
            .ok_or(PagerError::InvalidConfiguration)?;
        if !page_size.is_power_of_two()
            || !(MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&page_size)
            || !(1..=MAX_REGIONS).contains(&region_count)
            || !(1..=MAX_IN_FLIGHT).contains(&max_in_flight)
            || !(required_frame
                ..=u32::try_from(MAX_FRAME_BYTES).map_err(|_| PagerError::InvalidConfiguration)?)
                .contains(&max_frame_bytes)
            || operations != PagerOperations::v1()
        {
            return Err(PagerError::InvalidConfiguration);
        }
        Ok(Self {
            page_size,
            region_count,
            max_in_flight,
            max_frame_bytes,
            operations,
        })
    }

    /// Returns the selected page size.
    #[must_use]
    pub const fn page_size(self) -> u32 {
        self.page_size
    }

    /// Returns the exact number of regions that must follow the handshake.
    #[must_use]
    pub const fn region_count(self) -> u16 {
        self.region_count
    }

    /// Returns the combined outstanding request limit.
    #[must_use]
    pub const fn max_in_flight(self) -> u16 {
        self.max_in_flight
    }

    /// Returns the maximum complete encoded frame size.
    #[must_use]
    pub const fn max_frame_bytes(self) -> u32 {
        self.max_frame_bytes
    }

    /// Returns the closed selected operation set.
    #[must_use]
    pub const fn operations(self) -> PagerOperations {
        self.operations
    }

    /// Returns whether this is a valid peer selection for `offered`.
    #[must_use]
    pub const fn is_selection_for(self, offered: Self) -> bool {
        self.page_size == offered.page_size
            && self.region_count == offered.region_count
            && self.max_in_flight <= offered.max_in_flight
            && self.max_frame_bytes <= offered.max_frame_bytes
            && self.operations.0 == offered.operations.0
    }

    fn decode(
        page_size: u32,
        region_count: u16,
        max_in_flight: u16,
        max_frame_bytes: u32,
        operations: PagerOperations,
    ) -> Result<Self, PagerError> {
        Self::new(
            page_size,
            region_count,
            max_in_flight,
            max_frame_bytes,
            operations,
        )
        .map_err(|_| PagerError::InvalidFrame)
    }
}

/// One offset-only snapshot source region.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PagerRegion {
    id: PagerRegionId,
    source_offset: u64,
    length: u64,
}

impl PagerRegion {
    /// Constructs an aligned, nonempty and nonoverflowing region.
    pub fn new(
        id: PagerRegionId,
        source_offset: u64,
        length: u64,
        page_size: u32,
    ) -> Result<Self, PagerError> {
        let region = Self {
            id,
            source_offset,
            length,
        };
        if region.is_valid_for(page_size) {
            Ok(region)
        } else {
            Err(PagerError::InvalidConfiguration)
        }
    }

    /// Returns the opaque region identity.
    #[must_use]
    pub const fn id(self) -> PagerRegionId {
        self.id
    }

    /// Returns the offset in the peer-owned snapshot source.
    #[must_use]
    pub const fn source_offset(self) -> u64 {
        self.source_offset
    }

    /// Returns the region length.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }

    pub(crate) fn is_valid_for(self, page_size: u32) -> bool {
        let alignment = u64::from(page_size);
        alignment != 0
            && self.length != 0
            && self.source_offset.is_multiple_of(alignment)
            && self.length.is_multiple_of(alignment)
            && self.source_offset.checked_add(self.length).is_some()
    }

    fn decode(id: PagerRegionId, source_offset: u64, length: u64) -> Result<Self, PagerError> {
        if length == 0 || source_offset.checked_add(length).is_none() {
            Err(PagerError::InvalidFrame)
        } else {
            Ok(Self {
                id,
                source_offset,
                length,
            })
        }
    }
}

impl fmt::Debug for PagerRegion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PagerRegion(<redacted>)")
    }
}

/// Access that caused one page request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageAccess {
    /// The page is required for a read.
    Read,
    /// The page is required for a write.
    Write,
}

impl PageAccess {
    const fn wire(self) -> u32 {
        match self {
            Self::Read => 1,
            Self::Write => 2,
        }
    }

    fn decode(value: u32) -> Result<Self, PagerError> {
        match value {
            1 => Ok(Self::Read),
            2 => Ok(Self::Write),
            _ => Err(PagerError::InvalidFrame),
        }
    }
}

/// Closed reason for terminal, session-wide cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// The local restore operation was explicitly cancelled.
    Requested,
    /// A local source or coordinator prerequisite failed.
    SourceFailure,
}

impl CancelReason {
    const fn wire(self) -> u8 {
        match self {
            Self::Requested => 1,
            Self::SourceFailure => 2,
        }
    }

    fn decode(value: u8) -> Result<Self, PagerError> {
        match value {
            1 => Ok(Self::Requested),
            2 => Ok(Self::SourceFailure),
            _ => Err(PagerError::InvalidFrame),
        }
    }
}

/// Stable, string-free terminal failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCode {
    /// The sender rejected malformed peer bytes.
    InvalidFrame,
    /// The sender rejected peer lifecycle or identity state.
    InvalidState,
    /// The sender rejected a negotiated bound.
    LimitExceeded,
    /// The sender encountered an internal failure.
    Internal,
}

impl TerminalCode {
    const fn wire(self) -> u16 {
        match self {
            Self::InvalidFrame => 1,
            Self::InvalidState => 2,
            Self::LimitExceeded => 3,
            Self::Internal => 4,
        }
    }

    fn decode(value: u16) -> Result<Self, PagerError> {
        match value {
            1 => Ok(Self::InvalidFrame),
            2 => Ok(Self::InvalidState),
            3 => Ok(Self::LimitExceeded),
            4 => Ok(Self::Internal),
            _ => Err(PagerError::InvalidFrame),
        }
    }
}

/// Closed v1 frame kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum PagerFrameKind {
    /// VMM identity and limit offer.
    Hello = 1,
    /// Peer limit selection.
    HelloAck = 2,
    /// One offset-only source region.
    Region = 3,
    /// End of exact region configuration.
    Start = 4,
    /// Peer has committed the configuration.
    Ready = 5,
    /// VMM requests one page.
    PageRequest = 6,
    /// Peer returns exact page bytes.
    PageData = 7,
    /// Peer returns an all-zero page.
    PageZero = 8,
    /// VMM reports a removed range.
    Remove = 9,
    /// Peer acknowledges the exact removed range.
    Removed = 10,
    /// VMM cancels the complete session.
    Cancel = 11,
    /// Peer acknowledges terminal cancellation.
    Cancelled = 12,
    /// Either role reports a stable terminal category.
    Terminal = 13,
    /// VMM requests orderly, drained shutdown.
    Shutdown = 14,
    /// Peer acknowledges orderly shutdown.
    ShutdownAck = 15,
}

impl PagerFrameKind {
    fn decode(value: u16) -> Result<Self, PagerError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::Region),
            4 => Ok(Self::Start),
            5 => Ok(Self::Ready),
            6 => Ok(Self::PageRequest),
            7 => Ok(Self::PageData),
            8 => Ok(Self::PageZero),
            9 => Ok(Self::Remove),
            10 => Ok(Self::Removed),
            11 => Ok(Self::Cancel),
            12 => Ok(Self::Cancelled),
            13 => Ok(Self::Terminal),
            14 => Ok(Self::Shutdown),
            15 => Ok(Self::ShutdownAck),
            _ => Err(PagerError::InvalidFrame),
        }
    }
}

/// Exact metadata for one page request.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PagerPageRequest {
    pub(crate) request: PagerRequestId,
    pub(crate) region: PagerRegionId,
    pub(crate) access: PageAccess,
    pub(crate) generation: PagerGeneration,
    pub(crate) offset: u64,
    pub(crate) length: u32,
}

impl PagerPageRequest {
    /// Returns the request identity.
    #[must_use]
    pub const fn request(self) -> PagerRequestId {
        self.request
    }

    /// Returns the configured region identity.
    #[must_use]
    pub const fn region(self) -> PagerRegionId {
        self.region
    }

    /// Returns the fault access kind.
    #[must_use]
    pub const fn access(self) -> PageAccess {
        self.access
    }

    /// Returns the caller-selected region generation.
    #[must_use]
    pub const fn generation(self) -> PagerGeneration {
        self.generation
    }

    /// Returns the region-relative page offset.
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns the exact requested page length.
    #[must_use]
    pub const fn length(self) -> u32 {
        self.length
    }
}

impl fmt::Debug for PagerPageRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PagerPageRequest(<redacted>)")
    }
}

/// Exact metadata echoed by a page-data or page-zero response.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PagerPageResponse(pub(crate) PagerPageRequest);

impl PagerPageResponse {
    /// Returns the request identity.
    #[must_use]
    pub const fn request(self) -> PagerRequestId {
        self.0.request
    }

    /// Returns the configured region identity.
    #[must_use]
    pub const fn region(self) -> PagerRegionId {
        self.0.region
    }

    /// Returns the original fault access kind.
    #[must_use]
    pub const fn access(self) -> PageAccess {
        self.0.access
    }

    /// Returns the exact region generation.
    #[must_use]
    pub const fn generation(self) -> PagerGeneration {
        self.0.generation
    }

    /// Returns the region-relative page offset.
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.0.offset
    }

    /// Returns the exact response length.
    #[must_use]
    pub const fn length(self) -> u32 {
        self.0.length
    }

    pub(crate) const fn request_metadata(self) -> PagerPageRequest {
        self.0
    }
}

impl fmt::Debug for PagerPageResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PagerPageResponse(<redacted>)")
    }
}

/// Exact metadata for a removed source range.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PagerRemoveRequest {
    pub(crate) request: PagerRequestId,
    pub(crate) region: PagerRegionId,
    pub(crate) generation: PagerGeneration,
    pub(crate) offset: u64,
    pub(crate) length: u64,
}

impl PagerRemoveRequest {
    /// Returns the request identity.
    #[must_use]
    pub const fn request(self) -> PagerRequestId {
        self.request
    }

    /// Returns the configured region identity.
    #[must_use]
    pub const fn region(self) -> PagerRegionId {
        self.region
    }

    /// Returns the exact region generation.
    #[must_use]
    pub const fn generation(self) -> PagerGeneration {
        self.generation
    }

    /// Returns the region-relative removed offset.
    #[must_use]
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns the removed length.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }
}

impl fmt::Debug for PagerRemoveRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PagerRemoveRequest(<redacted>)")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum PagerMessage {
    Hello(PagerLimits),
    HelloAck(PagerLimits),
    Region(PagerRegion),
    Start,
    Ready,
    PageRequest(PagerPageRequest),
    PageData(PagerPageResponse, Vec<u8>),
    PageZero(PagerPageResponse),
    Remove(PagerRemoveRequest),
    Removed(PagerRemoveRequest),
    Cancel(CancelReason),
    Cancelled,
    Terminal(TerminalCode),
    Shutdown,
    ShutdownAck,
}

impl PagerMessage {
    const fn kind(&self) -> PagerFrameKind {
        match self {
            Self::Hello(_) => PagerFrameKind::Hello,
            Self::HelloAck(_) => PagerFrameKind::HelloAck,
            Self::Region(_) => PagerFrameKind::Region,
            Self::Start => PagerFrameKind::Start,
            Self::Ready => PagerFrameKind::Ready,
            Self::PageRequest(_) => PagerFrameKind::PageRequest,
            Self::PageData(_, _) => PagerFrameKind::PageData,
            Self::PageZero(_) => PagerFrameKind::PageZero,
            Self::Remove(_) => PagerFrameKind::Remove,
            Self::Removed(_) => PagerFrameKind::Removed,
            Self::Cancel(_) => PagerFrameKind::Cancel,
            Self::Cancelled => PagerFrameKind::Cancelled,
            Self::Terminal(_) => PagerFrameKind::Terminal,
            Self::Shutdown => PagerFrameKind::Shutdown,
            Self::ShutdownAck => PagerFrameKind::ShutdownAck,
        }
    }
}

/// One validated v1 frame.
#[derive(Clone, PartialEq, Eq)]
pub struct PagerFrame {
    session: PagerSessionId,
    pub(crate) message: PagerMessage,
}

impl PagerFrame {
    pub(crate) const fn new(session: PagerSessionId, message: PagerMessage) -> Self {
        Self { session, message }
    }

    /// Returns the bound session identity.
    #[must_use]
    pub const fn session(&self) -> PagerSessionId {
        self.session
    }

    /// Returns the closed frame kind.
    #[must_use]
    pub const fn kind(&self) -> PagerFrameKind {
        self.message.kind()
    }

    /// Returns handshake limits when this is Hello or HelloAck.
    #[must_use]
    pub const fn limits(&self) -> Option<PagerLimits> {
        match self.message {
            PagerMessage::Hello(limits) | PagerMessage::HelloAck(limits) => Some(limits),
            _ => None,
        }
    }

    /// Returns region metadata when this is Region.
    #[must_use]
    pub const fn region(&self) -> Option<PagerRegion> {
        match self.message {
            PagerMessage::Region(region) => Some(region),
            _ => None,
        }
    }

    /// Returns request metadata when this is PageRequest.
    #[must_use]
    pub const fn page_request(&self) -> Option<PagerPageRequest> {
        match self.message {
            PagerMessage::PageRequest(request) => Some(request),
            _ => None,
        }
    }

    /// Returns echoed response metadata for PageData or PageZero.
    #[must_use]
    pub const fn page_response(&self) -> Option<PagerPageResponse> {
        match self.message {
            PagerMessage::PageData(response, _) | PagerMessage::PageZero(response) => {
                Some(response)
            }
            _ => None,
        }
    }

    /// Returns page bytes only when this is PageData.
    #[must_use]
    pub fn page_data(&self) -> Option<&[u8]> {
        match &self.message {
            PagerMessage::PageData(_, data) => Some(data),
            _ => None,
        }
    }

    /// Returns exact removal metadata for Remove or Removed.
    #[must_use]
    pub const fn remove_request(&self) -> Option<PagerRemoveRequest> {
        match self.message {
            PagerMessage::Remove(request) | PagerMessage::Removed(request) => Some(request),
            _ => None,
        }
    }

    /// Returns the closed cancellation reason when this is Cancel.
    #[must_use]
    pub const fn cancel_reason(&self) -> Option<CancelReason> {
        match self.message {
            PagerMessage::Cancel(reason) => Some(reason),
            _ => None,
        }
    }

    /// Returns the stable category when this is Terminal.
    #[must_use]
    pub const fn terminal_code(&self) -> Option<TerminalCode> {
        match self.message {
            PagerMessage::Terminal(code) => Some(code),
            _ => None,
        }
    }
}

impl fmt::Debug for PagerFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PagerFrame")
            .field("session", &"<redacted>")
            .field("kind", &self.kind())
            .field("contents", &"<redacted>")
            .finish()
    }
}

/// Encodes one validated canonical v1 frame.
pub fn encode_frame(frame: &PagerFrame) -> Result<Vec<u8>, PagerError> {
    let mut body = Vec::with_capacity(PAGE_METADATA_BYTES);
    body.extend_from_slice(frame.session.as_bytes());
    match &frame.message {
        PagerMessage::Hello(limits) | PagerMessage::HelloAck(limits) => {
            encode_limits(&mut body, *limits);
        }
        PagerMessage::Region(region) => {
            push_u32(&mut body, region.id().get());
            push_u32(&mut body, 0);
            push_u64(&mut body, region.source_offset());
            push_u64(&mut body, region.length());
        }
        PagerMessage::Start
        | PagerMessage::Ready
        | PagerMessage::Cancelled
        | PagerMessage::Shutdown
        | PagerMessage::ShutdownAck => {}
        PagerMessage::PageRequest(request) => encode_page_metadata(&mut body, *request),
        PagerMessage::PageData(response, data) => {
            if data.len()
                != usize::try_from(response.length()).map_err(|_| PagerError::InvalidFrame)?
            {
                return Err(PagerError::InvalidFrame);
            }
            encode_page_metadata(&mut body, response.request_metadata());
            body.extend_from_slice(data);
        }
        PagerMessage::PageZero(response) => {
            encode_page_metadata(&mut body, response.request_metadata());
        }
        PagerMessage::Remove(request) | PagerMessage::Removed(request) => {
            encode_remove_metadata(&mut body, *request);
        }
        PagerMessage::Cancel(reason) => {
            body.push(reason.wire());
            body.extend_from_slice(&[0; 7]);
        }
        PagerMessage::Terminal(code) => {
            push_u16(&mut body, code.wire());
            body.extend_from_slice(&[0; 6]);
        }
    }
    let total = HEADER_BYTES
        .checked_add(body.len())
        .ok_or(PagerError::LimitExceeded)?;
    if total > MAX_FRAME_BYTES {
        return Err(PagerError::LimitExceeded);
    }
    let body_len = u32::try_from(body.len()).map_err(|_| PagerError::LimitExceeded)?;
    let mut encoded = Vec::with_capacity(total);
    encoded.extend_from_slice(&MAGIC);
    push_u16(&mut encoded, VERSION);
    push_u16(&mut encoded, frame.kind() as u16);
    push_u32(&mut encoded, body_len);
    push_u32(&mut encoded, 0);
    push_u32(&mut encoded, 0);
    encoded.extend_from_slice(&body);
    Ok(encoded)
}

/// Decodes exactly one complete v1 frame and rejects trailing bytes.
pub fn decode_frame(encoded: &[u8]) -> Result<PagerFrame, PagerError> {
    let (kind, total) = declared_frame(encoded)?;
    if encoded.len() < total {
        return Err(PagerError::UnexpectedEof);
    }
    if encoded.len() != total {
        return Err(PagerError::InvalidFrame);
    }
    let body = encoded
        .get(HEADER_BYTES..total)
        .ok_or(PagerError::InvalidFrame)?;
    decode_body(kind, body)
}

pub(crate) fn declared_frame(encoded: &[u8]) -> Result<(PagerFrameKind, usize), PagerError> {
    if encoded.len() < HEADER_BYTES {
        return Err(PagerError::UnexpectedEof);
    }
    let mut header = Reader::new(
        encoded
            .get(..HEADER_BYTES)
            .ok_or(PagerError::UnexpectedEof)?,
    );
    if header.array::<8>()? != MAGIC || header.u16()? != VERSION {
        return Err(PagerError::InvalidFrame);
    }
    let kind = PagerFrameKind::decode(header.u16()?)?;
    let body_len = usize::try_from(header.u32()?).map_err(|_| PagerError::LimitExceeded)?;
    if header.u32()? != 0 || header.u32()? != 0 || !header.is_finished() {
        return Err(PagerError::InvalidFrame);
    }
    let total = HEADER_BYTES
        .checked_add(body_len)
        .ok_or(PagerError::LimitExceeded)?;
    if body_len < SESSION_BYTES || total > MAX_FRAME_BYTES {
        return Err(PagerError::LimitExceeded);
    }
    Ok((kind, total))
}

/// Bounded incremental decoder for split and coalesced stream input.
#[derive(Default)]
pub struct PagerFrameDecoder {
    buffer: Vec<u8>,
    poisoned: bool,
}

impl PagerFrameDecoder {
    /// Creates an empty incremental decoder.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: Vec::new(),
            poisoned: false,
        }
    }

    /// Adds bytes and returns every newly complete frame.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<PagerFrame>, PagerError> {
        if self.poisoned {
            return Err(PagerError::Poisoned);
        }
        let result = self.push_inner(bytes);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn push_inner(&mut self, bytes: &[u8]) -> Result<Vec<PagerFrame>, PagerError> {
        let buffered = self
            .buffer
            .len()
            .checked_add(bytes.len())
            .ok_or(PagerError::LimitExceeded)?;
        if buffered > MAX_BUFFER_BYTES {
            return Err(PagerError::LimitExceeded);
        }
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();
        let mut consumed = 0_usize;
        loop {
            let remaining = self
                .buffer
                .get(consumed..)
                .ok_or(PagerError::InvalidFrame)?;
            if remaining.len() < HEADER_BYTES {
                break;
            }
            let (_, total) = declared_frame(remaining)?;
            if remaining.len() < total {
                break;
            }
            let encoded = remaining.get(..total).ok_or(PagerError::InvalidFrame)?;
            frames.push(decode_frame(encoded)?);
            consumed = consumed
                .checked_add(total)
                .ok_or(PagerError::LimitExceeded)?;
        }
        if consumed != 0 {
            self.buffer.drain(..consumed);
        }
        Ok(frames)
    }

    /// Rejects a stream that ends with a partial frame.
    pub fn finish(&self) -> Result<(), PagerError> {
        if self.poisoned {
            Err(PagerError::Poisoned)
        } else if self.buffer.is_empty() {
            Ok(())
        } else {
            Err(PagerError::UnexpectedEof)
        }
    }

    /// Returns whether malformed input made this decoder terminal.
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Returns the currently retained partial-byte count.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        self.buffer.len()
    }
}

impl fmt::Debug for PagerFrameDecoder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PagerFrameDecoder")
            .field("buffered_bytes", &self.buffer.len())
            .field("poisoned", &self.poisoned)
            .field("contents", &"<redacted>")
            .finish()
    }
}

fn encode_limits(body: &mut Vec<u8>, limits: PagerLimits) {
    push_u32(body, limits.page_size());
    push_u16(body, limits.region_count());
    push_u16(body, limits.max_in_flight());
    push_u32(body, limits.max_frame_bytes());
    push_u32(body, limits.operations().bits());
    body.extend_from_slice(&[0; 8]);
}

fn encode_page_metadata(body: &mut Vec<u8>, request: PagerPageRequest) {
    push_u64(body, request.request().get());
    push_u32(body, request.region().get());
    push_u32(body, request.access().wire());
    push_u64(body, request.generation().get());
    push_u64(body, request.offset());
    push_u32(body, request.length());
    push_u32(body, 0);
}

fn encode_remove_metadata(body: &mut Vec<u8>, request: PagerRemoveRequest) {
    push_u64(body, request.request().get());
    push_u32(body, request.region().get());
    push_u32(body, 0);
    push_u64(body, request.generation().get());
    push_u64(body, request.offset());
    push_u64(body, request.length());
}

fn decode_body(kind: PagerFrameKind, body: &[u8]) -> Result<PagerFrame, PagerError> {
    let mut reader = Reader::new(body);
    let session = PagerSessionId::decode(reader.array::<SESSION_BYTES>()?)?;
    let message = match kind {
        PagerFrameKind::Hello | PagerFrameKind::HelloAck => {
            require_body_len(body, LIMITS_BODY_BYTES)?;
            let page_size = reader.u32()?;
            let region_count = reader.u16()?;
            let max_in_flight = reader.u16()?;
            let max_frame_bytes = reader.u32()?;
            let operations = PagerOperations::decode(reader.u32()?)?;
            reader.zeroes(8)?;
            let limits = PagerLimits::decode(
                page_size,
                region_count,
                max_in_flight,
                max_frame_bytes,
                operations,
            )?;
            if kind == PagerFrameKind::Hello {
                PagerMessage::Hello(limits)
            } else {
                PagerMessage::HelloAck(limits)
            }
        }
        PagerFrameKind::Region => {
            require_body_len(body, REGION_BODY_BYTES)?;
            let id = PagerRegionId::decode(reader.u32()?)?;
            reader.zeroes(4)?;
            let source_offset = reader.u64()?;
            let length = reader.u64()?;
            PagerMessage::Region(PagerRegion::decode(id, source_offset, length)?)
        }
        PagerFrameKind::Start
        | PagerFrameKind::Ready
        | PagerFrameKind::Cancelled
        | PagerFrameKind::Shutdown
        | PagerFrameKind::ShutdownAck => {
            require_body_len(body, SESSION_BYTES)?;
            match kind {
                PagerFrameKind::Start => PagerMessage::Start,
                PagerFrameKind::Ready => PagerMessage::Ready,
                PagerFrameKind::Cancelled => PagerMessage::Cancelled,
                PagerFrameKind::Shutdown => PagerMessage::Shutdown,
                PagerFrameKind::ShutdownAck => PagerMessage::ShutdownAck,
                _ => return Err(PagerError::InvalidFrame),
            }
        }
        PagerFrameKind::PageRequest | PagerFrameKind::PageData | PagerFrameKind::PageZero => {
            if kind == PagerFrameKind::PageData {
                if body.len() < PAGE_METADATA_BYTES {
                    return Err(PagerError::InvalidFrame);
                }
            } else {
                require_body_len(body, PAGE_METADATA_BYTES)?;
            }
            let metadata = decode_page_metadata(&mut reader)?;
            match kind {
                PagerFrameKind::PageRequest => PagerMessage::PageRequest(metadata),
                PagerFrameKind::PageZero => PagerMessage::PageZero(PagerPageResponse(metadata)),
                PagerFrameKind::PageData => {
                    let expected =
                        usize::try_from(metadata.length).map_err(|_| PagerError::InvalidFrame)?;
                    let data = reader.remaining();
                    if data.len() != expected {
                        return Err(PagerError::InvalidFrame);
                    }
                    PagerMessage::PageData(PagerPageResponse(metadata), data.to_vec())
                }
                _ => return Err(PagerError::InvalidFrame),
            }
        }
        PagerFrameKind::Remove | PagerFrameKind::Removed => {
            require_body_len(body, REMOVE_BODY_BYTES)?;
            let request = decode_remove_metadata(&mut reader)?;
            if kind == PagerFrameKind::Remove {
                PagerMessage::Remove(request)
            } else {
                PagerMessage::Removed(request)
            }
        }
        PagerFrameKind::Cancel => {
            require_body_len(body, CANCEL_BODY_BYTES)?;
            let reason = CancelReason::decode(reader.u8()?)?;
            reader.zeroes(7)?;
            PagerMessage::Cancel(reason)
        }
        PagerFrameKind::Terminal => {
            require_body_len(body, TERMINAL_BODY_BYTES)?;
            let code = TerminalCode::decode(reader.u16()?)?;
            reader.zeroes(6)?;
            PagerMessage::Terminal(code)
        }
    };
    if !reader.is_finished() {
        return Err(PagerError::InvalidFrame);
    }
    Ok(PagerFrame { session, message })
}

fn decode_page_metadata(reader: &mut Reader<'_>) -> Result<PagerPageRequest, PagerError> {
    let request = PagerRequestId::decode(reader.u64()?)?;
    let region = PagerRegionId::decode(reader.u32()?)?;
    let access = PageAccess::decode(reader.u32()?)?;
    let generation = PagerGeneration::decode(reader.u64()?)?;
    let offset = reader.u64()?;
    let length = reader.u32()?;
    reader.zeroes(4)?;
    if length == 0 || offset.checked_add(u64::from(length)).is_none() {
        return Err(PagerError::InvalidFrame);
    }
    Ok(PagerPageRequest {
        request,
        region,
        access,
        generation,
        offset,
        length,
    })
}

fn decode_remove_metadata(reader: &mut Reader<'_>) -> Result<PagerRemoveRequest, PagerError> {
    let request = PagerRequestId::decode(reader.u64()?)?;
    let region = PagerRegionId::decode(reader.u32()?)?;
    reader.zeroes(4)?;
    let generation = PagerGeneration::decode(reader.u64()?)?;
    let offset = reader.u64()?;
    let length = reader.u64()?;
    if length == 0 || offset.checked_add(length).is_none() {
        return Err(PagerError::InvalidFrame);
    }
    Ok(PagerRemoveRequest {
        request,
        region,
        generation,
        offset,
        length,
    })
}

fn require_body_len(body: &[u8], expected: usize) -> Result<(), PagerError> {
    if body.len() == expected {
        Ok(())
    } else {
        Err(PagerError::InvalidFrame)
    }
}

fn push_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn push_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], PagerError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(PagerError::InvalidFrame)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(PagerError::UnexpectedEof)?;
        self.offset = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], PagerError> {
        self.take(N)?
            .try_into()
            .map_err(|_| PagerError::InvalidFrame)
    }

    fn u8(&mut self) -> Result<u8, PagerError> {
        self.array::<1>().map(u8::from_be_bytes)
    }

    fn u16(&mut self) -> Result<u16, PagerError> {
        self.array::<2>().map(u16::from_be_bytes)
    }

    fn u32(&mut self) -> Result<u32, PagerError> {
        self.array::<4>().map(u32::from_be_bytes)
    }

    fn u64(&mut self) -> Result<u64, PagerError> {
        self.array::<8>().map(u64::from_be_bytes)
    }

    fn zeroes(&mut self, length: usize) -> Result<(), PagerError> {
        if self.take(length)?.iter().all(|byte| *byte == 0) {
            Ok(())
        } else {
            Err(PagerError::InvalidFrame)
        }
    }

    fn remaining(&mut self) -> &'a [u8] {
        let remaining = self.bytes.get(self.offset..).unwrap_or_default();
        self.offset = self.bytes.len();
        remaining
    }

    const fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> PagerSessionId {
        PagerSessionId::from_bytes([7; SESSION_BYTES]).expect("identity should be nonzero")
    }

    fn limits() -> PagerLimits {
        PagerLimits::new(MIN_PAGE_SIZE, 1, 4, 8 * 1024, PagerOperations::v1())
            .expect("limits should be valid")
    }

    fn page() -> PagerPageRequest {
        PagerPageRequest {
            request: PagerRequestId::new(1).expect("request should be nonzero"),
            region: PagerRegionId::new(2).expect("region should be nonzero"),
            access: PageAccess::Read,
            generation: PagerGeneration::new(3).expect("generation should be nonzero"),
            offset: 0,
            length: MIN_PAGE_SIZE,
        }
    }

    fn remove() -> PagerRemoveRequest {
        PagerRemoveRequest {
            request: PagerRequestId::new(4).expect("request should be nonzero"),
            region: PagerRegionId::new(2).expect("region should be nonzero"),
            generation: PagerGeneration::new(3).expect("generation should be nonzero"),
            offset: 0,
            length: u64::from(MIN_PAGE_SIZE),
        }
    }

    #[test]
    fn every_frame_kind_round_trips() {
        let region = PagerRegion::new(
            PagerRegionId::new(2).expect("region should be nonzero"),
            0,
            u64::from(MIN_PAGE_SIZE),
            MIN_PAGE_SIZE,
        )
        .expect("region should be valid");
        let page = page();
        let remove = remove();
        let messages = [
            PagerMessage::Hello(limits()),
            PagerMessage::HelloAck(limits()),
            PagerMessage::Region(region),
            PagerMessage::Start,
            PagerMessage::Ready,
            PagerMessage::PageRequest(page),
            PagerMessage::PageData(PagerPageResponse(page), vec![9; MIN_PAGE_SIZE as usize]),
            PagerMessage::PageZero(PagerPageResponse(page)),
            PagerMessage::Remove(remove),
            PagerMessage::Removed(remove),
            PagerMessage::Cancel(CancelReason::Requested),
            PagerMessage::Cancelled,
            PagerMessage::Terminal(TerminalCode::Internal),
            PagerMessage::Shutdown,
            PagerMessage::ShutdownAck,
        ];
        for message in messages {
            let frame = PagerFrame::new(session(), message);
            let encoded = encode_frame(&frame).expect("frame should encode");
            assert_eq!(decode_frame(&encoded), Ok(frame));
        }
    }

    #[test]
    fn every_split_and_coalesced_frames_decode() {
        let first = PagerFrame::new(session(), PagerMessage::Hello(limits()));
        let second = PagerFrame::new(session(), PagerMessage::Start);
        let encoded = encode_frame(&first).expect("frame should encode");
        for split in 0..=encoded.len() {
            let mut decoder = PagerFrameDecoder::new();
            assert_eq!(
                decoder
                    .push(encoded.get(..split).expect("split should exist"))
                    .expect("prefix should decode"),
                if split == encoded.len() {
                    vec![first.clone()]
                } else {
                    Vec::new()
                }
            );
            let suffix = encoded.get(split..).expect("split should exist");
            let frames = decoder.push(suffix).expect("suffix should decode");
            if split == encoded.len() {
                assert!(frames.is_empty());
            } else {
                assert_eq!(frames, vec![first.clone()]);
            }
            decoder.finish().expect("decoder should be complete");
        }

        let mut coalesced = encoded;
        coalesced.extend_from_slice(&encode_frame(&second).expect("frame should encode"));
        assert_eq!(
            PagerFrameDecoder::new()
                .push(&coalesced)
                .expect("coalesced frames should decode"),
            vec![first, second]
        );

        let small = encode_frame(&PagerFrame::new(session(), PagerMessage::Start))
            .expect("small frame should encode");
        let many = small.repeat(4096);
        let frames = PagerFrameDecoder::new()
            .push(&many)
            .expect("many coalesced frames should decode");
        assert_eq!(frames.len(), 4096);
    }

    #[test]
    fn corrupt_headers_reserved_fields_and_trailing_bytes_fail_closed() {
        let frame = PagerFrame::new(session(), PagerMessage::Hello(limits()));
        let encoded = encode_frame(&frame).expect("frame should encode");
        for offset in [0_usize, 8, 10, 16, 20] {
            let mut corrupt = encoded.clone();
            let byte = corrupt.get_mut(offset).expect("header byte should exist");
            *byte ^= 0xff;
            assert!(decode_frame(&corrupt).is_err());
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert_eq!(decode_frame(&trailing), Err(PagerError::InvalidFrame));
        assert_eq!(
            decode_frame(
                encoded
                    .get(..encoded.len().saturating_sub(1))
                    .expect("prefix should exist")
            ),
            Err(PagerError::UnexpectedEof)
        );

        let region = PagerRegion::new(
            PagerRegionId::new(2).expect("region should be nonzero"),
            0,
            u64::from(MIN_PAGE_SIZE),
            MIN_PAGE_SIZE,
        )
        .expect("region should be valid");
        let mut body_reserved =
            encode_frame(&PagerFrame::new(session(), PagerMessage::Region(region)))
                .expect("region should encode");
        *body_reserved
            .get_mut(HEADER_BYTES + SESSION_BYTES + 4)
            .expect("region reserved byte should exist") = 1;
        assert_eq!(decode_frame(&body_reserved), Err(PagerError::InvalidFrame));

        let mut zero_session = encoded.clone();
        zero_session
            .get_mut(HEADER_BYTES..HEADER_BYTES + SESSION_BYTES)
            .expect("session field should exist")
            .fill(0);
        assert_eq!(decode_frame(&zero_session), Err(PagerError::InvalidFrame));

        let mut zero_request = encode_frame(&PagerFrame::new(
            session(),
            PagerMessage::PageRequest(page()),
        ))
        .expect("page request should encode");
        zero_request
            .get_mut(HEADER_BYTES + SESSION_BYTES..HEADER_BYTES + SESSION_BYTES + 8)
            .expect("request identity should exist")
            .fill(0);
        assert_eq!(decode_frame(&zero_request), Err(PagerError::InvalidFrame));
    }

    #[test]
    fn bounds_and_linux_uffd_shaped_input_are_rejected() {
        assert!(
            PagerLimits::new(MIN_PAGE_SIZE - 1, 1, 1, 8 * 1024, PagerOperations::v1()).is_err()
        );
        assert!(
            PagerLimits::new(
                MAX_PAGE_SIZE.saturating_mul(2),
                1,
                1,
                u32::MAX,
                PagerOperations::v1()
            )
            .is_err()
        );
        let linux_uffd = br#"{"uffd":7,"regions":[{"base_host_virt_addr":4096}]}"#;
        assert_eq!(decode_frame(linux_uffd), Err(PagerError::InvalidFrame));

        let mut decoder = PagerFrameDecoder::new();
        assert_eq!(decoder.push(linux_uffd), Err(PagerError::InvalidFrame));
        assert!(decoder.is_poisoned());
        assert_eq!(
            decoder.push(
                &encode_frame(&PagerFrame::new(session(), PagerMessage::Start))
                    .expect("valid frame should encode")
            ),
            Err(PagerError::Poisoned)
        );
        assert_eq!(decoder.finish(), Err(PagerError::Poisoned));
    }

    #[test]
    fn exact_global_edges_succeed_and_one_over_edges_fail() {
        let global_frame =
            u32::try_from(MAX_FRAME_BYTES).expect("maximum frame size should fit u32");
        let minimum_frame = u32::try_from(HEADER_BYTES + PAGE_METADATA_BYTES)
            .expect("metadata size should fit u32")
            .checked_add(MIN_PAGE_SIZE)
            .expect("minimum frame should fit");
        assert!(
            PagerLimits::new(MIN_PAGE_SIZE, 1, 1, minimum_frame, PagerOperations::v1(),).is_ok()
        );
        assert!(
            PagerLimits::new(
                MAX_PAGE_SIZE,
                MAX_REGIONS,
                MAX_IN_FLIGHT,
                global_frame,
                PagerOperations::v1(),
            )
            .is_ok()
        );

        for invalid in [
            PagerLimits::new(MIN_PAGE_SIZE, 0, 1, minimum_frame, PagerOperations::v1()),
            PagerLimits::new(
                MIN_PAGE_SIZE,
                MAX_REGIONS + 1,
                1,
                minimum_frame,
                PagerOperations::v1(),
            ),
            PagerLimits::new(MIN_PAGE_SIZE, 1, 0, minimum_frame, PagerOperations::v1()),
            PagerLimits::new(
                MIN_PAGE_SIZE,
                1,
                MAX_IN_FLIGHT + 1,
                minimum_frame,
                PagerOperations::v1(),
            ),
            PagerLimits::new(
                MIN_PAGE_SIZE,
                1,
                1,
                minimum_frame - 1,
                PagerOperations::v1(),
            ),
            PagerLimits::new(
                MAX_PAGE_SIZE,
                MAX_REGIONS,
                MAX_IN_FLIGHT,
                global_frame + 1,
                PagerOperations::v1(),
            ),
        ] {
            assert_eq!(invalid, Err(PagerError::InvalidConfiguration));
        }
        assert_eq!(
            PagerOperations::from_bits(V1_OPERATIONS | (1 << 31)),
            Err(PagerError::InvalidConfiguration)
        );

        let mut maximum_page = page();
        maximum_page.length = MAX_PAGE_SIZE;
        let maximum = PagerFrame::new(
            session(),
            PagerMessage::PageData(
                PagerPageResponse(maximum_page),
                vec![0xa5; MAX_PAGE_SIZE as usize],
            ),
        );
        let encoded = encode_frame(&maximum).expect("maximum frame should encode");
        assert_eq!(encoded.len(), MAX_FRAME_BYTES);
        assert_eq!(decode_frame(&encoded), Ok(maximum));

        maximum_page.length = MAX_PAGE_SIZE + 1;
        let one_over = PagerFrame::new(
            session(),
            PagerMessage::PageData(
                PagerPageResponse(maximum_page),
                vec![0xa5; MAX_PAGE_SIZE as usize + 1],
            ),
        );
        assert_eq!(encode_frame(&one_over), Err(PagerError::LimitExceeded));

        let page_size = u64::from(MIN_PAGE_SIZE);
        let largest_aligned = u64::MAX - (u64::MAX % page_size);
        assert!(
            PagerRegion::new(
                PagerRegionId::new(1).expect("region should be nonzero"),
                largest_aligned - page_size,
                page_size,
                MIN_PAGE_SIZE,
            )
            .is_ok()
        );
        assert_eq!(
            PagerRegion::new(
                PagerRegionId::new(1).expect("region should be nonzero"),
                largest_aligned,
                page_size,
                MIN_PAGE_SIZE,
            ),
            Err(PagerError::InvalidConfiguration)
        );
        assert_eq!(
            PagerSessionId::from_bytes([0; SESSION_BYTES]),
            Err(PagerError::InvalidConfiguration)
        );
        assert_eq!(
            PagerRequestId::new(0),
            Err(PagerError::InvalidConfiguration)
        );
    }

    #[test]
    fn frame_debug_redacts_session_offsets_and_payload() {
        let frame = PagerFrame::new(
            session(),
            PagerMessage::PageData(
                PagerPageResponse(page()),
                vec![0xab; MIN_PAGE_SIZE as usize],
            ),
        );
        let debug = format!("{frame:?}");
        assert!(debug.contains("PageData"));
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("171"));
        assert!(!debug.contains("PagerSessionId"));
    }
}
