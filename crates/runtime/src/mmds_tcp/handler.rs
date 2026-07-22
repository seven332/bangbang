// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Bounded connection handler adapted from Firecracker v1.16.0
//! `dumbo/tcp/handler.rs` and `mmds/ns.rs`.

use std::collections::TryReserveError;
use std::fmt;
use std::net::Ipv4Addr;

use super::{
    ConnectionWriteError, Endpoint, EndpointReceiveError, MMDS_TCP_MAX_CONNECTIONS,
    MMDS_TCP_MAX_PENDING_RESETS, NextSegmentStatus, OutgoingSegment, ResetConfig,
    SegmentParseError, TcpFlags, TcpSegment, TimestampRegression,
};

/// Remote half of an MMDS TCP connection key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peer {
    ipv4_address: Ipv4Addr,
    port: u16,
}

impl Peer {
    /// Creates a remote connection key.
    pub const fn new(ipv4_address: Ipv4Addr, port: u16) -> Self {
        Self { ipv4_address, port }
    }

    /// Remote IPv4 address.
    pub const fn ipv4_address(self) -> Ipv4Addr {
        self.ipv4_address
    }

    /// Remote TCP port.
    pub const fn port(self) -> u16 {
        self.port
    }
}

#[derive(Debug)]
struct ConnectionEntry {
    peer: Peer,
    endpoint: Endpoint,
}

#[derive(Debug, Clone, Copy)]
struct PendingReset {
    peer: Peer,
    config: ResetConfig,
}

/// Fixed-capacity MMDS TCP connection and reset handler.
#[derive(Debug)]
pub struct MmdsTcpHandler {
    local_port: u16,
    connections: Vec<ConnectionEntry>,
    connection_limit: usize,
    pending_resets: Vec<PendingReset>,
    pending_reset_limit: usize,
    next_connection: usize,
    last_timestamp: u64,
}

impl MmdsTcpHandler {
    /// Allocates the exact logical Firecracker MMDS limits.
    pub fn try_new(local_port: u16) -> Result<Self, HandlerBuildError> {
        Self::try_new_with_limits(
            local_port,
            MMDS_TCP_MAX_CONNECTIONS,
            MMDS_TCP_MAX_PENDING_RESETS,
        )
    }

    fn try_new_with_limits(
        local_port: u16,
        connection_limit: usize,
        pending_reset_limit: usize,
    ) -> Result<Self, HandlerBuildError> {
        let mut connections = Vec::new();
        connections
            .try_reserve_exact(connection_limit)
            .map_err(HandlerBuildError::ConnectionStorage)?;
        let mut pending_resets = Vec::new();
        pending_resets
            .try_reserve_exact(pending_reset_limit)
            .map_err(HandlerBuildError::ResetStorage)?;
        Ok(Self {
            local_port,
            connections,
            connection_limit,
            pending_resets,
            pending_reset_limit,
            next_connection: 0,
            last_timestamp: 0,
        })
    }

