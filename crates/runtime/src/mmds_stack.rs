// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! Per-interface MMDS Ethernet/IPv4/TCP session.
//!
//! Packet classification and serialization are a focused semantic adaptation
//! of Firecracker v1.16.0 `mmds/ns.rs` and `dumbo/pdu` at commit
//! `d83d72b710361a10294480131377b1b00b163af8`. TCP state lives in the
//! source-attributed [`crate::mmds_tcp`] adaptation.

use std::fmt;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use crate::metrics::SharedMmdsMetrics;
use crate::mmds::{
    DEFAULT_MMDS_MAC_ADDRESS, EthernetMacAddress, MMDS_GUEST_TCP_PORT, MmdsGuestRequest,
    MmdsGuestToken, MmdsState, MmdsStateHandle, MmdsStateLockError, MmdsVersion,
};
use crate::mmds_tcp::{
    EndpointReceiveError, HandlerBuildError, HandlerOutput, HandlerReceiveError,
    HandlerReceiveEvent, HandlerWriteError, HandlerWriteEvent, MmdsTcpHandler, NextSegmentStatus,
};
use crate::network::VIRTIO_NET_MAX_BUFFER_SIZE;

const ETHERNET_HEADER_LEN: usize = 14;
const ETHERNET_MAC_ADDRESS_LEN: usize = 6;
const ETHERNET_DESTINATION_OFFSET: usize = 0;
const ETHERNET_SOURCE_OFFSET: usize = 6;
const ETHERNET_ETHERTYPE_OFFSET: usize = 12;
const ETHERNET_ETHERTYPE_ARP: u16 = 0x0806;
const ETHERNET_ETHERTYPE_IPV4: u16 = 0x0800;

const ARP_PACKET_LEN: usize = 28;
const ARP_FRAME_LEN: usize = ETHERNET_HEADER_LEN + ARP_PACKET_LEN;
const ARP_HARDWARE_TYPE_OFFSET: usize = 0;
const ARP_PROTOCOL_TYPE_OFFSET: usize = 2;
const ARP_HARDWARE_ADDRESS_LEN_OFFSET: usize = 4;
const ARP_PROTOCOL_ADDRESS_LEN_OFFSET: usize = 5;
const ARP_OPERATION_OFFSET: usize = 6;
const ARP_SENDER_HARDWARE_ADDRESS_OFFSET: usize = 8;
const ARP_SENDER_PROTOCOL_ADDRESS_OFFSET: usize = 14;
const ARP_TARGET_HARDWARE_ADDRESS_OFFSET: usize = 18;
const ARP_TARGET_PROTOCOL_ADDRESS_OFFSET: usize = 24;
const ARP_HARDWARE_TYPE_ETHERNET: u16 = 1;
const ARP_PROTOCOL_TYPE_IPV4: u16 = ETHERNET_ETHERTYPE_IPV4;
const ARP_HARDWARE_ADDRESS_LEN_ETHERNET: u8 = ETHERNET_MAC_ADDRESS_LEN as u8;
const ARP_PROTOCOL_ADDRESS_LEN_IPV4: u8 = 4;
const ARP_OPERATION_REQUEST: u16 = 1;
const ARP_OPERATION_REPLY: u16 = 2;

const IPV4_MIN_HEADER_LEN: usize = 20;
const IPV4_VERSION: u8 = 4;
const IPV4_VERSION_IHL_OFFSET: usize = 0;
const IPV4_TOTAL_LEN_OFFSET: usize = 2;
const IPV4_IDENTIFICATION_OFFSET: usize = 4;
const IPV4_FLAGS_FRAGMENT_OFFSET: usize = 6;
const IPV4_TTL_OFFSET: usize = 8;
const IPV4_PROTOCOL_OFFSET: usize = 9;
const IPV4_CHECKSUM_OFFSET: usize = 10;
const IPV4_SOURCE_OFFSET: usize = 12;
const IPV4_DESTINATION_OFFSET: usize = 16;
const IPV4_PROTOCOL_TCP: u8 = 6;
const IPV4_DEFAULT_TTL: u8 = 1;

const TCP_MIN_HEADER_LEN: usize = 20;
const TCP_MSS_HEADER_LEN: usize = 24;
const TCP_SOURCE_PORT_OFFSET: usize = 0;
const TCP_DESTINATION_PORT_OFFSET: usize = 2;
const TCP_SEQUENCE_OFFSET: usize = 4;
const TCP_ACKNOWLEDGEMENT_OFFSET: usize = 8;
const TCP_DATA_OFFSET_OFFSET: usize = 12;
const TCP_FLAGS_OFFSET: usize = 13;
const TCP_WINDOW_OFFSET: usize = 14;
const TCP_CHECKSUM_OFFSET: usize = 16;
const TCP_URGENT_POINTER_OFFSET: usize = 18;
const TCP_OPTIONS_OFFSET: usize = 20;
const TCP_MSS_OPTION_KIND: u8 = 2;
const TCP_MSS_OPTION_LEN: u8 = 4;

const MIN_TCP_FRAME_LEN: usize = ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN;
const MAX_TCP_PAYLOAD_LEN: usize = u16::MAX as usize - IPV4_MIN_HEADER_LEN - TCP_MIN_HEADER_LEN;
const SESSION_FRAME_BUFFER_LEN: usize = VIRTIO_NET_MAX_BUFFER_SIZE as usize;

/// A fallible, shared handle to one interface-local MMDS network stack.
#[derive(Clone)]
pub struct MmdsNetworkStackHandle {
    state: Arc<Mutex<MmdsNetworkStackState>>,
    mmds_ipv4_address: Ipv4Addr,
    epoch: Instant,
}

impl fmt::Debug for MmdsNetworkStackHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsNetworkStackHandle")
            .field("state", &"<configured>")
            .field("epoch", &"<monotonic>")
            .finish()
    }
}

impl MmdsNetworkStackHandle {
    /// Builds one production session with an OS-seeded initial sequence stream.
    pub fn try_new(
        mmds_state: MmdsStateHandle,
        mmds_ipv4_address: Ipv4Addr,
        metrics: SharedMmdsMetrics,
    ) -> Result<Self, MmdsNetworkStackBuildError> {
        let mut seed = [0_u8; size_of::<u32>()];
        getrandom::fill(&mut seed).map_err(MmdsNetworkStackBuildError::Random)?;
        Self::try_new_with_seed_and_epoch(
            mmds_state,
            mmds_ipv4_address,
            metrics,
            u32::from_ne_bytes(seed),
            Instant::now(),
        )
    }

    #[doc(hidden)]
    pub fn try_new_for_test(
        mmds_state: MmdsStateHandle,
        mmds_ipv4_address: Ipv4Addr,
        metrics: SharedMmdsMetrics,
        initial_sequence_seed: u32,
    ) -> Result<Self, MmdsNetworkStackBuildError> {
        Self::try_new_with_seed_and_epoch(
            mmds_state,
            mmds_ipv4_address,
            metrics,
            initial_sequence_seed,
            Instant::now(),
        )
    }

    fn try_new_with_seed_and_epoch(
        mmds_state: MmdsStateHandle,
        mmds_ipv4_address: Ipv4Addr,
        metrics: SharedMmdsMetrics,
        initial_sequence_seed: u32,
        epoch: Instant,
    ) -> Result<Self, MmdsNetworkStackBuildError> {
        let mut output_frame = Vec::new();
        output_frame
            .try_reserve_exact(SESSION_FRAME_BUFFER_LEN)
            .map_err(|source| MmdsNetworkStackBuildError::FrameStorage {
                len: SESSION_FRAME_BUFFER_LEN,
                source,
            })?;
        output_frame.resize(SESSION_FRAME_BUFFER_LEN, 0);

        Ok(Self {
            state: Arc::new(Mutex::new(MmdsNetworkStackState {
                mmds_state,
                mmds_ipv4_address,
                remote_mac_address: DEFAULT_MMDS_MAC_ADDRESS,
                pending_arp_reply_destination: None,
                tcp_handler: MmdsTcpHandler::try_new(MMDS_GUEST_TCP_PORT)
                    .map_err(MmdsNetworkStackBuildError::TcpHandler)?,
                initial_sequence_state: initial_sequence_seed,
                output_frame,
                output_frame_len: None,
                metrics,
            })),
            mmds_ipv4_address,
            epoch,
        })
    }

