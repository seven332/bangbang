//! Closed, bounded macOS snapshot-pager protocol primitives.
//!
//! `bangbang-pager-v1` is shared by a future VMM coordinator and its external
//! page-content peer. The crate accepts only an already connected Unix stream;
//! it owns no socket path, process launch, descriptor transfer, guest mapping,
//! Mach/HVF object, or public API behavior.

#[cfg(not(unix))]
compile_error!("bangbang-pager requires Unix socket semantics");

mod client;
mod error;
mod frame;
mod reference;
mod state;
mod transport;

pub use client::{PagerClient, PagerClientPage, PagerClientState, PagerClientTerminalObserver};
pub use error::PagerError;
pub use frame::{
    CancelReason, HEADER_BYTES, MAX_FRAME_BYTES, MAX_IN_FLIGHT, MAX_PAGE_SIZE, MAX_REGIONS,
    MIN_PAGE_SIZE, PageAccess, PagerFrame, PagerFrameDecoder, PagerFrameKind, PagerGeneration,
    PagerLimits, PagerOperations, PagerPageRequest, PagerPageResponse, PagerRegion, PagerRegionId,
    PagerRemoveRequest, PagerRequestId, PagerSessionId, TerminalCode, decode_frame, encode_frame,
};
pub use reference::{
    REFERENCE_PAGE_BYTE, ReferencePeer, ReferencePeerReport, ReferencePeerTermination,
};
pub use state::{PagerPeerState, PagerVmmState, PeerSession, VmmSession};
pub use transport::PagerTransport;
