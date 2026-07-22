// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Passive-open connection state adapted from Firecracker v1.16.0
//! `dumbo/tcp/connection.rs`.

use std::fmt;
use std::num::{NonZeroU16, NonZeroU64};
use std::ops::{BitOr, BitOrAssign};

use super::{
    DEFAULT_MSS, MAX_WINDOW_SIZE, NextSegmentStatus, OutgoingSegment, ResetConfig, TcpFlags,
    TcpSegment, TimestampRegression, sequence_after, sequence_at_or_after,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConnectionStatus(u8);

impl ConnectionStatus {
    const SYN_RECEIVED: Self = Self(1);
    const SYN_ACK_SENT: Self = Self(1 << 1);
    const ESTABLISHED: Self = Self(1 << 2);
    const FIN_SENT: Self = Self(1 << 3);
    const FIN_ACKED: Self = Self(1 << 4);
    const RESET: Self = Self(1 << 5);

    const fn intersects(self, flags: Self) -> bool {
        self.0 & flags.0 != 0
    }

    fn insert(&mut self, flags: Self) {
        self.0 |= flags.0;
    }

    fn remove(&mut self, flags: Self) {
        self.0 &= !flags.0;
    }
}

/// Unusual but non-fatal conditions observed while receiving a segment.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReceiveStatus(u16);

impl ReceiveStatus {
    /// The acknowledgement number was outside the valid sent interval.
    pub const INVALID_ACK: Self = Self(1);
    /// The peer repeated the highest acknowledgement while data remains outstanding.
    pub const DUPLICATE_ACK: Self = Self(1 << 1);
    /// Payload extended beyond the advertised local receive window.
    pub const SEGMENT_BEYOND_RECEIVE_WINDOW: Self = Self(1 << 2);
    /// Payload did not start at the next expected sequence number.
    pub const UNEXPECTED_SEQUENCE: Self = Self(1 << 3);
    /// The peer moved its advertised receive-window edge backwards.
    pub const REMOTE_RECEIVE_WINDOW_EDGE: Self = Self(1 << 4);
    /// The peer sent data after its accepted FIN.
    pub const DATA_BEYOND_FIN: Self = Self(1 << 5);
    /// A valid reset was received.
    pub const RESET_RECEIVED: Self = Self(1 << 6);
    /// A reset was outside the local receive window.
    pub const INVALID_RESET: Self = Self(1 << 7);
    /// The segment was invalid for the connection state.
    pub const INVALID_SEGMENT: Self = Self(1 << 8);
    /// The connection has queued a reset and will stop after sending it.
    pub const CONNECTION_RESETTING: Self = Self(1 << 9);
    /// A FIN did not occupy the next expected sequence number.
    pub const INVALID_FIN: Self = Self(1 << 10);

    /// Empty receive status.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Whether no unusual condition was observed.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether any supplied status is present.
    pub const fn intersects(self, flags: Self) -> bool {
        self.0 & flags.0 != 0
    }
}

impl BitOr for ReceiveStatus {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for ReceiveStatus {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Caller-owned bytes and the sequence number of their first byte.
#[derive(Clone, Copy)]
pub struct PayloadSource<'a> {
    bytes: &'a [u8],
    initial_sequence_number: u32,
}

impl fmt::Debug for PayloadSource<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PayloadSource")
            .field("bytes", &"[REDACTED]")
            .field("len", &self.bytes.len())
            .field("initial_sequence_number", &self.initial_sequence_number)
            .finish()
    }
}

impl<'a> PayloadSource<'a> {
    /// Creates a payload source anchored at `initial_sequence_number`.
    pub const fn new(bytes: &'a [u8], initial_sequence_number: u32) -> Self {
        Self {
            bytes,
            initial_sequence_number,
        }
    }

    /// Complete bytes available for original sends and retransmissions.
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Sequence number associated with the first byte.
    pub const fn initial_sequence_number(self) -> u32 {
        self.initial_sequence_number
    }
}

/// Minimal passive-open TCP connection.
#[derive(Debug, Clone)]
pub struct Connection {
    acknowledgement_to_send: u32,
    highest_acknowledgement_received: u32,
    first_not_sent: u32,
    local_receive_window_edge: u32,
    remote_receive_window_edge: u32,
    retransmission_started_at: u64,
    retransmission_period: u64,
    retransmission_count: u16,
    retransmission_limit: u16,
    fin_received: Option<u32>,
    send_fin: Option<u32>,
    send_reset: Option<ResetConfig>,
    maximum_segment_size: u16,
    pending_acknowledgement: bool,
    duplicate_acknowledgement: bool,
    status: ConnectionStatus,
    last_timestamp: u64,
}