    /// Returns whether the Ethernet frame is speculatively addressed to MMDS.
    pub fn is_mmds_frame(&self, frame: &[u8]) -> bool {
        is_mmds_frame(frame, self.mmds_ipv4_address)
    }

    /// Routes a target frame using the production monotonic clock.
    pub fn detour_frame(&self, frame: &[u8]) -> Result<bool, MmdsNetworkStackError> {
        self.detour_frame_at(frame, self.now_ticks())
    }

    /// Routes a target frame at an injected monotonic tick.
    #[doc(hidden)]
    pub fn detour_frame_at(&self, frame: &[u8], now: u64) -> Result<bool, MmdsNetworkStackError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| MmdsNetworkStackError::Poisoned)?;
        if !is_mmds_frame(frame, state.mmds_ipv4_address) {
            return Ok(false);
        }
        state.metrics.record_rx_accepted();
        state.detour_target_frame(frame, now)?;
        Ok(true)
    }

    /// Copies the retained or newly generated frame into `destination`.
    pub fn copy_next_frame_into(
        &self,
        destination: &mut [u8],
    ) -> Result<Option<usize>, MmdsNetworkStackError> {
        self.copy_next_frame_into_at(destination, self.now_ticks())
    }

    /// Copies the retained or newly generated frame at an injected tick.
    #[doc(hidden)]
    pub fn copy_next_frame_into_at(
        &self,
        destination: &mut [u8],
        now: u64,
    ) -> Result<Option<usize>, MmdsNetworkStackError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| MmdsNetworkStackError::Poisoned)?;
        let len = match state.output_frame_len {
            Some(len) => len,
            None => match state.write_next_frame(now)? {
                Some(len) => {
                    state.output_frame_len = Some(len);
                    len
                }
                None => return Ok(None),
            },
        };
        let destination_len = destination.len();
        let target = destination.get_mut(..len).ok_or_else(|| {
            state.metrics.record_tx_error();
            MmdsNetworkStackError::OutputBufferTooSmall {
                required: len,
                actual: destination_len,
            }
        })?;
        let source = state.output_frame.get(..len).ok_or_else(|| {
            state.metrics.record_tx_error();
            MmdsNetworkStackError::FrameLayout
        })?;
        target.copy_from_slice(source);
        Ok(Some(len))
    }

    /// Commits delivery of the one retained frame.
    pub fn consume_frame(&self, expected_len: usize) -> Result<(), MmdsNetworkStackError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| MmdsNetworkStackError::Poisoned)?;
        match state.output_frame_len {
            Some(actual) if actual == expected_len => {
                state.output_frame_len = None;
                state.metrics.record_tx_frame(actual);
                Ok(())
            }
            actual => {
                state.metrics.record_tx_error();
                Err(MmdsNetworkStackError::RetainedFrameMismatch {
                    expected: expected_len,
                    actual,
                })
            }
        }
    }

    /// Cheap, non-consuming readiness for the current monotonic instant.
    pub fn has_ready_frame(&self) -> bool {
        self.has_ready_frame_at(self.now_ticks())
    }

    /// Readiness at an injected tick.
    #[doc(hidden)]
    pub fn has_ready_frame_at(&self, now: u64) -> bool {
        match self.state.try_lock() {
            Ok(state) => state.has_ready_frame(now),
            Err(TryLockError::Poisoned(_)) => true,
            Err(TryLockError::WouldBlock) => true,
        }
    }

    /// Future protocol retry duration, excluding immediate or retained output.
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after_at(self.now_ticks())
    }

    /// Future protocol retry duration at an injected tick.
    #[doc(hidden)]
    pub fn retry_after_at(&self, now: u64) -> Option<Duration> {
        let state = self.state.try_lock().ok()?;
        state.retry_after_at(now)
    }

    #[doc(hidden)]
    pub fn shares_state_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    #[doc(hidden)]
    pub fn connection_count(&self) -> Result<usize, MmdsNetworkStackError> {
        self.state
            .lock()
            .map(|state| state.tcp_handler.connection_count())
            .map_err(|_| MmdsNetworkStackError::Poisoned)
    }

    #[doc(hidden)]
    pub fn pending_reset_count(&self) -> Result<usize, MmdsNetworkStackError> {
        self.state
            .lock()
            .map(|state| state.tcp_handler.pending_reset_count())
            .map_err(|_| MmdsNetworkStackError::Poisoned)
    }

    #[doc(hidden)]
    pub fn retained_frame_len(&self) -> Result<Option<usize>, MmdsNetworkStackError> {
        self.state
            .lock()
            .map(|state| state.output_frame_len)
            .map_err(|_| MmdsNetworkStackError::Poisoned)
    }

    #[doc(hidden)]
    pub fn with_lock_for_test<R>(
        &self,
        operation: impl FnOnce() -> R,
    ) -> Result<R, MmdsNetworkStackError> {
        let _guard = self
            .state
            .lock()
            .map_err(|_| MmdsNetworkStackError::Poisoned)?;
        Ok(operation())
    }

    #[doc(hidden)]
    pub fn poison_for_test(&self) {
        let state = Arc::clone(&self.state);
        let _ = std::thread::spawn(move || {
            let _guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::panic::resume_unwind(Box::new("poison test MMDS network stack"));
        })
        .join();
    }

    fn now_ticks(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

struct MmdsNetworkStackState {
    mmds_state: MmdsStateHandle,
    mmds_ipv4_address: Ipv4Addr,
    remote_mac_address: EthernetMacAddress,
    pending_arp_reply_destination: Option<Ipv4Addr>,
    tcp_handler: MmdsTcpHandler,
    initial_sequence_state: u32,
    output_frame: Vec<u8>,
    output_frame_len: Option<usize>,
    metrics: SharedMmdsMetrics,
}

impl fmt::Debug for MmdsNetworkStackState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsNetworkStackState")
            .field("mmds_state", &"<configured>")
            .field("mmds_ipv4_address", &self.mmds_ipv4_address)
            .field("remote_mac_address", &self.remote_mac_address)
            .field(
                "pending_arp_reply_destination",
                &self.pending_arp_reply_destination,
            )
            .field("tcp_handler", &self.tcp_handler)
            .field("initial_sequence_state", &"<redacted>")
            .field("output_frame", &"[REDACTED]")
            .field("output_frame_len", &self.output_frame_len)
            .field("metrics", &"<configured>")
            .finish()
    }
}

impl MmdsNetworkStackState {
    fn detour_target_frame(&mut self, frame: &[u8], now: u64) -> Result<(), MmdsNetworkStackError> {
        match read_u16(frame, ETHERNET_ETHERTYPE_OFFSET) {
            Some(ETHERNET_ETHERTYPE_ARP) => {
                self.detour_arp(frame);
                Ok(())
            }
            Some(ETHERNET_ETHERTYPE_IPV4) => self.detour_ipv4(frame, now),
            Some(_) | None => Ok(()),
        }
    }

