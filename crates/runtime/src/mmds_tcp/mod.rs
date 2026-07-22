// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Portable, bounded MMDS TCP state.
//!
//! The algorithms in this module are a focused semantic adaptation of
//! Firecracker v1.16.0 `dumbo/tcp` at commit
//! `d83d72b710361a10294480131377b1b00b163af8`. The adaptation keeps the
//! passive-open connection, endpoint, and handler behavior while replacing
//! Firecracker-specific packet, time, allocation, HTTP, and metrics ownership.
//! See the colocated `NOTICE.md` and `LICENSE-APACHE-2.0` files.
//!
//! This module is staged for the production integration owned by issue #1499.
//! The current MMDS packet detour does not call it yet.

mod connection;
mod endpoint;
mod handler;
mod segment;

pub use connection::{
    Connection, ConnectionReceiveError, ConnectionWriteError, PassiveOpenError, PayloadSource,
    ReceiveStatus,
};
pub use endpoint::{Endpoint, EndpointReceiveError};
pub use handler::{
    HandlerBuildError, HandlerOutput, HandlerReceiveError, HandlerReceiveEvent, HandlerWriteError,
    HandlerWriteEvent, MmdsTcpHandler, Peer,
};
pub use segment::{OutgoingSegment, SegmentParseError, TcpFlags, TcpSegment};

/// Largest sequence-space interval recognized as a valid TCP window.
pub const MAX_WINDOW_SIZE: u32 = 1_073_725_440;

/// Default maximum segment size when the peer does not advertise one.
pub const DEFAULT_MSS: u16 = 536;

/// Smallest accepted advertised maximum segment size.
pub const MIN_MSS: u16 = 100;

/// Maximum number of simultaneous MMDS TCP connections.
pub const MMDS_TCP_MAX_CONNECTIONS: usize = 30;

/// Maximum number of reset replies waiting for output.
pub const MMDS_TCP_MAX_PENDING_RESETS: usize = 100;

/// Fixed request receive-buffer size for each MMDS TCP endpoint.
pub const MMDS_TCP_RECEIVE_BUFFER_SIZE: usize = 2_500;

/// Inactivity interval after which a connection can be evicted.
pub const MMDS_TCP_EVICTION_THRESHOLD_TICKS: u64 = 40_000_000_000;

/// Time between retransmission attempts.
pub const MMDS_TCP_RETRANSMISSION_PERIOD_TICKS: u64 = 1_200_000_000;

/// Timeout count at which a connection emits a reset instead of retransmitting.
pub const MMDS_TCP_RETRANSMISSION_LIMIT: u16 = 15;

/// Describes whether an entity has TCP output available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextSegmentStatus {
    /// Output can be produced immediately.
    Available,
    /// No output or deadline is pending.
    Nothing,
    /// Output becomes eligible at the supplied monotonic tick.
    Timeout(u64),
}

/// Sequence and acknowledgement fields for an outgoing reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetConfig {
    /// Send the supplied sequence number without the ACK flag.
    Sequence(u32),
    /// Send sequence zero and acknowledge the supplied number.
    Acknowledgement(u32),
}

impl ResetConfig {
    pub(crate) fn from_segment(segment: &TcpSegment<'_>) -> Self {
        if segment.flags().intersects(TcpFlags::ACK) {
            Self::Sequence(segment.acknowledgement_number())
        } else {
            let payload_len = u32::try_from(segment.payload().len()).unwrap_or(u32::MAX);
            Self::Acknowledgement(segment.sequence_number().wrapping_add(payload_len))
        }
    }

    pub(crate) const fn fields(self) -> (u32, u32, TcpFlags) {
        match self {
            Self::Sequence(sequence_number) => (sequence_number, 0, TcpFlags::RESET),
            Self::Acknowledgement(acknowledgement_number) => (
                0,
                acknowledgement_number,
                TcpFlags::RESET.union(TcpFlags::ACK),
            ),
        }
    }
}

/// Error returned when an injected timestamp moves backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimestampRegression {
    previous: u64,
    current: u64,
}

impl TimestampRegression {
    pub(crate) const fn new(previous: u64, current: u64) -> Self {
        Self { previous, current }
    }

    /// Most recently accepted timestamp.
    pub const fn previous(self) -> u64 {
        self.previous
    }

    /// Regressed timestamp supplied by the caller.
    pub const fn current(self) -> u64 {
        self.current
    }
}

impl std::fmt::Display for TimestampRegression {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "MMDS TCP timestamp regressed from {} to {}",
            self.previous, self.current
        )
    }
}

impl std::error::Error for TimestampRegression {}

/// Returns whether `candidate` follows `reference` in the pinned sequence window.
pub const fn sequence_after(candidate: u32, reference: u32) -> bool {
    candidate != reference && candidate.wrapping_sub(reference) < MAX_WINDOW_SIZE
}

/// Returns whether `candidate` equals or follows `reference` in the pinned sequence window.
pub const fn sequence_at_or_after(candidate: u32, reference: u32) -> bool {
    candidate.wrapping_sub(reference) < MAX_WINDOW_SIZE
}

#[cfg(test)]
mod tests;