impl Connection {
    /// Creates a connection in response to a pure SYN segment.
    pub fn passive_open(
        segment: &TcpSegment<'_>,
        local_receive_window_size: u32,
        retransmission_period: NonZeroU64,
        retransmission_limit: NonZeroU16,
        initial_sequence_number: u32,
        now: u64,
    ) -> Result<Self, PassiveOpenError> {
        if segment.flags() != TcpFlags::SYNCHRONIZE || !segment.payload().is_empty() {
            return Err(PassiveOpenError::InvalidSyn);
        }
        if local_receive_window_size > MAX_WINDOW_SIZE {
            return Err(PassiveOpenError::ReceiveWindowTooLarge {
                size: local_receive_window_size,
                limit: MAX_WINDOW_SIZE,
            });
        }

        let acknowledgement_to_send = segment.sequence_number().wrapping_add(1);
        let first_not_sent = initial_sequence_number.wrapping_add(1);
        Ok(Self {
            acknowledgement_to_send,
            highest_acknowledgement_received: initial_sequence_number,
            first_not_sent,
            local_receive_window_edge: acknowledgement_to_send
                .wrapping_add(local_receive_window_size),
            remote_receive_window_edge: first_not_sent
                .wrapping_add(u32::from(segment.window_size())),
            retransmission_started_at: now,
            retransmission_period: retransmission_period.get(),
            retransmission_count: 0,
            retransmission_limit: retransmission_limit.get(),
            fin_received: None,
            send_fin: None,
            send_reset: None,
            maximum_segment_size: segment.maximum_segment_size().unwrap_or(DEFAULT_MSS),
            pending_acknowledgement: false,
            duplicate_acknowledgement: false,
            status: ConnectionStatus::SYN_RECEIVED,
            last_timestamp: now,
        })
    }

    /// Closes the local sending half after all currently known payload bytes.
    pub fn close(&mut self) {
        if self.send_fin.is_none() {
            self.send_fin = Some(self.first_not_sent);
        }
    }

    /// Queues a reset unless one is already pending.
    pub fn reset(&mut self) {
        if self.send_reset.is_none() {
            self.send_reset = Some(self.make_reset_config());
        }
    }

    /// Reset shape appropriate for the current state.
    pub fn make_reset_config(&self) -> ResetConfig {
        if self.is_established() {
            ResetConfig::Sequence(self.first_not_sent)
        } else {
            ResetConfig::Acknowledgement(self.acknowledgement_to_send)
        }
    }

    /// Whether the handshake is complete.
    pub const fn is_established(&self) -> bool {
        self.status.intersects(ConnectionStatus::ESTABLISHED)
    }

    /// Whether the peer sent an accepted FIN.
    pub const fn fin_received(&self) -> bool {
        self.fin_received.is_some()
    }

    /// Whether this connection can be removed.
    pub const fn is_done(&self) -> bool {
        self.status.intersects(ConnectionStatus::RESET)
            || (self.fin_received.is_some() && self.status.intersects(ConnectionStatus::FIN_SENT))
    }

    /// First sequence number that has never been sent.
    pub const fn first_not_sent(&self) -> u32 {
        self.first_not_sent
    }

    /// Highest cumulative acknowledgement received.
    pub const fn highest_acknowledgement_received(&self) -> u32 {
        self.highest_acknowledgement_received
    }

    /// Right edge advertised by the peer.
    pub const fn remote_receive_window_edge(&self) -> u32 {
        self.remote_receive_window_edge
    }

    /// Negotiated maximum segment size.
    pub const fn maximum_segment_size(&self) -> u16 {
        self.maximum_segment_size
    }

    /// Whether a duplicate-ACK retransmission is pending.
    pub const fn duplicate_acknowledgement_pending(&self) -> bool {
        self.duplicate_acknowledgement
    }