    fn detour_arp(&mut self, frame: &[u8]) {
        let Some(arp) = frame.get(ETHERNET_HEADER_LEN..) else {
            return;
        };
        if arp.len() != ARP_PACKET_LEN
            || read_u16(arp, ARP_HARDWARE_TYPE_OFFSET) != Some(ARP_HARDWARE_TYPE_ETHERNET)
            || read_u16(arp, ARP_PROTOCOL_TYPE_OFFSET) != Some(ARP_PROTOCOL_TYPE_IPV4)
            || arp.get(ARP_HARDWARE_ADDRESS_LEN_OFFSET).copied()
                != Some(ARP_HARDWARE_ADDRESS_LEN_ETHERNET)
            || arp.get(ARP_PROTOCOL_ADDRESS_LEN_OFFSET).copied()
                != Some(ARP_PROTOCOL_ADDRESS_LEN_IPV4)
            || read_u16(arp, ARP_OPERATION_OFFSET) != Some(ARP_OPERATION_REQUEST)
        {
            return;
        }
        let Some(remote_mac) = read_mac(arp, ARP_SENDER_HARDWARE_ADDRESS_OFFSET) else {
            return;
        };
        let Some(remote_ipv4) = read_ipv4(arp, ARP_SENDER_PROTOCOL_ADDRESS_OFFSET) else {
            return;
        };
        self.remote_mac_address = remote_mac;
        self.pending_arp_reply_destination = Some(remote_ipv4);
    }

    fn detour_ipv4(&mut self, frame: &[u8], now: u64) -> Result<(), MmdsNetworkStackError> {
        let Some(ipv4) = parse_exact_ipv4(frame) else {
            return Ok(());
        };
        if ipv4.protocol != IPV4_PROTOCOL_TCP {
            self.metrics.record_rx_accepted_unusual();
            return Ok(());
        }
        if let Some(remote_mac) = read_mac(frame, ETHERNET_SOURCE_OFFSET) {
            self.remote_mac_address = remote_mac;
        }

        let initial_sequence_number = self.next_initial_sequence_number();
        let metrics = self.metrics.clone();
        let mmds_state = self.mmds_state.clone();
        let result = self.tcp_handler.receive_segment(
            ipv4.source,
            ipv4.payload,
            initial_sequence_number,
            now,
            move |request| {
                mmds_state.with_mut(|state| {
                    record_mmds_guest_http_request_metrics(&metrics, state, request);
                    state.guest_http_response_bytes(request)
                })
            },
        );

        match result {
            Ok(event) => {
                self.metrics.record_rx_count();
                self.record_receive_event(event);
                Ok(())
            }
            Err(HandlerReceiveError::Endpoint(EndpointReceiveError::Callback(source))) => {
                self.metrics.record_rx_accepted_error();
                Err(MmdsNetworkStackError::MmdsState(source))
            }
            Err(HandlerReceiveError::Endpoint(_)) => {
                self.metrics.record_rx_accepted_error();
                self.metrics.record_rx_count();
                Ok(())
            }
            Err(
                source @ (HandlerReceiveError::TimestampRegression(_)
                | HandlerReceiveError::ConnectionTableInvariant),
            ) => {
                self.metrics.record_rx_accepted_error();
                Err(MmdsNetworkStackError::TcpReceive(source))
            }
            Err(HandlerReceiveError::Segment(_) | HandlerReceiveError::InvalidPort { .. }) => {
                self.metrics.record_rx_accepted_error();
                Ok(())
            }
        }
    }

    fn record_receive_event(&self, event: HandlerReceiveEvent) {
        match event {
            HandlerReceiveEvent::EndpointDone { status } => {
                if !status.is_empty() {
                    self.metrics.record_rx_accepted_unusual();
                }
                self.metrics.record_connection_destroyed();
            }
            HandlerReceiveEvent::ExistingConnection { status } => {
                if !status.is_empty() {
                    self.metrics.record_rx_accepted_unusual();
                }
            }
            HandlerReceiveEvent::NewConnection => self.metrics.record_connection_created(),
            HandlerReceiveEvent::NewConnectionReplacing => {
                self.metrics.record_connection_created();
                self.metrics.record_connection_destroyed();
            }
            HandlerReceiveEvent::FailedNewConnection
            | HandlerReceiveEvent::NewConnectionDropped
            | HandlerReceiveEvent::UnexpectedSegment => {}
        }
    }

    fn write_next_frame(&mut self, now: u64) -> Result<Option<usize>, MmdsNetworkStackError> {
        let result = if let Some(remote_ipv4) = self.pending_arp_reply_destination {
            write_arp_reply(
                &mut self.output_frame,
                self.remote_mac_address,
                remote_ipv4,
                self.mmds_ipv4_address,
            )
            .map(Some)
        } else {
            let write_tcp = match self.tcp_handler.next_segment_status() {
                NextSegmentStatus::Available => true,
                NextSegmentStatus::Timeout(deadline) => now >= deadline,
                NextSegmentStatus::Nothing => false,
            };
            if !write_tcp {
                return Ok(None);
            }
            self.write_next_tcp_frame(now)
        };

        match result {
            Ok(Some(len)) => {
                self.pending_arp_reply_destination = None;
                Ok(Some(len))
            }
            Ok(None) => Ok(None),
            Err(source) => {
                self.metrics.record_tx_error();
                Err(source)
            }
        }
    }

    fn write_next_tcp_frame(&mut self, now: u64) -> Result<Option<usize>, MmdsNetworkStackError> {
        if self.output_frame.len() < TCP_MSS_HEADER_LEN + ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN
        {
            return Err(MmdsNetworkStackError::FrameLayout);
        }
        let payload_start = MIN_TCP_FRAME_LEN;
        let payload_end = payload_start
            .checked_add(MAX_TCP_PAYLOAD_LEN)
            .ok_or(MmdsNetworkStackError::FrameLayout)?
            .min(self.output_frame.len());
        let output = self
            .tcp_handler
            .write_next_segment(
                self.output_frame
                    .get_mut(payload_start..payload_end)
                    .ok_or(MmdsNetworkStackError::FrameLayout)?,
                0,
                now,
            )
            .map_err(MmdsNetworkStackError::TcpWrite)?;
        let Some(output) = output else {
            return Ok(None);
        };
        let len = write_tcp_frame(
            &mut self.output_frame,
            self.remote_mac_address,
            self.mmds_ipv4_address,
            output,
        )?;
        if output.event() == HandlerWriteEvent::EndpointDone {
            self.metrics.record_connection_destroyed();
        }
        Ok(Some(len))
    }

    fn has_ready_frame(&self, now: u64) -> bool {
        self.output_frame_len.is_some()
            || self.pending_arp_reply_destination.is_some()
            || match self.tcp_handler.next_segment_status() {
                NextSegmentStatus::Available => true,
                NextSegmentStatus::Timeout(deadline) => now >= deadline,
                NextSegmentStatus::Nothing => false,
            }
    }

    fn retry_after_at(&self, now: u64) -> Option<Duration> {
        if self.output_frame_len.is_some() || self.pending_arp_reply_destination.is_some() {
            return None;
        }
        match self.tcp_handler.next_segment_status() {
            NextSegmentStatus::Timeout(deadline) if deadline > now => {
                Some(Duration::from_nanos(deadline - now))
            }
            NextSegmentStatus::Available
            | NextSegmentStatus::Nothing
            | NextSegmentStatus::Timeout(_) => None,
        }
    }

    fn next_initial_sequence_number(&mut self) -> u32 {
        let mut value = self.initial_sequence_state;
        if value == 0 {
            value = 0x6d2b_79f5;
        }
        value ^= value << 13;
        value ^= value >> 17;
        value ^= value << 5;
        self.initial_sequence_state = value;
        value
    }
}

struct ParsedIpv4<'a> {
    source: Ipv4Addr,
    protocol: u8,
    payload: &'a [u8],
}