    /// Receives a raw TCP segment from `remote_ipv4_address`.
    ///
    /// `initial_sequence_number` is consumed only when this is a new pure SYN.
    pub fn receive_segment<E, F>(
        &mut self,
        remote_ipv4_address: Ipv4Addr,
        bytes: &[u8],
        initial_sequence_number: u32,
        now: u64,
        callback: F,
    ) -> Result<HandlerReceiveEvent, HandlerReceiveError<E>>
    where
        F: FnOnce(&[u8]) -> Result<Vec<u8>, E>,
    {
        let segment = TcpSegment::parse(bytes).map_err(HandlerReceiveError::Segment)?;
        self.validate_timestamp(now)
            .map_err(HandlerReceiveError::TimestampRegression)?;
        if segment.destination_port() != self.local_port {
            return Err(HandlerReceiveError::InvalidPort {
                expected: self.local_port,
                actual: segment.destination_port(),
            });
        }
        self.last_timestamp = now;
        let peer = Peer::new(remote_ipv4_address, segment.source_port());

        if let Some(index) = self.connections.iter().position(|entry| entry.peer == peer) {
            let status = self
                .connections
                .get_mut(index)
                .ok_or(HandlerReceiveError::ConnectionTableInvariant)?
                .endpoint
                .receive_segment(&segment, now, callback)
                .map_err(HandlerReceiveError::Endpoint)?;
            if self
                .connections
                .get(index)
                .is_some_and(|entry| entry.endpoint.is_done())
            {
                self.remove_connection(index);
                return Ok(HandlerReceiveEvent::EndpointDone);
            }
            return Ok(HandlerReceiveEvent::ExistingConnection { status });
        }

        if segment.flags() == TcpFlags::SYNCHRONIZE {
            let endpoint = match Endpoint::new(&segment, initial_sequence_number, now) {
                Ok(endpoint) => endpoint,
                Err(_) => return Ok(HandlerReceiveEvent::FailedNewConnection),
            };
            if self.connections.len() < self.connection_limit {
                self.connections.push(ConnectionEntry { peer, endpoint });
                return Ok(HandlerReceiveEvent::NewConnection);
            }

            let mut evict_index = None;
            for (index, entry) in self.connections.iter().enumerate() {
                if entry
                    .endpoint
                    .is_evictable(now)
                    .map_err(HandlerReceiveError::TimestampRegression)?
                {
                    evict_index = Some(index);
                    break;
                }
            }
            if let Some(index) = evict_index {
                let evicted = self
                    .connections
                    .get(index)
                    .ok_or(HandlerReceiveError::ConnectionTableInvariant)?;
                self.enqueue_reset(PendingReset {
                    peer: evicted.peer,
                    config: evicted.endpoint.connection().make_reset_config(),
                });
                self.remove_connection(index);
                self.connections.push(ConnectionEntry { peer, endpoint });
                return Ok(HandlerReceiveEvent::NewConnectionReplacing);
            }

            self.enqueue_reset(PendingReset {
                peer,
                config: ResetConfig::from_segment(&segment),
            });
            return Ok(HandlerReceiveEvent::NewConnectionDropped);
        }

        if !segment.flags().intersects(TcpFlags::RESET) {
            self.enqueue_reset(PendingReset {
                peer,
                config: ResetConfig::from_segment(&segment),
            });
        }
        Ok(HandlerReceiveEvent::UnexpectedSegment)
    }

    /// Produces at most one reset or endpoint segment.
    pub fn write_next_segment(
        &mut self,
        output_payload: &mut [u8],
        mss_reserved: u16,
        now: u64,
    ) -> Result<Option<HandlerOutput>, HandlerWriteError> {
        self.validate_timestamp(now)
            .map_err(HandlerWriteError::TimestampRegression)?;
        self.last_timestamp = now;

        if let Some(pending) = self.pending_resets.pop() {
            let (sequence_number, acknowledgement_number, flags) = pending.config.fields();
            return Ok(Some(HandlerOutput {
                peer: pending.peer,
                local_port: self.local_port,
                segment: OutgoingSegment::new(
                    sequence_number,
                    acknowledgement_number,
                    flags,
                    10_000,
                    None,
                    0,
                ),
                event: HandlerWriteEvent::Nothing,
            }));
        }

        let Some(index) = self.next_output_connection() else {
            return Ok(None);
        };
        let entry = self
            .connections
            .get_mut(index)
            .ok_or(HandlerWriteError::ConnectionTableInvariant)?;
        let peer = entry.peer;
        let Some(segment) = entry
            .endpoint
            .write_next_segment(output_payload, mss_reserved, now)
            .map_err(HandlerWriteError::Connection)?
        else {
            return Ok(None);
        };
        let done = entry.endpoint.is_done();
        self.next_connection = index.saturating_add(1);
        let event = if done {
            self.remove_connection(index);
            HandlerWriteEvent::EndpointDone
        } else {
            if !self.connections.is_empty() {
                self.next_connection %= self.connections.len();
            } else {
                self.next_connection = 0;
            }
            HandlerWriteEvent::Nothing
        };
        Ok(Some(HandlerOutput {
            peer,
            local_port: self.local_port,
            segment,
            event,
        }))
    }