    /// Advances the local receive-window edge without exceeding the pinned window.
    pub fn advance_local_receive_window(&mut self, value: u32) {
        let current = self
            .local_receive_window_edge
            .wrapping_sub(self.acknowledgement_to_send);
        if current == 0 {
            self.pending_acknowledgement = true;
        }
        let advance = value.min(MAX_WINDOW_SIZE.saturating_sub(current));
        self.local_receive_window_edge = self.local_receive_window_edge.wrapping_add(advance);
    }

    /// Pending control output or retransmission deadline.
    pub fn control_segment_or_timeout_status(&self) -> NextSegmentStatus {
        if self.syn_ack_pending()
            || self.send_reset.is_some()
            || self.can_send_first_fin()
            || self.pending_acknowledgement
        {
            NextSegmentStatus::Available
        } else if self.highest_acknowledgement_received != self.first_not_sent {
            NextSegmentStatus::Timeout(
                self.retransmission_started_at
                    .saturating_add(self.retransmission_period),
            )
        } else {
            NextSegmentStatus::Nothing
        }
    }

    /// Receives one validated segment into a caller-owned payload buffer.
    pub fn receive_segment(
        &mut self,
        segment: &TcpSegment<'_>,
        receive_buffer: &mut [u8],
        now: u64,
    ) -> Result<(usize, ReceiveStatus), ConnectionReceiveError> {
        self.validate_timestamp(now)?;
        if self.send_reset.is_some() || self.status.intersects(ConnectionStatus::RESET) {
            return Err(ConnectionReceiveError::ConnectionReset);
        }

        let flags = segment.flags();
        if flags.intersects(TcpFlags::RESET) {
            let sequence_number = segment.sequence_number();
            let status = if sequence_at_or_after(sequence_number, self.acknowledgement_to_send)
                && sequence_after(self.local_receive_window_edge, sequence_number)
            {
                self.status.insert(ConnectionStatus::RESET);
                ReceiveStatus::RESET_RECEIVED
            } else {
                ReceiveStatus::INVALID_RESET
            };
            self.last_timestamp = now;
            return Ok((0, status));
        }
        if segment.payload().len() > receive_buffer.len() {
            return Err(ConnectionReceiveError::BufferTooSmall {
                payload_len: segment.payload().len(),
                buffer_len: receive_buffer.len(),
            });
        }

        if !self.syn_ack_sent() {
            if self.is_same_syn(segment) {
                self.last_timestamp = now;
                return Ok((0, ReceiveStatus::empty()));
            }
            return self.reset_for_segment_result(segment, ReceiveStatus::INVALID_SEGMENT, now);
        }
        if !self.is_established() {
            if self.is_same_syn(segment) {
                self.status.remove(ConnectionStatus::SYN_ACK_SENT);
                self.last_timestamp = now;
                return Ok((0, ReceiveStatus::empty()));
            }
            if flags.intersects(TcpFlags::SYNCHRONIZE) {
                return self.reset_for_segment_result(segment, ReceiveStatus::INVALID_SEGMENT, now);
            }
        } else if flags.intersects(TcpFlags::SYNCHRONIZE) {
            return self.reset_for_segment_result(segment, ReceiveStatus::INVALID_SEGMENT, now);
        }

        let mut receive_status = ReceiveStatus::empty();
        if flags.intersects(TcpFlags::ACK) {
            let acknowledgement = segment.acknowledgement_number();
            if sequence_at_or_after(acknowledgement, self.highest_acknowledgement_received)
                && sequence_at_or_after(self.first_not_sent, acknowledgement)
            {
                self.retransmission_count = 0;
                if acknowledgement == self.highest_acknowledgement_received
                    && acknowledgement != self.first_not_sent
                {
                    if !self.is_established() {
                        return self.reset_for_segment_result(
                            segment,
                            ReceiveStatus::INVALID_ACK,
                            now,
                        );
                    }
                    self.duplicate_acknowledgement = true;
                    receive_status |= ReceiveStatus::DUPLICATE_ACK;
                } else {
                    self.highest_acknowledgement_received = acknowledgement;
                    self.retransmission_started_at = now;
                    if !self.is_established() && self.syn_ack_sent() {
                        self.status.insert(ConnectionStatus::ESTABLISHED);
                    }
                    if self.status.intersects(ConnectionStatus::FIN_SENT)
                        && acknowledgement == self.first_not_sent
                    {
                        self.status.insert(ConnectionStatus::FIN_ACKED);
                    }
                }

                if self.is_established() {
                    let edge = acknowledgement.wrapping_add(u32::from(segment.window_size()));
                    if sequence_after(edge, self.remote_receive_window_edge) {
                        self.remote_receive_window_edge = edge;
                    } else if edge != self.remote_receive_window_edge {
                        receive_status |= ReceiveStatus::REMOTE_RECEIVE_WINDOW_EDGE;
                    }
                }
            } else {
                receive_status |= ReceiveStatus::INVALID_ACK;
                if !self.is_established() {
                    return self.reset_for_segment_result(segment, receive_status, now);
                }
            }
        }

        if !self.is_established() {
            self.last_timestamp = now;
            return Ok((0, receive_status));
        }

        let sequence_number = segment.sequence_number();
        let payload_len = u32::try_from(segment.payload().len()).map_err(|_| {
            ConnectionReceiveError::BufferTooSmall {
                payload_len: segment.payload().len(),
                buffer_len: receive_buffer.len(),
            }
        })?;
        let data_end_sequence = sequence_number.wrapping_add(payload_len);
        let mut enqueue_acknowledgement = false;

        if payload_len > 0 {
            if let Some(fin_sequence) = self.fin_received
                && !sequence_at_or_after(fin_sequence, data_end_sequence)
            {
                self.last_timestamp = now;
                return Ok((0, receive_status | ReceiveStatus::DATA_BEYOND_FIN));
            }
            if !sequence_at_or_after(self.local_receive_window_edge, data_end_sequence) {
                self.last_timestamp = now;
                return Ok((
                    0,
                    receive_status | ReceiveStatus::SEGMENT_BEYOND_RECEIVE_WINDOW,
                ));
            }
            if sequence_number != self.acknowledgement_to_send {
                self.pending_acknowledgement = true;
                self.last_timestamp = now;
                return Ok((0, receive_status | ReceiveStatus::UNEXPECTED_SEQUENCE));
            }
            self.acknowledgement_to_send = data_end_sequence;
            enqueue_acknowledgement = true;
        }

        if flags.intersects(TcpFlags::FINISH) && self.fin_received.is_none() {
            let fin_sequence = data_end_sequence;
            if fin_sequence == self.acknowledgement_to_send {
                self.fin_received = Some(fin_sequence);
                self.acknowledgement_to_send = self.acknowledgement_to_send.wrapping_add(1);
                enqueue_acknowledgement = true;
            } else {
                receive_status |= ReceiveStatus::INVALID_FIN;
            }
        }

        if enqueue_acknowledgement {
            self.pending_acknowledgement = true;
        }
        let received = segment.payload().len();
        if received > 0 {
            let buffer_len = receive_buffer.len();
            let destination = receive_buffer.get_mut(..received).ok_or(
                ConnectionReceiveError::BufferTooSmall {
                    payload_len: received,
                    buffer_len,
                },
            )?;
            destination.copy_from_slice(segment.payload());
        }
        self.last_timestamp = now;
        Ok((received, receive_status))
    }