fn is_mmds_frame(frame: &[u8], mmds_ipv4_address: Ipv4Addr) -> bool {
    if frame.len() < ETHERNET_HEADER_LEN {
        return false;
    }
    match read_u16(frame, ETHERNET_ETHERTYPE_OFFSET) {
        Some(ETHERNET_ETHERTYPE_ARP) => {
            frame
                .get(ETHERNET_HEADER_LEN..)
                .and_then(|arp| read_ipv4(arp, ARP_TARGET_PROTOCOL_ADDRESS_OFFSET))
                == Some(mmds_ipv4_address)
        }
        Some(ETHERNET_ETHERTYPE_IPV4) => {
            frame
                .get(ETHERNET_HEADER_LEN..)
                .and_then(|ipv4| read_ipv4(ipv4, IPV4_DESTINATION_OFFSET))
                == Some(mmds_ipv4_address)
        }
        Some(_) | None => false,
    }
}

fn parse_exact_ipv4(frame: &[u8]) -> Option<ParsedIpv4<'_>> {
    let ipv4 = frame.get(ETHERNET_HEADER_LEN..)?;
    if ipv4.len() < IPV4_MIN_HEADER_LEN {
        return None;
    }
    let version_ihl = *ipv4.get(IPV4_VERSION_IHL_OFFSET)?;
    if version_ihl >> 4 != IPV4_VERSION {
        return None;
    }
    let header_len = usize::from(version_ihl & 0x0f).checked_mul(4)?;
    if header_len < IPV4_MIN_HEADER_LEN || header_len > ipv4.len() {
        return None;
    }
    let total_len = usize::from(read_u16(ipv4, IPV4_TOTAL_LEN_OFFSET)?);
    if total_len != ipv4.len() || total_len < header_len {
        return None;
    }
    Some(ParsedIpv4 {
        source: read_ipv4(ipv4, IPV4_SOURCE_OFFSET)?,
        protocol: *ipv4.get(IPV4_PROTOCOL_OFFSET)?,
        payload: ipv4.get(header_len..)?,
    })
}

fn write_arp_reply(
    frame: &mut [u8],
    remote_mac: EthernetMacAddress,
    remote_ipv4: Ipv4Addr,
    mmds_ipv4: Ipv4Addr,
) -> Result<usize, MmdsNetworkStackError> {
    let output = frame
        .get_mut(..ARP_FRAME_LEN)
        .ok_or(MmdsNetworkStackError::FrameLayout)?;
    output
        .get_mut(ETHERNET_DESTINATION_OFFSET..ETHERNET_SOURCE_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&remote_mac.octets());
    output
        .get_mut(ETHERNET_SOURCE_OFFSET..ETHERNET_ETHERTYPE_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&DEFAULT_MMDS_MAC_ADDRESS.octets());
    write_u16(output, ETHERNET_ETHERTYPE_OFFSET, ETHERNET_ETHERTYPE_ARP)?;
    let arp = output
        .get_mut(ETHERNET_HEADER_LEN..)
        .ok_or(MmdsNetworkStackError::FrameLayout)?;
    write_u16(arp, ARP_HARDWARE_TYPE_OFFSET, ARP_HARDWARE_TYPE_ETHERNET)?;
    write_u16(arp, ARP_PROTOCOL_TYPE_OFFSET, ARP_PROTOCOL_TYPE_IPV4)?;
    *arp.get_mut(ARP_HARDWARE_ADDRESS_LEN_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)? = ARP_HARDWARE_ADDRESS_LEN_ETHERNET;
    *arp.get_mut(ARP_PROTOCOL_ADDRESS_LEN_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)? = ARP_PROTOCOL_ADDRESS_LEN_IPV4;
    write_u16(arp, ARP_OPERATION_OFFSET, ARP_OPERATION_REPLY)?;
    arp.get_mut(
        ARP_SENDER_HARDWARE_ADDRESS_OFFSET
            ..ARP_SENDER_HARDWARE_ADDRESS_OFFSET + ETHERNET_MAC_ADDRESS_LEN,
    )
    .ok_or(MmdsNetworkStackError::FrameLayout)?
    .copy_from_slice(&DEFAULT_MMDS_MAC_ADDRESS.octets());
    arp.get_mut(ARP_SENDER_PROTOCOL_ADDRESS_OFFSET..ARP_SENDER_PROTOCOL_ADDRESS_OFFSET + 4)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&mmds_ipv4.octets());
    arp.get_mut(
        ARP_TARGET_HARDWARE_ADDRESS_OFFSET
            ..ARP_TARGET_HARDWARE_ADDRESS_OFFSET + ETHERNET_MAC_ADDRESS_LEN,
    )
    .ok_or(MmdsNetworkStackError::FrameLayout)?
    .copy_from_slice(&remote_mac.octets());
    arp.get_mut(ARP_TARGET_PROTOCOL_ADDRESS_OFFSET..ARP_TARGET_PROTOCOL_ADDRESS_OFFSET + 4)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&remote_ipv4.octets());
    Ok(ARP_FRAME_LEN)
}

fn write_tcp_frame(
    frame: &mut [u8],
    remote_mac: EthernetMacAddress,
    mmds_ipv4: Ipv4Addr,
    output: HandlerOutput,
) -> Result<usize, MmdsNetworkStackError> {
    let segment = output.segment();
    let tcp_header_len = if segment.maximum_segment_size().is_some() {
        TCP_MSS_HEADER_LEN
    } else {
        TCP_MIN_HEADER_LEN
    };
    let tcp_len = tcp_header_len
        .checked_add(segment.payload_len())
        .ok_or(MmdsNetworkStackError::FrameLayout)?;
    let ipv4_len = IPV4_MIN_HEADER_LEN
        .checked_add(tcp_len)
        .ok_or(MmdsNetworkStackError::FrameLayout)?;
    let ipv4_len = u16::try_from(ipv4_len).map_err(|_| MmdsNetworkStackError::FrameLayout)?;
    let frame_len = ETHERNET_HEADER_LEN
        .checked_add(usize::from(ipv4_len))
        .ok_or(MmdsNetworkStackError::FrameLayout)?;
    if frame_len > frame.len() {
        return Err(MmdsNetworkStackError::FrameLayout);
    }

    frame
        .get_mut(ETHERNET_DESTINATION_OFFSET..ETHERNET_SOURCE_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&remote_mac.octets());
    frame
        .get_mut(ETHERNET_SOURCE_OFFSET..ETHERNET_ETHERTYPE_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&DEFAULT_MMDS_MAC_ADDRESS.octets());
    write_u16(frame, ETHERNET_ETHERTYPE_OFFSET, ETHERNET_ETHERTYPE_IPV4)?;

    let ipv4 = frame
        .get_mut(ETHERNET_HEADER_LEN..frame_len)
        .ok_or(MmdsNetworkStackError::FrameLayout)?;
    ipv4.get_mut(..IPV4_MIN_HEADER_LEN)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .fill(0);
    *ipv4
        .get_mut(IPV4_VERSION_IHL_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)? = 0x45;
    write_u16(ipv4, IPV4_TOTAL_LEN_OFFSET, ipv4_len)?;
    write_u16(ipv4, IPV4_IDENTIFICATION_OFFSET, 0)?;
    write_u16(ipv4, IPV4_FLAGS_FRAGMENT_OFFSET, 0)?;
    *ipv4
        .get_mut(IPV4_TTL_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)? = IPV4_DEFAULT_TTL;
    *ipv4
        .get_mut(IPV4_PROTOCOL_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)? = IPV4_PROTOCOL_TCP;
    ipv4.get_mut(IPV4_SOURCE_OFFSET..IPV4_SOURCE_OFFSET + 4)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&mmds_ipv4.octets());
    let remote_ipv4 = output.peer().ipv4_address();
    ipv4.get_mut(IPV4_DESTINATION_OFFSET..IPV4_DESTINATION_OFFSET + 4)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&remote_ipv4.octets());

    let tcp = ipv4
        .get_mut(IPV4_MIN_HEADER_LEN..)
        .ok_or(MmdsNetworkStackError::FrameLayout)?;
    tcp.get_mut(..tcp_header_len)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .fill(0);
    write_u16(tcp, TCP_SOURCE_PORT_OFFSET, output.local_port())?;
    write_u16(tcp, TCP_DESTINATION_PORT_OFFSET, output.peer().port())?;
    write_u32(tcp, TCP_SEQUENCE_OFFSET, segment.sequence_number())?;
    write_u32(
        tcp,
        TCP_ACKNOWLEDGEMENT_OFFSET,
        segment.acknowledgement_number(),
    )?;
    *tcp.get_mut(TCP_DATA_OFFSET_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)? =
        u8::try_from(tcp_header_len / 4).unwrap_or(15) << 4;
    *tcp.get_mut(TCP_FLAGS_OFFSET)
        .ok_or(MmdsNetworkStackError::FrameLayout)? = segment.flags().bits();
    write_u16(tcp, TCP_WINDOW_OFFSET, segment.window_size())?;
    write_u16(tcp, TCP_CHECKSUM_OFFSET, 0)?;
    write_u16(tcp, TCP_URGENT_POINTER_OFFSET, 0)?;
    if let Some(mss) = segment.maximum_segment_size() {
        let options = tcp
            .get_mut(TCP_OPTIONS_OFFSET..TCP_MSS_HEADER_LEN)
            .ok_or(MmdsNetworkStackError::FrameLayout)?;
        options
            .get_mut(..2)
            .ok_or(MmdsNetworkStackError::FrameLayout)?
            .copy_from_slice(&[TCP_MSS_OPTION_KIND, TCP_MSS_OPTION_LEN]);
        options
            .get_mut(2..4)
            .ok_or(MmdsNetworkStackError::FrameLayout)?
            .copy_from_slice(&mss.to_be_bytes());
    }

    let checksum = tcp_checksum(mmds_ipv4, remote_ipv4, tcp)?;
    write_u16(tcp, TCP_CHECKSUM_OFFSET, checksum)?;
    write_u16(ipv4, IPV4_CHECKSUM_OFFSET, 0)?;
    let checksum = internet_checksum(
        ipv4.get(..IPV4_MIN_HEADER_LEN)
            .ok_or(MmdsNetworkStackError::FrameLayout)?,
    );
    write_u16(ipv4, IPV4_CHECKSUM_OFFSET, checksum)?;
    Ok(frame_len)
}

