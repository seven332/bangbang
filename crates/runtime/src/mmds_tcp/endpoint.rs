// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Bounded MMDS endpoint adapted from Firecracker v1.16.0
//! `dumbo/tcp/endpoint.rs`.

use std::fmt;
use std::num::{NonZeroU16, NonZeroU64};

use super::{
    Connection, ConnectionReceiveError, ConnectionWriteError, MAX_WINDOW_SIZE,
    MMDS_TCP_EVICTION_THRESHOLD_TICKS, MMDS_TCP_RECEIVE_BUFFER_SIZE, MMDS_TCP_RETRANSMISSION_LIMIT,
    MMDS_TCP_RETRANSMISSION_PERIOD_TICKS, NextSegmentStatus, OutgoingSegment, PassiveOpenError,
    PayloadSource, ReceiveStatus, TcpSegment, TimestampRegression, sequence_after,
};

/// One bounded HTTP-over-TCP endpoint.
pub struct Endpoint {
    receive_buffer: [u8; MMDS_TCP_RECEIVE_BUFFER_SIZE],
    receive_buffer_len: usize,
    response: Option<Vec<u8>>,
    response_initial_sequence: u32,
    response_len_limit: u32,
    connection: Connection,
    last_segment_received_at: u64,
    last_timestamp: u64,
    stop_receiving: bool,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Endpoint")
            .field("receive_buffer", &"[REDACTED]")
            .field("receive_buffer_len", &self.receive_buffer_len)
            .field(
                "response",
                &self.response.as_ref().map(|response| response.len()),
            )
            .field("response_initial_sequence", &self.response_initial_sequence)
            .field("response_len_limit", &self.response_len_limit)
            .field("connection", &self.connection)
            .field("last_segment_received_at", &self.last_segment_received_at)
            .field("last_timestamp", &self.last_timestamp)
            .field("stop_receiving", &self.stop_receiving)
            .finish()
    }
}

impl Endpoint {
    /// Creates an endpoint from a pure SYN using the pinned MMDS limits.
    pub fn new(
        segment: &TcpSegment<'_>,
        initial_sequence_number: u32,
        now: u64,
    ) -> Result<Self, PassiveOpenError> {
        let connection = Connection::passive_open(
            segment,
            u32::try_from(MMDS_TCP_RECEIVE_BUFFER_SIZE).unwrap_or(MAX_WINDOW_SIZE),
            NonZeroU64::new(MMDS_TCP_RETRANSMISSION_PERIOD_TICKS)
                .ok_or(PassiveOpenError::InvalidSyn)?,
            NonZeroU16::new(MMDS_TCP_RETRANSMISSION_LIMIT).ok_or(PassiveOpenError::InvalidSyn)?,
            initial_sequence_number,
            now,
        )?;
        let response_initial_sequence = connection.first_not_sent();
        Ok(Self {
            receive_buffer: [0; MMDS_TCP_RECEIVE_BUFFER_SIZE],
            receive_buffer_len: 0,
            response: None,
            response_initial_sequence,
            response_len_limit: MAX_WINDOW_SIZE,
            connection,
            last_segment_received_at: now,
            last_timestamp: now,
            stop_receiving: false,
        })
    }

    /// Receives one segment and invokes `callback` for at most one complete request.
    ///
    /// A callback failure or oversized response queues a reset because the accepted
    /// request cannot otherwise be completed safely.
    pub fn receive_segment<E, F>(
        &mut self,
        segment: &TcpSegment<'_>,
        now: u64,
        callback: F,
    ) -> Result<ReceiveStatus, EndpointReceiveError<E>>
    where
        F: FnOnce(&[u8]) -> Result<Vec<u8>, E>,
    {
        self.validate_timestamp(now)
            .map_err(EndpointReceiveError::TimestampRegression)?;
        if self.stop_receiving {
            self.last_timestamp = now;
            return Ok(ReceiveStatus::empty());
        }

        let receive_slice = self
            .receive_buffer
            .get_mut(self.receive_buffer_len..)
            .ok_or(EndpointReceiveError::ReceiveBufferInvariant)?;
        let (received, status) = self
            .connection
            .receive_segment(segment, receive_slice, now)
            .map_err(EndpointReceiveError::Connection)?;
        self.last_segment_received_at = now;
        self.last_timestamp = now;
        self.receive_buffer_len = self
            .receive_buffer_len
            .checked_add(received)
            .filter(|len| *len <= MMDS_TCP_RECEIVE_BUFFER_SIZE)
            .ok_or(EndpointReceiveError::ReceiveBufferInvariant)?;

        if status.intersects(ReceiveStatus::CONNECTION_RESETTING) {
            self.stop_receiving = true;
            return Ok(status);
        }

        self.clear_acknowledged_response();
        if self.response.is_none()
            && let Some(request_end) = complete_request_end(
                self.receive_buffer
                    .get(..self.receive_buffer_len)
                    .ok_or(EndpointReceiveError::ReceiveBufferInvariant)?,
            )
        {
            let request = self
                .receive_buffer
                .get(..request_end)
                .ok_or(EndpointReceiveError::ReceiveBufferInvariant)?;
            let response = match callback(request) {
                Ok(response) => response,
                Err(source) => {
                    self.connection.reset();
                    self.stop_receiving = true;
                    return Err(EndpointReceiveError::Callback(source));
                }
            };
            if response.len() > usize::try_from(self.response_len_limit).unwrap_or(usize::MAX) {
                let len = response.len();
                self.connection.reset();
                self.stop_receiving = true;
                return Err(EndpointReceiveError::ResponseTooLarge {
                    len,
                    limit: self.response_len_limit,
                });
            }

            self.response_initial_sequence = self.connection.first_not_sent();
            if !response.is_empty() {
                self.response = Some(response);
            }
            self.receive_buffer
                .copy_within(request_end..self.receive_buffer_len, 0);
            self.receive_buffer_len -= request_end;
            self.connection
                .advance_local_receive_window(u32::try_from(request_end).unwrap_or(u32::MAX));
        }

        if self.response.is_none() && self.receive_buffer_len == MMDS_TCP_RECEIVE_BUFFER_SIZE {
            self.connection.reset();
            self.stop_receiving = true;
        }
        if self.connection.fin_received() && self.response.is_none() {
            self.connection.close();
        }
        Ok(status)
    }