    /// Writes metadata for the next segment and copies any payload into `output_payload`.
    pub fn write_next_segment(
        &mut self,
        output_payload: &mut [u8],
        mss_reserved: u16,
        payload_source: Option<PayloadSource<'_>>,
        now: u64,
    ) -> Result<Option<OutgoingSegment>, ConnectionWriteError> {
        self.validate_timestamp(now)?;
        if self.status.intersects(ConnectionStatus::RESET) {
            return Err(ConnectionWriteError::ConnectionReset);
        }
        let mss_remaining = self.maximum_segment_size.checked_sub(mss_reserved).ok_or(
            ConnectionWriteError::MssRemaining {
                maximum_segment_size: self.maximum_segment_size,
                reserved: mss_reserved,
            },
        )?;

        if let Some(reset) = self.send_reset {
            let segment = self.reset_segment(reset);
            self.status.insert(ConnectionStatus::RESET);
            self.last_timestamp = now;
            return Ok(Some(segment));
        }

        if self.syn_ack_pending() {
            let segment = self.control_segment();
            self.status.insert(ConnectionStatus::SYN_ACK_SENT);
            self.retransmission_started_at = now;
            self.last_timestamp = now;
            return Ok(Some(segment));
        }

        if !self.is_established() {
            if self.retransmission_expired(now) {
                let next_count = self.retransmission_count.saturating_add(1);
                if next_count >= self.retransmission_limit {
                    self.retransmission_count = next_count;
                    let reset = self.make_reset_config();
                    self.send_reset = Some(reset);
                    let segment = self.reset_segment(reset);
                    self.status.insert(ConnectionStatus::RESET);
                    self.last_timestamp = now;
                    return Ok(Some(segment));
                }
                let segment = self.control_segment();
                self.retransmission_count = next_count;
                self.retransmission_started_at = now;
                self.last_timestamp = now;
                return Ok(Some(segment));
            }
            self.last_timestamp = now;
            return Ok(None);
        }

        if let Some(source) = payload_source {
            let source_len = u32::try_from(source.bytes.len()).map_err(|_| {
                ConnectionWriteError::PayloadTooLarge {
                    len: source.bytes.len(),
                    limit: MAX_WINDOW_SIZE,
                }
            })?;
            if source_len > MAX_WINDOW_SIZE {
                return Err(ConnectionWriteError::PayloadTooLarge {
                    len: source.bytes.len(),
                    limit: MAX_WINDOW_SIZE,
                });
            }
            let payload_end = source.initial_sequence_number.wrapping_add(source_len);
            let timeout_retransmission = self.highest_acknowledgement_received
                != self.first_not_sent
                && self.retransmission_expired(now);
            let next_timeout_count =
                timeout_retransmission.then(|| self.retransmission_count.saturating_add(1));

            if next_timeout_count.is_some_and(|count| count >= self.retransmission_limit) {
                self.retransmission_count = next_timeout_count.unwrap_or(self.retransmission_limit);
                let reset = self.make_reset_config();
                self.send_reset = Some(reset);
                let segment = self.reset_segment(reset);
                self.status.insert(ConnectionStatus::RESET);
                self.last_timestamp = now;
                return Ok(Some(segment));
            }

            let sequence_to_send = if timeout_retransmission || self.duplicate_acknowledgement {
                self.highest_acknowledgement_received
            } else {
                self.first_not_sent
            };

            if timeout_retransmission
                && self.send_fin == Some(self.highest_acknowledgement_received)
            {
                let segment = self.control_segment();
                self.retransmission_count = next_timeout_count.unwrap_or(0);
                self.retransmission_started_at = now;
                self.last_timestamp = now;
                return Ok(Some(segment));
            }

            if !sequence_at_or_after(sequence_to_send, source.initial_sequence_number) {
                return Err(ConnectionWriteError::PayloadMissingSequence {
                    sequence_number: sequence_to_send,
                });
            }
            let source_offset = sequence_to_send.wrapping_sub(source.initial_sequence_number);
            if source_offset > source_len {
                return Err(ConnectionWriteError::PayloadMissingSequence {
                    sequence_number: sequence_to_send,
                });
            }

            let actual_end = if sequence_at_or_after(self.remote_receive_window_edge, payload_end) {
                payload_end
            } else {
                self.remote_receive_window_edge
            };
            if let Some(fin_sequence) = self.send_fin
                && sequence_after(actual_end, fin_sequence)
            {
                return Err(ConnectionWriteError::DataAfterFin);
            }

            if sequence_after(actual_end, sequence_to_send) {
                let available = usize::try_from(actual_end.wrapping_sub(sequence_to_send))
                    .unwrap_or(usize::MAX);
                let source_offset = usize::try_from(source_offset).unwrap_or(usize::MAX);
                let source_bytes = source.bytes.get(source_offset..).ok_or(
                    ConnectionWriteError::PayloadMissingSequence {
                        sequence_number: sequence_to_send,
                    },
                )?;
                let send_len = available
                    .min(source_bytes.len())
                    .min(usize::from(mss_remaining))
                    .min(output_payload.len());
                if send_len == 0 {
                    return Err(ConnectionWriteError::PayloadBufferTooSmall);
                }
                let destination = output_payload
                    .get_mut(..send_len)
                    .ok_or(ConnectionWriteError::PayloadBufferTooSmall)?;
                let source_bytes = source_bytes.get(..send_len).ok_or(
                    ConnectionWriteError::PayloadMissingSequence {
                        sequence_number: sequence_to_send,
                    },
                )?;
                destination.copy_from_slice(source_bytes);

                let mut flags = TcpFlags::ACK;
                let mut first_sequence_after =
                    sequence_to_send.wrapping_add(u32::try_from(send_len).unwrap_or(u32::MAX));
                let send_fin = self.send_fin == Some(first_sequence_after);
                if send_fin {
                    flags |= TcpFlags::FINISH;
                    first_sequence_after = first_sequence_after.wrapping_add(1);
                }

                self.pending_acknowledgement = false;
                self.duplicate_acknowledgement = false;
                if send_fin {
                    self.status.insert(ConnectionStatus::FIN_SENT);
                }
                if let Some(count) = next_timeout_count {
                    self.retransmission_count = count;
                }
                if timeout_retransmission
                    || self.first_not_sent == self.highest_acknowledgement_received
                {
                    self.retransmission_started_at = now;
                }
                if sequence_after(first_sequence_after, self.first_not_sent) {
                    self.first_not_sent = first_sequence_after;
                }
                let segment = OutgoingSegment::new(
                    sequence_to_send,
                    self.acknowledgement_to_send,
                    flags,
                    self.local_receive_window(),
                    None,
                    send_len,
                );
                self.last_timestamp = now;
                return Ok(Some(segment));
            }
        }

        let send_first_fin = self.can_send_first_fin();
        if self.pending_acknowledgement || send_first_fin {
            let segment = self.control_segment();
            self.pending_acknowledgement = false;
            if send_first_fin {
                self.first_not_sent = self.first_not_sent.wrapping_add(1);
                self.status.insert(ConnectionStatus::FIN_SENT);
            }
            self.last_timestamp = now;
            return Ok(Some(segment));
        }

        self.last_timestamp = now;
        Ok(None)
    }