    /// Aggregate immediate-output or earliest-timeout status.
    pub fn next_segment_status(&self) -> NextSegmentStatus {
        if !self.pending_resets.is_empty()
            || self
                .connections
                .iter()
                .any(|entry| entry.endpoint.next_segment_status() == NextSegmentStatus::Available)
        {
            return NextSegmentStatus::Available;
        }
        self.connections
            .iter()
            .filter_map(|entry| match entry.endpoint.next_segment_status() {
                NextSegmentStatus::Timeout(deadline) => Some(deadline),
                NextSegmentStatus::Available | NextSegmentStatus::Nothing => None,
            })
            .min()
            .map_or(NextSegmentStatus::Nothing, NextSegmentStatus::Timeout)
    }

    /// Bound local TCP port.
    pub const fn local_port(&self) -> u16 {
        self.local_port
    }

    /// Current number of live endpoints.
    pub const fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Configured logical connection limit.
    pub const fn connection_limit(&self) -> usize {
        self.connection_limit
    }

    /// Number of reset replies waiting for output.
    pub const fn pending_reset_count(&self) -> usize {
        self.pending_resets.len()
    }

    /// Configured logical pending-reset limit.
    pub const fn pending_reset_limit(&self) -> usize {
        self.pending_reset_limit
    }

    #[cfg(test)]
    pub(crate) fn try_new_for_test(
        local_port: u16,
        connection_limit: usize,
        pending_reset_limit: usize,
    ) -> Result<Self, HandlerBuildError> {
        Self::try_new_with_limits(local_port, connection_limit, pending_reset_limit)
    }

    fn validate_timestamp(&self, now: u64) -> Result<(), TimestampRegression> {
        if now < self.last_timestamp {
            Err(TimestampRegression::new(self.last_timestamp, now))
        } else {
            Ok(())
        }
    }

    fn enqueue_reset(&mut self, reset: PendingReset) {
        if self.pending_resets.len() < self.pending_reset_limit {
            self.pending_resets.push(reset);
        }
    }

    fn remove_connection(&mut self, index: usize) {
        if index < self.connections.len() {
            self.connections.remove(index);
        }
        if self.connections.is_empty() {
            self.next_connection = 0;
        } else if self.next_connection >= self.connections.len() {
            self.next_connection %= self.connections.len();
        }
    }

    fn next_output_connection(&self) -> Option<usize> {
        if self.connections.is_empty() {
            return None;
        }
        let len = self.connections.len();
        let start = self.next_connection % len;
        let mut earliest_timeout = None;
        for offset in 0..len {
            let index = (start + offset) % len;
            let status = self.connections.get(index)?.endpoint.next_segment_status();
            match status {
                NextSegmentStatus::Available => return Some(index),
                NextSegmentStatus::Timeout(deadline) => match earliest_timeout {
                    Some((current, _)) if current <= deadline => {}
                    _ => earliest_timeout = Some((deadline, index)),
                },
                NextSegmentStatus::Nothing => {}
            }
        }
        earliest_timeout.map(|(_, index)| index)
    }
}

/// Handler receive-side state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerReceiveEvent {
    /// An endpoint became done and was removed.
    EndpointDone,
    /// Existing connection accepted or classified a segment.
    ExistingConnection { status: super::ReceiveStatus },
    /// A new endpoint was created.
    NewConnection,
    /// The incoming SYN could not create an endpoint.
    FailedNewConnection,
    /// Capacity was full and no stale endpoint could be evicted.
    NewConnectionDropped,
    /// A stale endpoint was reset and replaced.
    NewConnectionReplacing,
    /// A non-SYN segment did not match an endpoint.
    UnexpectedSegment,
}