fn tcp_checksum(
    source: Ipv4Addr,
    destination: Ipv4Addr,
    segment: &[u8],
) -> Result<u16, MmdsNetworkStackError> {
    let segment_len =
        u16::try_from(segment.len()).map_err(|_| MmdsNetworkStackError::FrameLayout)?;
    let mut sum = 0_u32;
    sum = checksum_add(sum, &source.octets());
    sum = checksum_add(sum, &destination.octets());
    sum = checksum_add(sum, &[0, IPV4_PROTOCOL_TCP]);
    sum = checksum_add(sum, &segment_len.to_be_bytes());
    sum = checksum_add(sum, segment);
    Ok(checksum_finish(sum))
}

fn internet_checksum(bytes: &[u8]) -> u16 {
    checksum_finish(checksum_add(0, bytes))
}

fn checksum_add(mut sum: u32, bytes: &[u8]) -> u32 {
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        let [first, second] = chunk else {
            continue;
        };
        sum = sum.saturating_add(u32::from(u16::from_be_bytes([*first, *second])));
    }
    if let Some(byte) = chunks.remainder().first() {
        sum = sum.saturating_add(u32::from(*byte) << 8);
    }
    sum
}

fn checksum_finish(mut sum: u32) -> u16 {
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn record_mmds_guest_http_request_metrics(
    metrics: &SharedMmdsMetrics,
    state: &MmdsState,
    request_payload: &[u8],
) {
    let Some(config) = state.config() else {
        return;
    };
    if config.version() != MmdsVersion::V2 {
        return;
    }
    let Ok(MmdsGuestRequest::Get(request)) = MmdsGuestRequest::parse_http(request_payload) else {
        return;
    };

    match request.token() {
        MmdsGuestToken::Missing => metrics.record_rx_no_token(),
        MmdsGuestToken::Header { token_value, .. } if state.is_guest_token_valid(token_value) => {}
        MmdsGuestToken::Header { .. } | MmdsGuestToken::Duplicate => {
            metrics.record_rx_invalid_token();
        }
    }
}

fn read_mac(bytes: &[u8], offset: usize) -> Option<EthernetMacAddress> {
    let value = <[u8; ETHERNET_MAC_ADDRESS_LEN]>::try_from(
        bytes.get(offset..offset + ETHERNET_MAC_ADDRESS_LEN)?,
    )
    .ok()?;
    Some(EthernetMacAddress::from_octets(value))
}

fn read_ipv4(bytes: &[u8], offset: usize) -> Option<Ipv4Addr> {
    Some(Ipv4Addr::from(
        <[u8; 4]>::try_from(bytes.get(offset..offset + 4)?).ok()?,
    ))
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes(
        <[u8; 2]>::try_from(bytes.get(offset..offset + 2)?).ok()?,
    ))
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<(), MmdsNetworkStackError> {
    bytes
        .get_mut(offset..offset + 2)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), MmdsNetworkStackError> {
    bytes
        .get_mut(offset..offset + 4)
        .ok_or(MmdsNetworkStackError::FrameLayout)?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}

/// Failure while constructing fixed per-interface MMDS storage.
#[derive(Debug)]
pub enum MmdsNetworkStackBuildError {
    Random(getrandom::Error),
    TcpHandler(HandlerBuildError),
    FrameStorage {
        len: usize,
        source: std::collections::TryReserveError,
    },
}

impl fmt::Display for MmdsNetworkStackBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Random(source) => write!(
                formatter,
                "failed to seed MMDS TCP sequence state: {source}"
            ),
            Self::TcpHandler(source) => source.fmt(formatter),
            Self::FrameStorage { len, source } => write!(
                formatter,
                "failed to reserve MMDS retained frame buffer of {len} bytes: {source}"
            ),
        }
    }
}

impl std::error::Error for MmdsNetworkStackBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Random(_) => None,
            Self::TcpHandler(source) => Some(source),
            Self::FrameStorage { source, .. } => Some(source),
        }
    }
}

/// Failure while routing or producing an MMDS frame.
#[derive(Debug)]
pub enum MmdsNetworkStackError {
    Poisoned,
    MmdsState(MmdsStateLockError),
    TcpReceive(HandlerReceiveError<MmdsStateLockError>),
    TcpWrite(HandlerWriteError),
    FrameLayout,
    OutputBufferTooSmall {
        required: usize,
        actual: usize,
    },
    RetainedFrameMismatch {
        expected: usize,
        actual: Option<usize>,
    },
}

impl fmt::Display for MmdsNetworkStackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Poisoned => formatter.write_str("MMDS network stack lock is poisoned"),
            Self::MmdsState(source) => source.fmt(formatter),
            Self::TcpReceive(source) => source.fmt(formatter),
            Self::TcpWrite(source) => source.fmt(formatter),
            Self::FrameLayout => formatter.write_str("MMDS output frame layout is invalid"),
            Self::OutputBufferTooSmall { required, actual } => write!(
                formatter,
                "MMDS output frame requires {required} bytes but destination has {actual}"
            ),
            Self::RetainedFrameMismatch { expected, actual } => write!(
                formatter,
                "MMDS retained frame length {actual:?} does not match consumed length {expected}"
            ),
        }
    }
}