    fn validate_timestamp(&self, now: u64) -> Result<(), TimestampRegression> {
        if now < self.last_timestamp {
            Err(TimestampRegression::new(self.last_timestamp, now))
        } else {
            Ok(())
        }
    }

    fn syn_ack_pending(&self) -> bool {
        self.status.intersects(ConnectionStatus::SYN_RECEIVED) && !self.syn_ack_sent()
    }

    fn syn_ack_sent(&self) -> bool {
        self.status.intersects(ConnectionStatus::SYN_ACK_SENT)
    }

    fn is_same_syn(&self, segment: &TcpSegment<'_>) -> bool {
        segment.flags() == TcpFlags::SYNCHRONIZE
            && segment.payload().is_empty()
            && self.acknowledgement_to_send == segment.sequence_number().wrapping_add(1)
            && self.maximum_segment_size == segment.maximum_segment_size().unwrap_or(DEFAULT_MSS)
    }

    fn reset_for_segment_result(
        &mut self,
        segment: &TcpSegment<'_>,
        status: ReceiveStatus,
        now: u64,
    ) -> Result<(usize, ReceiveStatus), ConnectionReceiveError> {
        if self.send_reset.is_none() {
            self.send_reset = Some(ResetConfig::from_segment(segment));
        }
        self.last_timestamp = now;
        Ok((0, status | ReceiveStatus::CONNECTION_RESETTING))
    }