/// Handler write-side state change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerWriteEvent {
    /// The endpoint became done and was removed after this segment.
    EndpointDone,
    /// No endpoint lifecycle transition occurred.
    Nothing,
}

/// One handler-produced segment and its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandlerOutput {
    peer: Peer,
    local_port: u16,
    segment: OutgoingSegment,
    event: HandlerWriteEvent,
}

impl HandlerOutput {
    /// Remote destination.
    pub const fn peer(self) -> Peer {
        self.peer
    }

    /// Local source port.
    pub const fn local_port(self) -> u16 {
        self.local_port
    }

    /// TCP segment metadata; payload bytes occupy the caller buffer prefix.
    pub const fn segment(self) -> OutgoingSegment {
        self.segment
    }

    /// Lifecycle event caused by this output.
    pub const fn event(self) -> HandlerWriteEvent {
        self.event
    }
}

/// Failure while allocating fixed handler storage.
#[derive(Debug)]
pub enum HandlerBuildError {
    /// Could not reserve connection slots.
    ConnectionStorage(TryReserveError),
    /// Could not reserve pending-reset slots.
    ResetStorage(TryReserveError),
}

impl fmt::Display for HandlerBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionStorage(source) => {
                write!(
                    formatter,
                    "failed to reserve MMDS TCP connection slots: {source}"
                )
            }
            Self::ResetStorage(source) => {
                write!(
                    formatter,
                    "failed to reserve MMDS TCP reset slots: {source}"
                )
            }
        }
    }
}

impl std::error::Error for HandlerBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConnectionStorage(source) | Self::ResetStorage(source) => Some(source),
        }
    }
}

/// Error while routing an incoming raw segment.
#[derive(Debug)]
pub enum HandlerReceiveError<E> {
    /// Raw TCP segment was malformed.
    Segment(SegmentParseError),
    /// Destination port does not match the handler.
    InvalidPort { expected: u16, actual: u16 },
    /// Endpoint processing failed.
    Endpoint(EndpointReceiveError<E>),
    /// The injected monotonic timestamp moved backwards.
    TimestampRegression(TimestampRegression),
    /// Internal bounded connection indexing was inconsistent.
    ConnectionTableInvariant,
}

impl<E: fmt::Display> fmt::Display for HandlerReceiveError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Segment(source) => source.fmt(formatter),
            Self::InvalidPort { expected, actual } => write!(
                formatter,
                "MMDS TCP destination port {actual} does not match {expected}"
            ),
            Self::Endpoint(source) => source.fmt(formatter),
            Self::TimestampRegression(source) => source.fmt(formatter),
            Self::ConnectionTableInvariant => {
                formatter.write_str("MMDS TCP connection-table invariant failed")
            }
        }
    }
}

impl<E> std::error::Error for HandlerReceiveError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Segment(source) => Some(source),
            Self::Endpoint(source) => Some(source),
            Self::TimestampRegression(source) => Some(source),
            Self::InvalidPort { .. } | Self::ConnectionTableInvariant => None,
        }
    }
}

/// Error while producing handler output.
#[derive(Debug)]
pub enum HandlerWriteError {
    /// Selected connection could not produce output.
    Connection(ConnectionWriteError),
    /// The injected monotonic timestamp moved backwards.
    TimestampRegression(TimestampRegression),
    /// Internal bounded connection indexing was inconsistent.
    ConnectionTableInvariant,
}

impl fmt::Display for HandlerWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(source) => source.fmt(formatter),
            Self::TimestampRegression(source) => source.fmt(formatter),
            Self::ConnectionTableInvariant => {
                formatter.write_str("MMDS TCP connection-table invariant failed")
            }
        }
    }
}

impl std::error::Error for HandlerWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connection(source) => Some(source),
            Self::TimestampRegression(source) => Some(source),
            Self::ConnectionTableInvariant => None,
        }
    }
}