    /// Writes the next segment and copies response bytes into `output_payload`.
    pub fn write_next_segment(
        &mut self,
        output_payload: &mut [u8],
        mss_reserved: u16,
        now: u64,
    ) -> Result<Option<OutgoingSegment>, ConnectionWriteError> {
        let payload_source = self.response.as_ref().map(|response| {
            PayloadSource::new(response.as_slice(), self.response_initial_sequence)
        });
        let result =
            self.connection
                .write_next_segment(output_payload, mss_reserved, payload_source, now);
        if result.is_ok() {
            self.last_timestamp = now;
        }
        result
    }

    /// Whether the connection has completed or reset.
    pub const fn is_done(&self) -> bool {
        self.connection.is_done()
    }

    /// Whether the endpoint has been inactive beyond the pinned eviction threshold.
    pub fn is_evictable(&self, now: u64) -> Result<bool, TimestampRegression> {
        self.validate_timestamp(now)?;
        Ok(now.saturating_sub(self.last_segment_received_at) > MMDS_TCP_EVICTION_THRESHOLD_TICKS)
    }

    /// Pending output or the earliest retransmission deadline.
    pub fn next_segment_status(&self) -> NextSegmentStatus {
        let can_send_new_data = self.response.as_ref().is_some_and(|response| {
            let response_len = u32::try_from(response.len()).unwrap_or(MAX_WINDOW_SIZE);
            let response_end = self.response_initial_sequence.wrapping_add(response_len);
            sequence_after(response_end, self.connection.first_not_sent())
                && sequence_after(
                    self.connection.remote_receive_window_edge(),
                    self.connection.first_not_sent(),
                )
        });
        if can_send_new_data || self.connection.duplicate_acknowledgement_pending() {
            NextSegmentStatus::Available
        } else {
            self.connection.control_segment_or_timeout_status()
        }
    }

    /// Underlying portable connection state.
    pub const fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Number of request bytes retained in the fixed receive buffer.
    pub const fn buffered_request_len(&self) -> usize {
        self.receive_buffer_len
    }

    /// Whether one response is awaiting send or acknowledgement.
    pub const fn response_pending(&self) -> bool {
        self.response.is_some()
    }

    #[cfg(test)]
    pub(crate) fn set_response_len_limit_for_test(&mut self, limit: u32) {
        self.response_len_limit = limit;
    }

    fn validate_timestamp(&self, now: u64) -> Result<(), TimestampRegression> {
        if now < self.last_timestamp {
            Err(TimestampRegression::new(self.last_timestamp, now))
        } else {
            Ok(())
        }
    }

    fn clear_acknowledged_response(&mut self) {
        let Some(response) = self.response.as_ref() else {
            return;
        };
        let response_len = u32::try_from(response.len()).unwrap_or(MAX_WINDOW_SIZE);
        if self.connection.highest_acknowledgement_received()
            == self.response_initial_sequence.wrapping_add(response_len)
        {
            self.response = None;
            self.response_initial_sequence = self.connection.first_not_sent();
        }
    }
}

/// Error while accepting data into an endpoint.
#[derive(Debug)]
pub enum EndpointReceiveError<E> {
    /// The connection rejected the segment.
    Connection(ConnectionReceiveError),
    /// The request callback could not generate a response.
    Callback(E),
    /// Generated response exceeds the connection sequence-window bound.
    ResponseTooLarge { len: usize, limit: u32 },
    /// The injected monotonic timestamp moved backwards.
    TimestampRegression(TimestampRegression),
    /// Internal fixed-buffer accounting was inconsistent.
    ReceiveBufferInvariant,
}

impl<E: fmt::Display> fmt::Display for EndpointReceiveError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connection(source) => source.fmt(formatter),
            Self::Callback(source) => write!(formatter, "MMDS request callback failed: {source}"),
            Self::ResponseTooLarge { len, limit } => write!(
                formatter,
                "MMDS TCP response length {len} exceeds sequence-window limit {limit}"
            ),
            Self::TimestampRegression(source) => source.fmt(formatter),
            Self::ReceiveBufferInvariant => {
                formatter.write_str("MMDS TCP receive-buffer invariant failed")
            }
        }
    }
}

impl<E> std::error::Error for EndpointReceiveError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connection(source) => Some(source),
            Self::Callback(source) => Some(source),
            Self::TimestampRegression(source) => Some(source),
            Self::ResponseTooLarge { .. } | Self::ReceiveBufferInvariant => None,
        }
    }
}

fn complete_request_end(bytes: &[u8]) -> Option<usize> {
    for (index, byte) in bytes.iter().copied().enumerate() {
        if byte != b'\n' {
            continue;
        }
        if bytes.get(index + 1) == Some(&b'\n') {
            return index.checked_add(2);
        }
        if bytes.get(index + 1..index + 3) == Some(b"\r\n") {
            return index.checked_add(3);
        }
    }
    None
}