    fn retransmission_expired(&self, now: u64) -> bool {
        now.saturating_sub(self.retransmission_started_at) >= self.retransmission_period
    }

    fn can_send_first_fin(&self) -> bool {
        !self.status.intersects(ConnectionStatus::FIN_SENT)
            && self.send_fin == Some(self.highest_acknowledgement_received)
    }

    fn local_receive_window(&self) -> u16 {
        let window = self
            .local_receive_window_edge
            .wrapping_sub(self.acknowledgement_to_send);
        u16::try_from(window).unwrap_or(u16::MAX)
    }

    fn control_segment(&self) -> OutgoingSegment {
        if let Some(reset) = self.send_reset {
            return self.reset_segment(reset);
        }
        if !self.is_established() {
            return OutgoingSegment::new(
                self.first_not_sent.wrapping_sub(1),
                self.acknowledgement_to_send,
                TcpFlags::SYNCHRONIZE | TcpFlags::ACK,
                self.local_receive_window(),
                Some(self.maximum_segment_size),
                0,
            );
        }

        let mut sequence_number = self.highest_acknowledgement_received;
        let mut flags = TcpFlags::ACK;
        if let Some(fin_sequence) = self.send_fin
            && !self.status.intersects(ConnectionStatus::FIN_ACKED)
            && sequence_at_or_after(sequence_number, fin_sequence)
        {
            sequence_number = fin_sequence;
            flags |= TcpFlags::FINISH;
        }
        OutgoingSegment::new(
            sequence_number,
            self.acknowledgement_to_send,
            flags,
            self.local_receive_window(),
            None,
            0,
        )
    }