impl std::error::Error for MmdsNetworkStackError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MmdsState(source) => Some(source),
            Self::TcpReceive(source) => Some(source),
            Self::TcpWrite(source) => Some(source),
            Self::Poisoned
            | Self::FrameLayout
            | Self::OutputBufferTooSmall { .. }
            | Self::RetainedFrameMismatch { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mmds::MmdsContentInput;
    use crate::mmds_tcp::MMDS_TCP_RETRANSMISSION_PERIOD_TICKS;

    const REMOTE_MAC: [u8; 6] = [0x02, 0, 0, 0, 0, 2];
    const REMOTE_IPV4: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);
    const REMOTE_PORT: u16 = 49_152;
    const SYN: u8 = 0x02;
    const ACK: u8 = 0x10;
    const PSH: u8 = 0x08;

    fn stack(metrics: SharedMmdsMetrics) -> MmdsNetworkStackHandle {
        stack_with_state(MmdsStateHandle::default(), metrics, 0x1234_5678)
    }

    fn stack_with_state(
        mmds_state: MmdsStateHandle,
        metrics: SharedMmdsMetrics,
        initial_sequence_seed: u32,
    ) -> MmdsNetworkStackHandle {
        MmdsNetworkStackHandle::try_new_for_test(
            mmds_state,
            crate::mmds::DEFAULT_MMDS_IPV4_ADDRESS,
            metrics,
            initial_sequence_seed,
        )
        .expect("test MMDS stack should build")
    }

    fn tcp_frame(
        sequence_number: u32,
        acknowledgement_number: u32,
        flags: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let ipv4_len = IPV4_MIN_HEADER_LEN + TCP_MIN_HEADER_LEN + payload.len();
        let mut frame = vec![0_u8; ETHERNET_HEADER_LEN + ipv4_len];
        frame[ETHERNET_DESTINATION_OFFSET..ETHERNET_SOURCE_OFFSET]
            .copy_from_slice(&DEFAULT_MMDS_MAC_ADDRESS.octets());
        frame[ETHERNET_SOURCE_OFFSET..ETHERNET_ETHERTYPE_OFFSET].copy_from_slice(&REMOTE_MAC);
        write_u16(
            &mut frame,
            ETHERNET_ETHERTYPE_OFFSET,
            ETHERNET_ETHERTYPE_IPV4,
        )
        .expect("Ethernet header should fit");

        let ipv4 = &mut frame[ETHERNET_HEADER_LEN..];
        ipv4[IPV4_VERSION_IHL_OFFSET] = 0x45;
        write_u16(
            ipv4,
            IPV4_TOTAL_LEN_OFFSET,
            u16::try_from(ipv4_len).expect("test IPv4 packet should fit"),
        )
        .expect("IPv4 length should fit");
        ipv4[IPV4_TTL_OFFSET] = 64;
        ipv4[IPV4_PROTOCOL_OFFSET] = IPV4_PROTOCOL_TCP;
        ipv4[IPV4_SOURCE_OFFSET..IPV4_SOURCE_OFFSET + 4].copy_from_slice(&REMOTE_IPV4.octets());
        ipv4[IPV4_DESTINATION_OFFSET..IPV4_DESTINATION_OFFSET + 4]
            .copy_from_slice(&crate::mmds::DEFAULT_MMDS_IPV4_ADDRESS.octets());

        let tcp = &mut ipv4[IPV4_MIN_HEADER_LEN..];
        write_u16(tcp, TCP_SOURCE_PORT_OFFSET, REMOTE_PORT).expect("TCP source port should fit");
        write_u16(tcp, TCP_DESTINATION_PORT_OFFSET, MMDS_GUEST_TCP_PORT)
            .expect("TCP destination port should fit");
        write_u32(tcp, TCP_SEQUENCE_OFFSET, sequence_number).expect("TCP sequence should fit");
        write_u32(tcp, TCP_ACKNOWLEDGEMENT_OFFSET, acknowledgement_number)
            .expect("TCP acknowledgement should fit");
        tcp[TCP_DATA_OFFSET_OFFSET] = 5 << 4;
        tcp[TCP_FLAGS_OFFSET] = flags;
        write_u16(tcp, TCP_WINDOW_OFFSET, u16::MAX).expect("TCP window should fit");
        tcp[TCP_MIN_HEADER_LEN..].copy_from_slice(payload);
        frame
    }

    fn arp_request() -> Vec<u8> {
        let mut frame = vec![0_u8; ARP_FRAME_LEN];
        frame[ETHERNET_DESTINATION_OFFSET..ETHERNET_SOURCE_OFFSET].fill(0xff);
        frame[ETHERNET_SOURCE_OFFSET..ETHERNET_ETHERTYPE_OFFSET].copy_from_slice(&REMOTE_MAC);
        write_u16(
            &mut frame,
            ETHERNET_ETHERTYPE_OFFSET,
            ETHERNET_ETHERTYPE_ARP,
        )
        .expect("Ethernet header should fit");
        let arp = &mut frame[ETHERNET_HEADER_LEN..];
        write_u16(arp, ARP_HARDWARE_TYPE_OFFSET, ARP_HARDWARE_TYPE_ETHERNET)
            .expect("ARP hardware type should fit");
        write_u16(arp, ARP_PROTOCOL_TYPE_OFFSET, ARP_PROTOCOL_TYPE_IPV4)
            .expect("ARP protocol type should fit");
        arp[ARP_HARDWARE_ADDRESS_LEN_OFFSET] = ARP_HARDWARE_ADDRESS_LEN_ETHERNET;
        arp[ARP_PROTOCOL_ADDRESS_LEN_OFFSET] = ARP_PROTOCOL_ADDRESS_LEN_IPV4;
        write_u16(arp, ARP_OPERATION_OFFSET, ARP_OPERATION_REQUEST)
            .expect("ARP operation should fit");
        arp[ARP_SENDER_HARDWARE_ADDRESS_OFFSET
            ..ARP_SENDER_HARDWARE_ADDRESS_OFFSET + ETHERNET_MAC_ADDRESS_LEN]
            .copy_from_slice(&REMOTE_MAC);
        arp[ARP_SENDER_PROTOCOL_ADDRESS_OFFSET..ARP_SENDER_PROTOCOL_ADDRESS_OFFSET + 4]
            .copy_from_slice(&REMOTE_IPV4.octets());
        arp[ARP_TARGET_PROTOCOL_ADDRESS_OFFSET..ARP_TARGET_PROTOCOL_ADDRESS_OFFSET + 4]
            .copy_from_slice(&crate::mmds::DEFAULT_MMDS_IPV4_ADDRESS.octets());
        frame
    }

    fn tcp_fields(frame: &[u8]) -> (u32, u32, u8, &[u8]) {
        let ipv4 = &frame[ETHERNET_HEADER_LEN..];
        let tcp = &ipv4[IPV4_MIN_HEADER_LEN..];
        let header_len = usize::from(tcp[TCP_DATA_OFFSET_OFFSET] >> 4) * 4;
        (
            u32::from_be_bytes(
                tcp[TCP_SEQUENCE_OFFSET..TCP_SEQUENCE_OFFSET + 4]
                    .try_into()
                    .expect("TCP sequence should exist"),
            ),
            u32::from_be_bytes(
                tcp[TCP_ACKNOWLEDGEMENT_OFFSET..TCP_ACKNOWLEDGEMENT_OFFSET + 4]
                    .try_into()
                    .expect("TCP acknowledgement should exist"),
            ),
            tcp[TCP_FLAGS_OFFSET],
            &tcp[header_len..],
        )
    }

    fn next_frame(stack: &MmdsNetworkStackHandle, now: u64) -> Vec<u8> {
        let mut frame = vec![0_u8; SESSION_FRAME_BUFFER_LEN];
        let len = stack
            .copy_next_frame_into_at(&mut frame, now)
            .expect("MMDS output should succeed")
            .expect("MMDS output should be ready");
        frame.truncate(len);
        frame
    }

    #[test]
    fn arp_reply_is_retained_until_consumed() {
        let metrics = SharedMmdsMetrics::default();
        let stack = stack(metrics.clone());
        let request = arp_request();

        assert!(stack.is_mmds_frame(&request));
        assert!(
            stack
                .detour_frame_at(&request, 10)
                .expect("ARP request should route")
        );
        assert!(stack.has_ready_frame_at(10));
        let reply = next_frame(&stack, 10);
        assert_eq!(reply.len(), ARP_FRAME_LEN);
        assert_eq!(&reply[..ETHERNET_MAC_ADDRESS_LEN], &REMOTE_MAC);
        assert_eq!(
            read_u16(&reply[ETHERNET_HEADER_LEN..], ARP_OPERATION_OFFSET),
            Some(ARP_OPERATION_REPLY)
        );
        assert_eq!(next_frame(&stack, 11), reply);
        assert_eq!(stack.retry_after_at(11), None);

        stack
            .consume_frame(reply.len())
            .expect("retained ARP reply should consume");
        assert!(!stack.has_ready_frame_at(11));
        assert_eq!(metrics.snapshot().rx_accepted(), 1);
        assert_eq!(metrics.snapshot().tx_count(), 1);
        assert_eq!(metrics.snapshot().tx_frames(), 1);
        assert_eq!(
            metrics.snapshot().tx_bytes(),
            u64::try_from(ARP_FRAME_LEN).expect("ARP length should fit")
        );
    }

    #[test]
    fn retained_frame_survives_a_short_destination_without_double_counting() {
        let metrics = SharedMmdsMetrics::default();
        let stack = stack(metrics.clone());
        stack
            .detour_frame_at(&arp_request(), 10)
            .expect("ARP request should route");

        let error = stack
            .copy_next_frame_into_at(&mut [0_u8; 1], 10)
            .expect_err("short output storage should fail");
        assert!(matches!(
            error,
            MmdsNetworkStackError::OutputBufferTooSmall {
                required: ARP_FRAME_LEN,
                actual: 1
            }
        ));
        assert_eq!(
            stack
                .retained_frame_len()
                .expect("retained frame state should remain readable"),
            Some(ARP_FRAME_LEN)
        );
        assert_eq!(metrics.snapshot().tx_count(), 0);
        assert_eq!(metrics.snapshot().tx_frames(), 0);

        let reply = next_frame(&stack, 11);
        stack
            .consume_frame(reply.len())
            .expect("retained ARP reply should consume");
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.tx_count(), 1);
        assert_eq!(snapshot.tx_errors(), 1);
        assert_eq!(snapshot.tx_frames(), 1);
        assert_eq!(snapshot.tx_bytes(), ARP_FRAME_LEN as u64);
    }

    #[test]
    fn arp_output_precedes_an_already_ready_tcp_segment() {
        let metrics = SharedMmdsMetrics::default();
        let stack = stack(metrics.clone());
        stack
            .detour_frame_at(&tcp_frame(100, 0, SYN, b""), 10)
            .expect("SYN should route");
        stack
            .detour_frame_at(&arp_request(), 11)
            .expect("ARP request should route");

        let arp = next_frame(&stack, 11);
        assert_eq!(
            read_u16(&arp, ETHERNET_ETHERTYPE_OFFSET),
            Some(ETHERNET_ETHERTYPE_ARP)
        );
        stack
            .consume_frame(arp.len())
            .expect("ARP reply should consume");

        let syn_ack = next_frame(&stack, 11);
        assert_eq!(
            read_u16(&syn_ack, ETHERNET_ETHERTYPE_OFFSET),
            Some(ETHERNET_ETHERTYPE_IPV4)
        );
        assert_eq!(tcp_fields(&syn_ack).2, SYN | ACK);
        stack
            .consume_frame(syn_ack.len())
            .expect("SYN-ACK should consume");
        assert_eq!(metrics.snapshot().tx_count(), 2);
        assert_eq!(metrics.snapshot().tx_frames(), 2);
    }

    #[test]
    fn targeted_malformed_and_fragmented_ipv4_frames_are_consumed_independently() {
        const IPV4_MORE_FRAGMENTS: u16 = 0x2000;
        const IPV4_FRAGMENT_OFFSET_ONE: u16 = 1;
        const IPV4_PROTOCOL_UDP: u8 = 17;

        let metrics = SharedMmdsMetrics::default();
        let stack = stack(metrics.clone());

        let mut invalid_version = tcp_frame(100, 0, SYN, b"");
        invalid_version[ETHERNET_HEADER_LEN + IPV4_VERSION_IHL_OFFSET] = 0x65;
        assert!(stack.is_mmds_frame(&invalid_version));
        assert!(
            stack
                .detour_frame_at(&invalid_version, 10)
                .expect("targeted invalid IPv4 should still be consumed")
        );

        let mut non_tcp = tcp_frame(100, 0, SYN, b"");
        non_tcp[ETHERNET_HEADER_LEN + IPV4_PROTOCOL_OFFSET] = IPV4_PROTOCOL_UDP;
        assert!(
            stack
                .detour_frame_at(&non_tcp, 11)
                .expect("targeted UDP should be consumed")
        );

        let mut later_fragment = tcp_frame(100, 0, SYN, b"");
        later_fragment.truncate(ETHERNET_HEADER_LEN + IPV4_MIN_HEADER_LEN + 8);
        let later_fragment_ipv4_len = later_fragment.len() - ETHERNET_HEADER_LEN;
        write_u16(
            &mut later_fragment[ETHERNET_HEADER_LEN..],
            IPV4_TOTAL_LEN_OFFSET,
            u16::try_from(later_fragment_ipv4_len).expect("fragment length should fit"),
        )
        .expect("fragment total length should fit");
        write_u16(
            &mut later_fragment[ETHERNET_HEADER_LEN..],
            IPV4_FLAGS_FRAGMENT_OFFSET,
            IPV4_FRAGMENT_OFFSET_ONE,
        )
        .expect("fragment field should fit");
        assert!(
            stack
                .detour_frame_at(&later_fragment, 12)
                .expect("targeted later fragment should be consumed")
        );

        let mut first_fragment = tcp_frame(200, 0, SYN, b"");
        write_u16(
            &mut first_fragment[ETHERNET_HEADER_LEN..],
            IPV4_FLAGS_FRAGMENT_OFFSET,
            IPV4_MORE_FRAGMENTS,
        )
        .expect("fragment field should fit");
        assert!(
            stack
                .detour_frame_at(&first_fragment, 13)
                .expect("parseable first fragment should route independently")
        );
        assert_eq!(
            stack
                .connection_count()
                .expect("connection count should remain readable"),
            1
        );
        let syn_ack = next_frame(&stack, 13);
        assert_eq!(tcp_fields(&syn_ack).2, SYN | ACK);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.rx_accepted(), 4);
        assert_eq!(snapshot.rx_accepted_unusual(), 1);
        assert_eq!(snapshot.rx_accepted_err(), 1);
        assert_eq!(snapshot.rx_count(), 1);
        assert_eq!(snapshot.connections_created(), 1);
    }

    #[test]
    fn tcp_response_retransmits_on_the_protocol_deadline() {
        let metrics = SharedMmdsMetrics::default();
        let stack = stack(metrics.clone());
        let guest_sequence = 100_u32;

        stack
            .detour_frame_at(&tcp_frame(guest_sequence, 0, SYN, b""), 10)
            .expect("SYN should route");
        let syn_ack = next_frame(&stack, 10);
        let (server_sequence, acknowledgement, flags, payload) = tcp_fields(&syn_ack);
        assert_eq!(acknowledgement, guest_sequence + 1);
        assert_eq!(flags, SYN | ACK);
        assert!(payload.is_empty());
        assert_eq!(
            internet_checksum(&syn_ack[ETHERNET_HEADER_LEN..ETHERNET_HEADER_LEN + 20]),
            0
        );
        let syn_ack_ipv4 = &syn_ack[ETHERNET_HEADER_LEN..];
        assert_eq!(
            tcp_checksum(
                crate::mmds::DEFAULT_MMDS_IPV4_ADDRESS,
                REMOTE_IPV4,
                &syn_ack_ipv4[IPV4_MIN_HEADER_LEN..]
            )
            .expect("TCP checksum should evaluate"),
            0
        );
        stack
            .consume_frame(syn_ack.len())
            .expect("SYN-ACK should consume");
        assert_eq!(
            stack.retry_after_at(10),
            Some(Duration::from_nanos(MMDS_TCP_RETRANSMISSION_PERIOD_TICKS))
        );

        let guest_next = guest_sequence + 1;
        let server_next = server_sequence.wrapping_add(1);
        stack
            .detour_frame_at(&tcp_frame(guest_next, server_next, ACK, b""), 20)
            .expect("handshake ACK should route");
        assert!(!stack.has_ready_frame_at(20));

        let request = b"GET / HTTP/1.1\r\n\r\n";
        stack
            .detour_frame_at(&tcp_frame(guest_next, server_next, ACK | PSH, request), 30)
            .expect("HTTP request should route");
        let response = next_frame(&stack, 30);
        let (response_sequence, response_acknowledgement, response_flags, response_payload) =
            tcp_fields(&response);
        assert_eq!(response_sequence, server_next);
        assert_eq!(
            response_acknowledgement,
            guest_next + u32::try_from(request.len()).expect("request length should fit")
        );
        assert_eq!(response_flags, ACK);
        assert!(response_payload.starts_with(b"HTTP/1.1 400 Bad Request"));
        let response_payload_len = response_payload.len();
        stack
            .consume_frame(response.len())
            .expect("response should consume");
        assert_eq!(
            stack.retry_after_at(30),
            Some(Duration::from_nanos(MMDS_TCP_RETRANSMISSION_PERIOD_TICKS))
        );
        assert_eq!(
            stack
                .copy_next_frame_into_at(
                    &mut vec![0_u8; SESSION_FRAME_BUFFER_LEN],
                    30 + MMDS_TCP_RETRANSMISSION_PERIOD_TICKS - 1
                )
                .expect("early retry probe should succeed"),
            None
        );

        let retransmission = next_frame(&stack, 30 + MMDS_TCP_RETRANSMISSION_PERIOD_TICKS);
        assert_eq!(tcp_fields(&retransmission), tcp_fields(&response));
        assert_eq!(
            stack.retry_after_at(30 + MMDS_TCP_RETRANSMISSION_PERIOD_TICKS),
            None
        );
        stack
            .consume_frame(retransmission.len())
            .expect("retransmission should consume");

        stack
            .detour_frame_at(
                &tcp_frame(
                    guest_next + u32::try_from(request.len()).expect("request length should fit"),
                    response_sequence
                        + u32::try_from(response_payload_len).expect("response length should fit"),
                    ACK,
                    b"",
                ),
                30 + MMDS_TCP_RETRANSMISSION_PERIOD_TICKS + 1,
            )
            .expect("response ACK should route");
        assert_eq!(
            stack.retry_after_at(30 + MMDS_TCP_RETRANSMISSION_PERIOD_TICKS + 1),
            None
        );
        assert_eq!(metrics.snapshot().connections_created(), 1);
        assert_eq!(metrics.snapshot().rx_accepted(), 4);
        assert_eq!(metrics.snapshot().rx_count(), 4);
        assert_eq!(metrics.snapshot().tx_count(), 3);
        assert_eq!(metrics.snapshot().tx_frames(), 3);
    }

    #[test]
    fn long_http_response_is_segmented_and_acknowledged_on_one_interface() {
        let body = "x".repeat(4_096);
        let mmds_state = MmdsStateHandle::new(MmdsState::new(16_384));
        mmds_state
            .with_mut(|state| {
                state.put_data(MmdsContentInput::new(serde_json::json!({
                    "blob": body.clone(),
                })))
            })
            .expect("MMDS state should lock")
            .expect("MMDS data should store");
        let metrics = SharedMmdsMetrics::default();
        let stack = stack_with_state(mmds_state.clone(), metrics.clone(), 0x1234_5678);
        let other_interface = stack_with_state(mmds_state, metrics, 0x8765_4321);
        let guest_sequence = 1_000_u32;

        stack
            .detour_frame_at(&tcp_frame(guest_sequence, 0, SYN, b""), 10)
            .expect("SYN should route");
        let syn_ack = next_frame(&stack, 10);
        let (server_sequence, _, _, _) = tcp_fields(&syn_ack);
        stack
            .consume_frame(syn_ack.len())
            .expect("SYN-ACK should consume");
        let guest_next = guest_sequence + 1;
        let mut server_next = server_sequence.wrapping_add(1);
        stack
            .detour_frame_at(&tcp_frame(guest_next, server_next, ACK, b""), 11)
            .expect("handshake ACK should route");

        let request = b"GET /blob HTTP/1.1\r\nAccept: application/json\r\n\r\n";
        let guest_request_end = guest_next
            .wrapping_add(u32::try_from(request.len()).expect("request length should fit"));
        stack
            .detour_frame_at(&tcp_frame(guest_next, server_next, ACK | PSH, request), 12)
            .expect("HTTP request should route");

        let mut response = Vec::new();
        let mut segment_count = 0_usize;
        let mut now = 12_u64;
        while stack.has_ready_frame_at(now) {
            let frame = next_frame(&stack, now);
            let (sequence, acknowledgement, flags, payload) = tcp_fields(&frame);
            assert_eq!(sequence, server_next);
            assert_eq!(acknowledgement, guest_request_end);
            assert_eq!(flags, ACK);
            assert!(!payload.is_empty());
            let payload_len = u32::try_from(payload.len()).expect("payload length should fit");
            response.extend_from_slice(payload);
            segment_count += 1;
            stack
                .consume_frame(frame.len())
                .expect("response segment should consume");
            server_next = server_next.wrapping_add(payload_len);
            now += 1;
            stack
                .detour_frame_at(&tcp_frame(guest_request_end, server_next, ACK, b""), now)
                .expect("response ACK should route");
            assert!(
                segment_count < 32,
                "response segmentation should stay bounded"
            );
        }

        assert!(segment_count > 1);
        let separator = b"\r\n\r\n";
        let body_offset = response
            .windows(separator.len())
            .position(|window| window == separator)
            .expect("HTTP response should contain a header terminator")
            + separator.len();
        assert_eq!(&response[..12], b"HTTP/1.1 200");
        assert_eq!(
            &response[body_offset..],
            serde_json::to_string(&body)
                .expect("test body should serialize")
                .as_bytes()
        );
        assert_eq!(
            other_interface
                .connection_count()
                .expect("other interface connection count should remain readable"),
            0
        );
        assert!(!other_interface.has_ready_frame_at(now));
        assert_eq!(
            stack
                .connection_count()
                .expect("connection count should remain readable"),
            1
        );
        assert_eq!(
            stack
                .retained_frame_len()
                .expect("retained frame state should remain readable"),
            None
        );
    }

    #[test]
    fn classifier_is_lock_free_and_rejects_non_target_frames() {
        let stack = stack(SharedMmdsMetrics::default());
        let target = tcp_frame(1, 0, SYN, b"");
        let mut non_target = target.clone();
        non_target[ETHERNET_HEADER_LEN + IPV4_DESTINATION_OFFSET
            ..ETHERNET_HEADER_LEN + IPV4_DESTINATION_OFFSET + 4]
            .copy_from_slice(&Ipv4Addr::new(198, 51, 100, 1).octets());

        stack
            .with_lock_for_test(|| {
                assert!(stack.is_mmds_frame(&target));
                assert!(!stack.is_mmds_frame(&non_target));
            })
            .expect("test stack lock should succeed");
    }
}