    fn reset_segment(&self, reset: ResetConfig) -> OutgoingSegment {
        let (sequence_number, acknowledgement_number, flags) = reset.fields();
        OutgoingSegment::new(
            sequence_number,
            acknowledgement_number,
            flags,
            self.local_receive_window(),
            None,
            0,
        )
    }
}

/// Error while creating a passive-open connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassiveOpenError {
    /// The input was not a payload-free pure SYN.
    InvalidSyn,
    /// The configured local receive window exceeds the supported sequence interval.
    ReceiveWindowTooLarge { size: u32, limit: u32 },
}

impl fmt::Display for PassiveOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSyn => formatter.write_str("incoming segment is not a valid TCP SYN"),
            Self::ReceiveWindowTooLarge { size, limit } => write!(
                formatter,
                "TCP receive window {size} exceeds supported limit {limit}"
            ),
        }
    }
}

impl std::error::Error for PassiveOpenError {}

/// Error while receiving a segment on an existing connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionReceiveError {
    /// Caller-provided receive storage cannot hold the segment payload.
    BufferTooSmall {
        payload_len: usize,
        buffer_len: usize,
    },
    /// The connection is already reset or has a reset queued.
    ConnectionReset,
    /// The injected monotonic timestamp moved backwards.
    TimestampRegression(TimestampRegression),
}

impl fmt::Display for ConnectionReceiveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall {
                payload_len,
                buffer_len,
            } => write!(
                formatter,
                "TCP payload length {payload_len} exceeds receive buffer length {buffer_len}"
            ),
            Self::ConnectionReset => formatter.write_str("TCP connection is reset"),
            Self::TimestampRegression(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for ConnectionReceiveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TimestampRegression(source) => Some(source),
            Self::BufferTooSmall { .. } | Self::ConnectionReset => None,
        }
    }
}

impl From<TimestampRegression> for ConnectionReceiveError {
    fn from(source: TimestampRegression) -> Self {
        Self::TimestampRegression(source)
    }
}

/// Error while producing the next segment for a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionWriteError {
    /// The connection is already reset.
    ConnectionReset,
    /// Payload was supplied after the local FIN boundary.
    DataAfterFin,
    /// Lower-layer reservation exceeds the negotiated MSS.
    MssRemaining {
        maximum_segment_size: u16,
        reserved: u16,
    },
    /// Payload source exceeds the pinned sequence window.
    PayloadTooLarge { len: usize, limit: u32 },
    /// Payload source does not contain the required sequence number.
    PayloadMissingSequence { sequence_number: u32 },
    /// Caller output storage cannot hold even one available payload byte.
    PayloadBufferTooSmall,
    /// The injected monotonic timestamp moved backwards.
    TimestampRegression(TimestampRegression),
}

impl fmt::Display for ConnectionWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionReset => formatter.write_str("TCP connection is reset"),
            Self::DataAfterFin => formatter.write_str("TCP payload extends past the local FIN"),
            Self::MssRemaining {
                maximum_segment_size,
                reserved,
            } => write!(
                formatter,
                "TCP MSS {maximum_segment_size} cannot reserve {reserved} lower-layer bytes"
            ),
            Self::PayloadTooLarge { len, limit } => {
                write!(formatter, "TCP payload length {len} exceeds limit {limit}")
            }
            Self::PayloadMissingSequence { sequence_number } => write!(
                formatter,
                "TCP payload source does not contain sequence number {sequence_number}"
            ),
            Self::PayloadBufferTooSmall => {
                formatter.write_str("TCP output payload buffer is empty")
            }
            Self::TimestampRegression(source) => source.fmt(formatter),
        }
    }
}

impl std::error::Error for ConnectionWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TimestampRegression(source) => Some(source),
            Self::ConnectionReset
            | Self::DataAfterFin
            | Self::MssRemaining { .. }
            | Self::PayloadTooLarge { .. }
            | Self::PayloadMissingSequence { .. }
            | Self::PayloadBufferTooSmall => None,
        }
    }
}

impl From<TimestampRegression> for ConnectionWriteError {
    fn from(source: TimestampRegression) -> Self {
        Self::TimestampRegression(source)
    }
}
