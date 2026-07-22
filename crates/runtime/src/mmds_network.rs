//! Authority-free MMDS packet detour and virtio-net packet I/O.

use std::collections::{TryReserveError, VecDeque};
use std::fmt;
use std::net::Ipv4Addr;
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, TryLockError};

use crate::memory::GuestMemory;
use crate::metrics::SharedMmdsMetrics;
use crate::mmds::{
    MmdsGuestArpResponseFrameError, MmdsGuestRequest, MmdsGuestTcpPacket,
    MmdsGuestTcpResponseContext, MmdsGuestTcpResponseFrameError, MmdsGuestToken, MmdsState,
    MmdsStateHandle, MmdsStateLockError, MmdsVersion, classify_mmds_guest_arp_request,
    classify_mmds_guest_tcp_packet,
};
use crate::network::{
    GuestMacAddress, VIRTIO_NET_MAX_BUFFER_SIZE, VirtioNetworkBackendMetrics,
    VirtioNetworkRxPacket, VirtioNetworkRxPacketSource, VirtioNetworkRxPacketSourceError,
    VirtioNetworkTxFrame, VirtioNetworkTxPacketCommit, VirtioNetworkTxPacketDisposition,
    VirtioNetworkTxPacketSink, VirtioNetworkTxPacketSinkError, VirtioNetworkTxPacketStage,
};
use crate::network_packet::{VirtioNetworkPacketEnvelope, VirtioNetworkPacketPlan};
use crate::startup::{
    Arm64BootNetworkInterface, Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError,
    Arm64BootNetworkPacketIoProvider,
};

pub const DEFAULT_MMDS_VIRTIO_NETWORK_RX_BUFFER_LEN: usize = VIRTIO_NET_MAX_BUFFER_SIZE as usize;
pub const DEFAULT_MMDS_RESPONSE_QUEUE_CAPACITY: usize = 64;
#[doc(hidden)]
pub const DEFAULT_MMDS_REQUEST_BUFFER_CAPACITY: usize = 30;
#[doc(hidden)]
pub const DEFAULT_MMDS_REQUEST_BUFFER_LEN_LIMIT: usize = 8 * 1024;
const MMDS_HTTP_HEADER_TERMINATOR: &[u8] = b"\r\n\r\n";

#[derive(Debug)]
pub enum MmdsOnlyVirtioNetworkPacketIoBuildError {
    EmptyRxBuffer,
    RxBufferAllocation { len: usize, source: TryReserveError },
}

impl fmt::Display for MmdsOnlyVirtioNetworkPacketIoBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRxBuffer => f.write_str("MMDS-only virtio-net RX buffer must not be empty"),
            Self::RxBufferAllocation { len, source } => {
                write!(
                    f,
                    "failed to reserve MMDS-only virtio-net RX buffer of {len} bytes: {source}"
                )
            }
        }
    }
}

impl std::error::Error for MmdsOnlyVirtioNetworkPacketIoBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RxBufferAllocation { source, .. } => Some(source),
            Self::EmptyRxBuffer => None,
        }
    }
}

#[derive(Debug)]
pub enum MmdsOnlyVirtioNetworkPacketIoProviderBuildError {
    DuplicateInterfaceId { iface_id: String },
}

impl fmt::Display for MmdsOnlyVirtioNetworkPacketIoProviderBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateInterfaceId { iface_id } => {
                write!(f, "duplicate MMDS-only network interface id {iface_id}")
            }
        }
    }
}

impl std::error::Error for MmdsOnlyVirtioNetworkPacketIoProviderBuildError {}

pub struct MmdsPacketDetour {
    mmds_state: MmdsStateHandle,
    mmds_ipv4_address: Ipv4Addr,
    response_queue: MmdsResponseQueue,
    request_buffers: MmdsRequestBuffers,
    metrics: SharedMmdsMetrics,
}

impl fmt::Debug for MmdsPacketDetour {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsPacketDetour")
            .field("mmds_state", &"<configured>")
            .field("mmds_ipv4_address", &self.mmds_ipv4_address)
            .field("response_queue", &self.response_queue)
            .field("request_buffers", &self.request_buffers)
            .field("metrics", &"<configured>")
            .finish()
    }
}

impl MmdsPacketDetour {
    pub fn new(
        mmds_state: MmdsStateHandle,
        mmds_ipv4_address: Ipv4Addr,
        response_queue: MmdsResponseQueue,
        metrics: SharedMmdsMetrics,
    ) -> Self {
        let response_queue = response_queue.with_shared_metrics(metrics.clone());
        Self {
            mmds_state,
            mmds_ipv4_address,
            response_queue,
            request_buffers: MmdsRequestBuffers::default(),
            metrics,
        }
    }

    pub fn detour_packet(&mut self, packet: &[u8]) -> Result<bool, MmdsPacketDetourError> {
        if let Some(arp_request) = classify_mmds_guest_arp_request(packet, self.mmds_ipv4_address) {
            self.record_rx_accepted_result(self.response_queue.push_priority_with(|| {
                arp_request
                    .response_frame()
                    .map_err(MmdsPacketDetourError::ArpResponseFrame)
            }))?;
            return Ok(true);
        }

        let Some(classified) = classify_mmds_guest_tcp_packet(packet, self.mmds_ipv4_address)
        else {
            return Ok(false);
        };
        if classified.is_initial_synchronization_request() {
            self.record_rx_count_result(self.response_queue.push_with(|| {
                classified
                    .syn_ack_response_frame()
                    .map_err(MmdsPacketDetourError::ResponseFrame)
            }))?;
            self.metrics.record_connection_created();
            return Ok(true);
        }
        if classified.acknowledges_initial_synchronization_response() {
            self.record_rx_count();
            return Ok(true);
        }
        if classified.is_empty_fin_close_request() {
            self.record_rx_count_result(self.response_queue.push_pair_with(|| {
                classified
                    .fin_close_response_frames()
                    .map_err(MmdsPacketDetourError::ResponseFrame)
            }))?;
            self.metrics.record_connection_destroyed();
            return Ok(true);
        }
        if classified.is_reset_control() {
            self.record_rx_count();
            self.metrics.record_connection_destroyed();
            return Ok(true);
        }
        if classified.is_unsupported_empty_control_reset_request() {
            self.record_rx_count_result(self.response_queue.push_with(|| {
                classified
                    .reset_response_frame()
                    .map_err(MmdsPacketDetourError::ResponseFrame)
            }))?;
            self.metrics.record_rx_accepted_unusual();
            return Ok(true);
        }
        if classified.payload().is_empty() {
            return Ok(false);
        }
        if classified.has_unsupported_payload_control_flags() {
            return Ok(false);
        }
        if !classified.has_initial_synchronization_acknowledgement() {
            return Ok(false);
        }

        let key = MmdsRequestBufferKey::from_packet(classified);
        let appended_request = self.request_buffers.append_existing(
            key,
            classified.sequence_number(),
            classified.payload(),
        );
        match self.record_rx_accepted_error_result(appended_request)? {
            MmdsRequestBufferAppend::Complete(request) => {
                self.queue_response(
                    request.response_context,
                    request.payload.len(),
                    &request.payload,
                )?;
                return Ok(true);
            }
            MmdsRequestBufferAppend::Buffered => {
                self.record_rx_count();
                return Ok(true);
            }
            MmdsRequestBufferAppend::NotFound => {}
        }

        if mmds_http_request_is_complete(classified.payload()) {
            self.queue_response(
                classified.response_context(),
                classified.payload().len(),
                classified.payload(),
            )?;
            return Ok(true);
        }

        let start_request_result = self.request_buffers.start_request(
            key,
            classified.response_context(),
            classified.sequence_number(),
            classified.payload(),
        );
        self.record_rx_accepted_error_result(start_request_result)?;
        self.record_rx_count();
        Ok(true)
    }

    /// Classifies whether `detour_packet` can perform an MMDS effect without
    /// mutating request buffers, response queues, state, or metrics.
    ///
    /// Batch sinks use this only to preserve effect order by flushing earlier
    /// external frames before the matching packet is committed.
    pub fn would_detour_packet(&self, packet: &[u8]) -> bool {
        if classify_mmds_guest_arp_request(packet, self.mmds_ipv4_address).is_some() {
            return true;
        }
        let Some(classified) = classify_mmds_guest_tcp_packet(packet, self.mmds_ipv4_address)
        else {
            return false;
        };
        if classified.is_initial_synchronization_request()
            || classified.acknowledges_initial_synchronization_response()
            || classified.is_empty_fin_close_request()
            || classified.is_reset_control()
            || classified.is_unsupported_empty_control_reset_request()
        {
            return true;
        }
        !classified.payload().is_empty()
            && !classified.has_unsupported_payload_control_flags()
            && classified.has_initial_synchronization_acknowledgement()
    }

    fn queue_response(
        &self,
        response_context: MmdsGuestTcpResponseContext,
        request_payload_len: usize,
        request_payload: &[u8],
    ) -> Result<(), MmdsPacketDetourError> {
        self.record_rx_count_result(self.response_queue.push_with(|| {
            let response = self
                .mmds_state
                .with_mut(|state| {
                    record_mmds_guest_http_request_metrics(&self.metrics, state, request_payload);
                    state.guest_http_response_bytes(request_payload)
                })
                .map_err(MmdsPacketDetourError::MmdsState)?;
            response_context
                .response_frame(&response, request_payload_len)
                .map_err(MmdsPacketDetourError::ResponseFrame)
        }))
    }

    pub fn response_queue(&self) -> MmdsResponseQueue {
        self.response_queue.clone()
    }

    fn record_rx_count(&self) {
        self.metrics.record_rx_accepted();
        self.metrics.record_rx_count();
    }

    fn record_rx_count_result(
        &self,
        result: Result<(), MmdsPacketDetourError>,
    ) -> Result<(), MmdsPacketDetourError> {
        match result {
            Ok(()) => {
                self.record_rx_count();
                Ok(())
            }
            Err(err) => {
                self.metrics.record_rx_accepted_error();
                Err(err)
            }
        }
    }

    fn record_rx_accepted_result(
        &self,
        result: Result<(), MmdsPacketDetourError>,
    ) -> Result<(), MmdsPacketDetourError> {
        match result {
            Ok(()) => {
                self.metrics.record_rx_accepted();
                Ok(())
            }
            Err(err) => {
                self.metrics.record_rx_accepted_error();
                Err(err)
            }
        }
    }

    fn record_rx_accepted_error_result<T, E>(&self, result: Result<T, E>) -> Result<T, E> {
        if result.is_err() {
            self.metrics.record_rx_accepted_error();
        }

        result
    }
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

struct MmdsRequestBuffers {
    capacity: usize,
    request_len_limit: usize,
    entries: Vec<MmdsRequestBufferEntry>,
}

impl fmt::Debug for MmdsRequestBuffers {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsRequestBuffers")
            .field("capacity", &self.capacity)
            .field("request_len_limit", &self.request_len_limit)
            .field("entry_count", &self.entries.len())
            .finish()
    }
}

impl Default for MmdsRequestBuffers {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_MMDS_REQUEST_BUFFER_CAPACITY,
            request_len_limit: DEFAULT_MMDS_REQUEST_BUFFER_LEN_LIMIT,
            entries: Vec::new(),
        }
    }
}

impl MmdsRequestBuffers {
    fn append_existing(
        &mut self,
        key: MmdsRequestBufferKey,
        sequence_number: u32,
        payload: &[u8],
    ) -> Result<MmdsRequestBufferAppend, MmdsRequestBufferError> {
        let Some(index) = self.entries.iter().position(|entry| entry.key == key) else {
            return Ok(MmdsRequestBufferAppend::NotFound);
        };

        let mut entry = self.entries.swap_remove(index);
        entry.append_payload(sequence_number, payload, self.request_len_limit)?;
        if entry.is_complete() {
            return Ok(MmdsRequestBufferAppend::Complete(
                entry.into_buffered_request(),
            ));
        }

        self.entries.push(entry);
        Ok(MmdsRequestBufferAppend::Buffered)
    }

    fn start_request(
        &mut self,
        key: MmdsRequestBufferKey,
        response_context: MmdsGuestTcpResponseContext,
        sequence_number: u32,
        payload: &[u8],
    ) -> Result<(), MmdsRequestBufferError> {
        if payload.len() > self.request_len_limit {
            return Err(MmdsRequestBufferError::RequestTooLarge {
                len: payload.len(),
                limit: self.request_len_limit,
            });
        }
        if self.entries.len() >= self.capacity {
            return Err(MmdsRequestBufferError::Full {
                capacity: self.capacity,
            });
        }
        let next_sequence_number = mmds_request_next_sequence_number(
            sequence_number,
            payload.len(),
            self.request_len_limit,
        )?;

        let mut buffered_payload = Vec::new();
        buffered_payload
            .try_reserve_exact(payload.len())
            .map_err(|source| MmdsRequestBufferError::PayloadAllocation {
                len: payload.len(),
                source,
            })?;
        buffered_payload.extend_from_slice(payload);
        self.entries
            .try_reserve_exact(1)
            .map_err(|source| MmdsRequestBufferError::EntryAllocation { source })?;
        self.entries.push(MmdsRequestBufferEntry {
            key,
            response_context,
            next_sequence_number,
            payload: buffered_payload,
        });
        Ok(())
    }
}

#[derive(Debug)]
enum MmdsRequestBufferAppend {
    NotFound,
    Buffered,
    Complete(MmdsBufferedRequest),
}

struct MmdsRequestBufferEntry {
    key: MmdsRequestBufferKey,
    response_context: MmdsGuestTcpResponseContext,
    next_sequence_number: u32,
    payload: Vec<u8>,
}

impl fmt::Debug for MmdsRequestBufferEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsRequestBufferEntry")
            .field("key", &self.key)
            .field("response_context", &self.response_context)
            .field("next_sequence_number", &self.next_sequence_number)
            .field("payload", &"[REDACTED]")
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

impl MmdsRequestBufferEntry {
    fn append_payload(
        &mut self,
        sequence_number: u32,
        payload: &[u8],
        request_len_limit: usize,
    ) -> Result<(), MmdsRequestBufferError> {
        if sequence_number != self.next_sequence_number {
            return Err(MmdsRequestBufferError::UnexpectedSequence {
                expected: self.next_sequence_number,
                actual: sequence_number,
            });
        }
        let len = self.payload.len().checked_add(payload.len()).ok_or(
            MmdsRequestBufferError::RequestTooLarge {
                len: usize::MAX,
                limit: request_len_limit,
            },
        )?;
        if len > request_len_limit {
            return Err(MmdsRequestBufferError::RequestTooLarge {
                len,
                limit: request_len_limit,
            });
        }
        let next_sequence_number =
            mmds_request_next_sequence_number(sequence_number, payload.len(), request_len_limit)?;

        self.payload
            .try_reserve_exact(payload.len())
            .map_err(|source| MmdsRequestBufferError::PayloadAllocation { len, source })?;
        self.payload.extend_from_slice(payload);
        self.next_sequence_number = next_sequence_number;
        Ok(())
    }

    fn is_complete(&self) -> bool {
        mmds_http_request_is_complete(&self.payload)
    }

    fn into_buffered_request(self) -> MmdsBufferedRequest {
        MmdsBufferedRequest {
            response_context: self.response_context,
            payload: self.payload,
        }
    }
}

struct MmdsBufferedRequest {
    response_context: MmdsGuestTcpResponseContext,
    payload: Vec<u8>,
}

impl fmt::Debug for MmdsBufferedRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsBufferedRequest")
            .field("response_context", &self.response_context)
            .field("payload", &"[REDACTED]")
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

fn mmds_request_next_sequence_number(
    sequence_number: u32,
    payload_len: usize,
    request_len_limit: usize,
) -> Result<u32, MmdsRequestBufferError> {
    let payload_len =
        u32::try_from(payload_len).map_err(|_| MmdsRequestBufferError::RequestTooLarge {
            len: payload_len,
            limit: request_len_limit,
        })?;
    Ok(sequence_number.wrapping_add(payload_len))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MmdsRequestBufferKey {
    source_ipv4_address: Ipv4Addr,
    destination_ipv4_address: Ipv4Addr,
    source_port: u16,
    destination_port: u16,
}

impl MmdsRequestBufferKey {
    fn from_packet(packet: MmdsGuestTcpPacket<'_>) -> Self {
        Self {
            source_ipv4_address: packet.source_ipv4_address(),
            destination_ipv4_address: packet.destination_ipv4_address(),
            source_port: packet.source_port(),
            destination_port: packet.destination_port(),
        }
    }
}

#[derive(Debug)]
pub enum MmdsRequestBufferError {
    Full { capacity: usize },
    UnexpectedSequence { expected: u32, actual: u32 },
    RequestTooLarge { len: usize, limit: usize },
    PayloadAllocation { len: usize, source: TryReserveError },
    EntryAllocation { source: TryReserveError },
}

impl fmt::Display for MmdsRequestBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full { capacity } => {
                write!(f, "MMDS request buffer is full at capacity {capacity}")
            }
            Self::UnexpectedSequence { expected, actual } => write!(
                f,
                "MMDS request buffer expected TCP sequence number {expected} but received {actual}"
            ),
            Self::RequestTooLarge { len, limit } => {
                write!(f, "MMDS request buffer length {len} exceeds limit {limit}")
            }
            Self::PayloadAllocation { len, source } => write!(
                f,
                "failed to reserve MMDS request buffer payload of {len} bytes: {source}"
            ),
            Self::EntryAllocation { source } => {
                write!(f, "failed to reserve MMDS request buffer entry: {source}")
            }
        }
    }
}

impl std::error::Error for MmdsRequestBufferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PayloadAllocation { source, .. } | Self::EntryAllocation { source } => {
                Some(source)
            }
            Self::Full { .. } | Self::UnexpectedSequence { .. } | Self::RequestTooLarge { .. } => {
                None
            }
        }
    }
}

impl From<MmdsRequestBufferError> for MmdsPacketDetourError {
    fn from(source: MmdsRequestBufferError) -> Self {
        Self::RequestBuffer(source)
    }
}

fn mmds_http_request_is_complete(payload: &[u8]) -> bool {
    payload
        .windows(MMDS_HTTP_HEADER_TERMINATOR.len())
        .any(|window| window == MMDS_HTTP_HEADER_TERMINATOR)
}

#[derive(Clone)]
pub struct MmdsResponseQueue {
    state: Arc<Mutex<MmdsResponseQueueState>>,
    metrics: SharedMmdsMetrics,
}

impl fmt::Debug for MmdsResponseQueue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsResponseQueue")
            .field("state", &"<configured>")
            .field("metrics", &"<configured>")
            .finish()
    }
}

impl Default for MmdsResponseQueue {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_MMDS_RESPONSE_QUEUE_CAPACITY)
    }
}

impl MmdsResponseQueue {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(MmdsResponseQueueState {
                capacity,
                responses: VecDeque::new(),
            })),
            metrics: SharedMmdsMetrics::default(),
        }
    }

    pub fn with_shared_metrics(&self, metrics: SharedMmdsMetrics) -> Self {
        Self {
            state: Arc::clone(&self.state),
            metrics,
        }
    }

    pub fn push_with(
        &self,
        response: impl FnOnce() -> Result<Vec<u8>, MmdsPacketDetourError>,
    ) -> Result<(), MmdsPacketDetourError> {
        self.push_with_direction(response, MmdsResponseQueueDirection::Normal)
    }

    fn push_pair_with(
        &self,
        responses: impl FnOnce() -> Result<[Vec<u8>; 2], MmdsPacketDetourError>,
    ) -> Result<(), MmdsPacketDetourError> {
        let result = (|| {
            let mut state = self.state.lock().map_err(|_| {
                MmdsPacketDetourError::ResponseQueue(MmdsResponseQueueError::Poisoned)
            })?;
            if state.responses.len().saturating_add(2) > state.capacity {
                return Err(MmdsPacketDetourError::ResponseQueue(
                    MmdsResponseQueueError::Full {
                        capacity: state.capacity,
                    },
                ));
            }

            let [first, second] = responses()?;
            state.responses.push_back(MmdsQueuedResponse {
                priority: MmdsResponseQueuePriority::Normal,
                frame: first,
            });
            state.responses.push_back(MmdsQueuedResponse {
                priority: MmdsResponseQueuePriority::Normal,
                frame: second,
            });
            Ok(())
        })();
        if result.is_err() {
            self.metrics.record_tx_error();
        }

        result
    }

    fn push_priority_with(
        &self,
        response: impl FnOnce() -> Result<Vec<u8>, MmdsPacketDetourError>,
    ) -> Result<(), MmdsPacketDetourError> {
        self.push_with_direction(response, MmdsResponseQueueDirection::Priority)
    }

    fn push_with_direction(
        &self,
        response: impl FnOnce() -> Result<Vec<u8>, MmdsPacketDetourError>,
        direction: MmdsResponseQueueDirection,
    ) -> Result<(), MmdsPacketDetourError> {
        let result = (|| {
            let mut state = self.state.lock().map_err(|_| {
                MmdsPacketDetourError::ResponseQueue(MmdsResponseQueueError::Poisoned)
            })?;
            if state.responses.len() >= state.capacity {
                return Err(MmdsPacketDetourError::ResponseQueue(
                    MmdsResponseQueueError::Full {
                        capacity: state.capacity,
                    },
                ));
            }

            let response = response()?;
            match direction {
                MmdsResponseQueueDirection::Normal => {
                    state.responses.push_back(MmdsQueuedResponse {
                        priority: MmdsResponseQueuePriority::Normal,
                        frame: response,
                    });
                }
                MmdsResponseQueueDirection::Priority => {
                    let insert_at = state
                        .responses
                        .iter()
                        .position(|queued| queued.priority == MmdsResponseQueuePriority::Normal)
                        .unwrap_or(state.responses.len());
                    state.responses.insert(
                        insert_at,
                        MmdsQueuedResponse {
                            priority: MmdsResponseQueuePriority::Priority,
                            frame: response,
                        },
                    );
                }
            }
            Ok(())
        })();
        if result.is_err() {
            self.metrics.record_tx_error();
        }

        result
    }

    pub fn copy_front_into(
        &self,
        buffer: &mut [u8],
    ) -> Result<Option<usize>, MmdsResponseQueueError> {
        let result = (|| {
            let state = self
                .state
                .lock()
                .map_err(|_| MmdsResponseQueueError::Poisoned)?;
            let Some(response) = state.responses.front() else {
                return Ok(None);
            };
            let len = response.frame.len();
            let Some(target) = buffer.get_mut(..len) else {
                return Err(MmdsResponseQueueError::ResponseFrameTooLarge {
                    len,
                    buffer_len: buffer.len(),
                });
            };

            target.copy_from_slice(&response.frame);
            Ok(Some(len))
        })();
        if result.is_err() {
            self.metrics.record_tx_error();
        }

        result
    }

    pub fn may_have_response(&self) -> bool {
        match self.state.try_lock() {
            Ok(state) => !state.responses.is_empty(),
            Err(TryLockError::Poisoned(_)) => true,
            Err(TryLockError::WouldBlock) => false,
        }
    }

    pub fn pop_front(&self) -> Result<bool, MmdsResponseQueueError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| MmdsResponseQueueError::Poisoned)?;
        Ok(state.responses.pop_front().is_some())
    }

    #[doc(hidden)]
    pub fn responses(&self) -> Result<Vec<Vec<u8>>, MmdsResponseQueueError> {
        let state = self
            .state
            .lock()
            .map_err(|_| MmdsResponseQueueError::Poisoned)?;
        Ok(state
            .responses
            .iter()
            .map(|response| response.frame.clone())
            .collect())
    }

    #[doc(hidden)]
    pub fn shares_state_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    pub fn record_transmitted(&self, frame_len: usize) {
        self.metrics.record_tx_frame(frame_len);
    }

    pub fn record_transmit_error(&self) {
        self.metrics.record_tx_error();
    }

    #[doc(hidden)]
    pub fn with_lock_for_test<R>(
        &self,
        operation: impl FnOnce() -> R,
    ) -> Result<R, MmdsResponseQueueError> {
        let _guard = self
            .state
            .lock()
            .map_err(|_| MmdsResponseQueueError::Poisoned)?;
        Ok(operation())
    }

    #[doc(hidden)]
    pub fn poison_for_test(&self) {
        let state = Arc::clone(&self.state);
        let _ = std::thread::spawn(move || {
            let _guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::panic::resume_unwind(Box::new("poison test MMDS response queue"));
        })
        .join();
    }
}

struct MmdsResponseQueueState {
    capacity: usize,
    responses: VecDeque<MmdsQueuedResponse>,
}

impl fmt::Debug for MmdsResponseQueueState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsResponseQueueState")
            .field("capacity", &self.capacity)
            .field("response_count", &self.responses.len())
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct MmdsQueuedResponse {
    priority: MmdsResponseQueuePriority,
    frame: Vec<u8>,
}

impl fmt::Debug for MmdsQueuedResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsQueuedResponse")
            .field("priority", &self.priority)
            .field("frame", &"[REDACTED]")
            .field("frame_len", &self.frame.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MmdsResponseQueueDirection {
    Normal,
    Priority,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MmdsResponseQueuePriority {
    Normal,
    Priority,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MmdsResponseQueueError {
    Full { capacity: usize },
    Poisoned,
    ResponseFrameTooLarge { len: usize, buffer_len: usize },
}

impl fmt::Display for MmdsResponseQueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full { capacity } => {
                write!(f, "MMDS response queue is full at capacity {capacity}")
            }
            Self::Poisoned => f.write_str("MMDS response queue lock is poisoned"),
            Self::ResponseFrameTooLarge { len, buffer_len } => write!(
                f,
                "MMDS response frame length {len} exceeds RX buffer length {buffer_len}"
            ),
        }
    }
}

impl std::error::Error for MmdsResponseQueueError {}

#[derive(Debug)]
pub enum MmdsPacketDetourError {
    ArpResponseFrame(MmdsGuestArpResponseFrameError),
    MmdsState(MmdsStateLockError),
    RequestBuffer(MmdsRequestBufferError),
    ResponseFrame(MmdsGuestTcpResponseFrameError),
    ResponseQueue(MmdsResponseQueueError),
}

impl fmt::Display for MmdsPacketDetourError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArpResponseFrame(source) => write!(f, "{source}"),
            Self::MmdsState(source) => write!(f, "{source}"),
            Self::RequestBuffer(source) => write!(f, "{source}"),
            Self::ResponseFrame(source) => write!(f, "{source}"),
            Self::ResponseQueue(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for MmdsPacketDetourError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ArpResponseFrame(source) => Some(source),
            Self::MmdsState(source) => Some(source),
            Self::RequestBuffer(source) => Some(source),
            Self::ResponseFrame(source) => Some(source),
            Self::ResponseQueue(source) => Some(source),
        }
    }
}
#[derive(Debug)]
pub struct MmdsOnlyVirtioNetworkPacketIoProvider {
    #[doc(hidden)]
    pub entries: Vec<MmdsOnlyVirtioNetworkPacketIoProviderEntry>,
}

impl MmdsOnlyVirtioNetworkPacketIoProvider {
    pub fn new(
        entries: Vec<MmdsOnlyVirtioNetworkPacketIoProviderEntry>,
    ) -> Result<Self, MmdsOnlyVirtioNetworkPacketIoProviderBuildError> {
        for (index, entry) in entries.iter().enumerate() {
            if entries
                .iter()
                .skip(index + 1)
                .any(|candidate| candidate.iface_id == entry.iface_id)
            {
                return Err(
                    MmdsOnlyVirtioNetworkPacketIoProviderBuildError::DuplicateInterfaceId {
                        iface_id: entry.iface_id.clone(),
                    },
                );
            }
        }

        Ok(Self { entries })
    }
}

impl Arm64BootNetworkPacketIoProvider for MmdsOnlyVirtioNetworkPacketIoProvider {
    fn packet_io(
        &mut self,
        interface: Arm64BootNetworkInterface<'_>,
    ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
        let iface_id = interface.iface_id();
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.iface_id == iface_id)
        else {
            return Err(Arm64BootNetworkPacketIoError::new(format!(
                "missing MMDS-only packet I/O for interface {iface_id}"
            )));
        };

        let MmdsOnlyVirtioNetworkPacketIo { tx_sink, rx_source } = &mut entry.packet_io;
        Ok(Arm64BootNetworkPacketIo::new(tx_sink, rx_source))
    }
}

#[derive(Debug)]
pub struct MmdsOnlyVirtioNetworkPacketIoProviderEntry {
    #[doc(hidden)]
    pub iface_id: String,
    #[doc(hidden)]
    pub packet_io: MmdsOnlyVirtioNetworkPacketIo,
}

impl MmdsOnlyVirtioNetworkPacketIoProviderEntry {
    pub fn new(iface_id: impl Into<String>, packet_io: MmdsOnlyVirtioNetworkPacketIo) -> Self {
        Self {
            iface_id: iface_id.into(),
            packet_io,
        }
    }
}
#[derive(Debug)]
pub struct MmdsOnlyVirtioNetworkPacketIo {
    #[doc(hidden)]
    pub tx_sink: MmdsOnlyVirtioNetworkTxPacketSink,
    #[doc(hidden)]
    pub rx_source: MmdsOnlyVirtioNetworkRxPacketSource,
}

impl MmdsOnlyVirtioNetworkPacketIo {
    pub fn new(
        mmds_detour: MmdsPacketDetour,
    ) -> Result<Self, MmdsOnlyVirtioNetworkPacketIoBuildError> {
        Self::with_guest_mac(mmds_detour, None)
    }

    pub fn with_guest_mac(
        mmds_detour: MmdsPacketDetour,
        guest_mac: Option<GuestMacAddress>,
    ) -> Result<Self, MmdsOnlyVirtioNetworkPacketIoBuildError> {
        let response_queue = mmds_detour.response_queue();
        Ok(Self {
            tx_sink: MmdsOnlyVirtioNetworkTxPacketSink {
                mmds_detour,
                staged_frame: None,
                guest_mac,
                backend_metrics: VirtioNetworkBackendMetrics::default(),
            },
            rx_source: MmdsOnlyVirtioNetworkRxPacketSource::new(
                response_queue,
                DEFAULT_MMDS_VIRTIO_NETWORK_RX_BUFFER_LEN,
            )?,
        })
    }
}

pub struct MmdsOnlyVirtioNetworkTxPacketSink {
    mmds_detour: MmdsPacketDetour,
    staged_frame: Option<StagedMmdsOnlyTxFrame>,
    guest_mac: Option<GuestMacAddress>,
    backend_metrics: VirtioNetworkBackendMetrics,
}

impl fmt::Debug for MmdsOnlyVirtioNetworkTxPacketSink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsOnlyVirtioNetworkTxPacketSink")
            .field("mmds_detour", &"<configured>")
            .field("staged_frame", &self.staged_frame.is_some())
            .field("guest_mac", &self.guest_mac.map(|_| "<configured>"))
            .finish()
    }
}

struct StagedMmdsOnlyTxFrame {
    packet: VirtioNetworkPacketPlan,
    disposition: VirtioNetworkTxPacketDisposition,
}

impl fmt::Debug for StagedMmdsOnlyTxFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StagedMmdsOnlyTxFrame")
            .field("packet", &"[REDACTED]")
            .field("disposition", &self.disposition)
            .finish()
    }
}

impl MmdsOnlyVirtioNetworkTxPacketSink {
    fn prepare_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<StagedMmdsOnlyTxFrame, VirtioNetworkTxPacketSinkError> {
        let plan = frame
            .prepare_packet(memory)
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        self.prepare_owned_packet_plan(plan)
    }

    fn classify_packet_plan(
        &self,
        plan: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let mut disposition = None;
        let visit = plan
            .visit_packets(VirtioNetworkPacketEnvelope::RawEthernet, |packet| {
                let current = if self.mmds_detour.would_detour_packet(packet) {
                    VirtioNetworkTxPacketDisposition::Detoured
                } else {
                    VirtioNetworkTxPacketDisposition::Forwarded
                };
                if disposition.is_some_and(|previous| previous != current) {
                    return ControlFlow::Break(VirtioNetworkTxPacketSinkError::new(
                        "MMDS classification changed within one normalized TX frame",
                    ));
                }
                disposition = Some(current);
                ControlFlow::Continue(())
            })
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        if let ControlFlow::Break(source) = visit {
            return Err(source);
        }
        disposition.ok_or_else(|| {
            VirtioNetworkTxPacketSinkError::new("MMDS-only normalization emitted no TX packet")
        })
    }

    fn prepare_owned_packet_plan(
        &mut self,
        packet: VirtioNetworkPacketPlan,
    ) -> Result<StagedMmdsOnlyTxFrame, VirtioNetworkTxPacketSinkError> {
        let disposition = self.classify_packet_plan(&packet)?;
        self.observe_source_mac(&packet);
        Ok(StagedMmdsOnlyTxFrame {
            packet,
            disposition,
        })
    }

    fn prepare_borrowed_packet_plan(
        &mut self,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<StagedMmdsOnlyTxFrame, VirtioNetworkTxPacketSinkError> {
        let disposition = self.classify_packet_plan(packet)?;
        let packet = packet
            .try_clone_owned()
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        self.observe_source_mac(&packet);
        Ok(StagedMmdsOnlyTxFrame {
            packet,
            disposition,
        })
    }

    fn observe_source_mac(&mut self, packet: &VirtioNetworkPacketPlan) {
        if let (Some(expected), Some(observed)) = (self.guest_mac, packet.source_mac())
            && expected.octets() != observed
        {
            self.backend_metrics.record_spoofed_mac();
        }
    }

    fn commit_frame(
        &mut self,
        staged: StagedMmdsOnlyTxFrame,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let expected_detour = staged.disposition == VirtioNetworkTxPacketDisposition::Detoured;
        let visit = staged
            .packet
            .visit_packets(
                VirtioNetworkPacketEnvelope::RawEthernet,
                |packet| match self
                    .mmds_detour
                    .detour_packet(packet)
                    .map_err(tx_mmds_detour_error)
                {
                    Ok(detoured) if detoured == expected_detour => ControlFlow::Continue(()),
                    Ok(_) => ControlFlow::Break(VirtioNetworkTxPacketSinkError::new(
                        "MMDS side-effect classification changed at commit",
                    )),
                    Err(source) => ControlFlow::Break(source),
                },
            )
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        if let ControlFlow::Break(source) = visit {
            return Err(source);
        }
        Ok(staged.disposition)
    }
}

impl VirtioNetworkTxPacketSink for MmdsOnlyVirtioNetworkTxPacketSink {
    fn transmit_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let staged = self.prepare_frame(memory, frame)?;
        self.commit_frame(staged)
    }

    fn transmit_prepared_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let staged = self.prepare_borrowed_packet_plan(packet)?;
        self.commit_frame(staged)
    }

    fn supports_staged_batch(&self) -> bool {
        true
    }

    fn stage_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        if self.staged_frame.is_some() {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "MMDS-only TX staging already owns an uncommitted frame",
            ));
        }
        let staged = self.prepare_frame(memory, frame)?;
        self.staged_frame = Some(staged);
        Ok(VirtioNetworkTxPacketStage::Staged {
            flush_before_commit: true,
        })
    }

    fn stage_prepared_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        if self.staged_frame.is_some() {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "MMDS-only TX staging already owns an uncommitted frame",
            ));
        }
        let staged = self.prepare_borrowed_packet_plan(packet)?;
        self.staged_frame = Some(staged);
        Ok(VirtioNetworkTxPacketStage::Staged {
            flush_before_commit: true,
        })
    }

    fn commit_staged_frame(&mut self) -> VirtioNetworkTxPacketCommit {
        let result = self
            .staged_frame
            .take()
            .ok_or_else(|| {
                VirtioNetworkTxPacketSinkError::new("MMDS-only TX commit has no staged frame")
            })
            .and_then(|staged| self.commit_frame(staged));
        VirtioNetworkTxPacketCommit::Immediate(result)
    }

    fn discard_staged_frame(&mut self) {
        self.staged_frame = None;
    }

    fn take_backend_metrics(&mut self) -> VirtioNetworkBackendMetrics {
        std::mem::take(&mut self.backend_metrics)
    }
}

pub struct MmdsOnlyVirtioNetworkRxPacketSource {
    response_queue: MmdsResponseQueue,
    read_buffer: Vec<u8>,
    cached_len: Option<usize>,
}

impl fmt::Debug for MmdsOnlyVirtioNetworkRxPacketSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MmdsOnlyVirtioNetworkRxPacketSource")
            .field("response_queue", &self.response_queue)
            .field("read_buffer", &"[REDACTED]")
            .field("read_buffer_len", &self.read_buffer.len())
            .field("cached_len", &self.cached_len)
            .finish()
    }
}

impl MmdsOnlyVirtioNetworkRxPacketSource {
    fn new(
        response_queue: MmdsResponseQueue,
        rx_buffer_len: usize,
    ) -> Result<Self, MmdsOnlyVirtioNetworkPacketIoBuildError> {
        if rx_buffer_len == 0 {
            return Err(MmdsOnlyVirtioNetworkPacketIoBuildError::EmptyRxBuffer);
        }

        let mut read_buffer = Vec::new();
        read_buffer
            .try_reserve_exact(rx_buffer_len)
            .map_err(
                |source| MmdsOnlyVirtioNetworkPacketIoBuildError::RxBufferAllocation {
                    len: rx_buffer_len,
                    source,
                },
            )?;
        read_buffer.resize(rx_buffer_len, 0);

        Ok(Self {
            response_queue,
            read_buffer,
            cached_len: None,
        })
    }

    fn cached_packet(&self) -> Option<VirtioNetworkRxPacket<'_>> {
        let len = self.cached_len?;
        self.read_buffer.get(..len).map(VirtioNetworkRxPacket::new)
    }
}

impl VirtioNetworkRxPacketSource for MmdsOnlyVirtioNetworkRxPacketSource {
    fn retry_after_tx_hint(&self) -> bool {
        self.cached_len.is_some() || self.response_queue.may_have_response()
    }

    fn peek_packet(
        &mut self,
    ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
        if self.cached_len.is_some() {
            return Ok(self.cached_packet());
        }

        if let Some(len) = self
            .response_queue
            .copy_front_into(&mut self.read_buffer)
            .map_err(rx_mmds_response_queue_error)?
        {
            self.cached_len = Some(len);
        }

        Ok(self.cached_packet())
    }

    fn consume_packet(&mut self) {
        if let Some(len) = self.cached_len.take() {
            match self.response_queue.pop_front() {
                Ok(true) => self.response_queue.record_transmitted(len),
                Ok(false) | Err(_) => self.response_queue.record_transmit_error(),
            }
        }
    }
}

fn tx_mmds_detour_error(source: MmdsPacketDetourError) -> VirtioNetworkTxPacketSinkError {
    VirtioNetworkTxPacketSinkError::new(format!("MMDS packet detour failed: {source}"))
}

fn rx_mmds_response_queue_error(
    source: MmdsResponseQueueError,
) -> VirtioNetworkRxPacketSourceError {
    VirtioNetworkRxPacketSourceError::new(format!("MMDS response queue read failed: {source}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mmds::{DEFAULT_MMDS_IPV4_ADDRESS, MMDS_GUEST_TCP_PORT};
    use crate::network::VirtioNetworkTxHeader;

    fn test_mmds_tcp_packet(payload: &[u8]) -> Vec<u8> {
        const ETHERNET_HEADER_LEN: usize = 14;
        const IPV4_HEADER_LEN: usize = 20;
        const TCP_HEADER_LEN: usize = 20;

        let ipv4_total_len = u16::try_from(IPV4_HEADER_LEN + TCP_HEADER_LEN + payload.len())
            .expect("test IPv4 packet length should fit u16");
        let mut packet = Vec::with_capacity(ETHERNET_HEADER_LEN + usize::from(ipv4_total_len));
        packet.extend_from_slice(&[0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
        packet.extend_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
        packet.extend_from_slice(&0x0800_u16.to_be_bytes());

        packet.push(0x45);
        packet.push(0);
        packet.extend_from_slice(&ipv4_total_len.to_be_bytes());
        packet.extend_from_slice(&0x1234_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.push(64);
        packet.push(6);
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&[10, 0, 0, 2]);
        packet.extend_from_slice(&DEFAULT_MMDS_IPV4_ADDRESS.octets());

        packet.extend_from_slice(&49152_u16.to_be_bytes());
        packet.extend_from_slice(&MMDS_GUEST_TCP_PORT.to_be_bytes());
        packet.extend_from_slice(&7_u32.to_be_bytes());
        packet.extend_from_slice(&1_u32.to_be_bytes());
        packet.push(0x50);
        packet.push(0x18);
        packet.extend_from_slice(&4096_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(&0_u16.to_be_bytes());
        packet.extend_from_slice(payload);
        packet
    }

    fn assert_debug_redacts(debug_output: &str, protected_value: &str) {
        let byte_sequence = protected_value
            .as_bytes()
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        assert!(!debug_output.contains(protected_value));
        assert!(!debug_output.contains(&byte_sequence));
    }

    #[test]
    fn mmds_request_buffer_debug_surfaces_redact_token_payloads() {
        let token_value = "private-buffered-token-value-that-must-not-appear";
        let request = format!("GET /meta-data HTTP/1.1\r\nX-metadata-token: {token_value}");
        let packet = test_mmds_tcp_packet(request.as_bytes());
        let classified = classify_mmds_guest_tcp_packet(&packet, DEFAULT_MMDS_IPV4_ADDRESS)
            .expect("test MMDS packet should classify");

        let mut request_buffers = MmdsRequestBuffers::default();
        request_buffers
            .start_request(
                MmdsRequestBufferKey::from_packet(classified),
                classified.response_context(),
                classified.sequence_number(),
                classified.payload(),
            )
            .expect("incomplete request should buffer");
        let buffers_debug = format!("{request_buffers:?}");
        let entry_debug = format!(
            "{:?}",
            request_buffers
                .entries
                .first()
                .expect("buffered request entry should exist")
        );
        let buffered_request_debug = format!(
            "{:?}",
            request_buffers
                .entries
                .pop()
                .expect("buffered request entry should exist")
                .into_buffered_request()
        );

        let mut detour = MmdsPacketDetour::new(
            MmdsStateHandle::default(),
            DEFAULT_MMDS_IPV4_ADDRESS,
            MmdsResponseQueue::default(),
            SharedMmdsMetrics::default(),
        );
        assert!(
            detour
                .detour_packet(&packet)
                .expect("incomplete MMDS request should detour")
        );
        let detour_debug = format!("{detour:?}");

        for debug_output in [
            &buffers_debug,
            &entry_debug,
            &buffered_request_debug,
            &detour_debug,
        ] {
            assert_debug_redacts(debug_output, token_value);
        }
        assert!(buffers_debug.contains("entry_count: 1"));
        assert!(entry_debug.contains("[REDACTED]"));
        assert!(buffered_request_debug.contains("[REDACTED]"));
        assert!(detour_debug.contains("entry_count: 1"));
    }

    #[test]
    fn mmds_response_debug_surfaces_redact_queued_and_cached_frames() {
        let token_value = "private-response-token-value-that-must-not-appear";
        let queue = MmdsResponseQueue::with_capacity(2);
        queue
            .push_with(|| Ok(token_value.as_bytes().to_vec()))
            .expect("test response should queue");

        let queue_debug = format!("{queue:?}");
        let state = queue.state.lock().expect("test response queue should lock");
        let state_debug = format!("{state:?}");
        let queued_response_debug = format!(
            "{:?}",
            state
                .responses
                .front()
                .expect("queued response should exist")
        );
        drop(state);

        let mut rx_source = MmdsOnlyVirtioNetworkRxPacketSource::new(queue, 256)
            .expect("test MMDS RX source should build");
        assert!(
            rx_source
                .peek_packet()
                .expect("queued response should be readable")
                .is_some()
        );
        let rx_source_debug = format!("{rx_source:?}");

        for debug_output in [
            &queue_debug,
            &state_debug,
            &queued_response_debug,
            &rx_source_debug,
        ] {
            assert_debug_redacts(debug_output, token_value);
        }
        assert!(state_debug.contains("response_count: 1"));
        assert!(queued_response_debug.contains("[REDACTED]"));
        assert!(rx_source_debug.contains("[REDACTED]"));
        assert!(rx_source_debug.contains(&format!("cached_len: Some({})", token_value.len())));
    }

    #[test]
    fn staged_mmds_tx_frame_debug_redacts_packet_bytes() {
        let token_value = "private-staged-token-value-that-must-not-appear";
        let mut packet = vec![0; 14];
        packet.extend_from_slice(token_value.as_bytes());
        let staged = StagedMmdsOnlyTxFrame {
            packet: VirtioNetworkPacketPlan::prepare(VirtioNetworkTxHeader::new(), 0, packet)
                .expect("test Ethernet packet should validate"),
            disposition: VirtioNetworkTxPacketDisposition::Forwarded,
        };
        let debug_output = format!("{staged:?}");

        assert_debug_redacts(&debug_output, token_value);
        assert!(debug_output.contains("[REDACTED]"));
    }

    #[test]
    fn mmds_only_tx_observes_configured_source_mac_without_filtering() {
        let expected = GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 1]);
        let detour = MmdsPacketDetour::new(
            MmdsStateHandle::default(),
            DEFAULT_MMDS_IPV4_ADDRESS,
            MmdsResponseQueue::default(),
            SharedMmdsMetrics::default(),
        );
        let mut packet_io = MmdsOnlyVirtioNetworkPacketIo::with_guest_mac(detour, Some(expected))
            .expect("MMDS-only packet I/O should build");
        let mut packet = vec![0; 14];
        packet[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 2]);
        let plan = VirtioNetworkPacketPlan::prepare(VirtioNetworkTxHeader::new(), 0, packet)
            .expect("plain Ethernet packet should validate");

        let staged = packet_io
            .tx_sink
            .prepare_borrowed_packet_plan(&plan)
            .expect("spoof observation must not filter the packet");

        assert_eq!(
            staged.disposition,
            VirtioNetworkTxPacketDisposition::Forwarded
        );
        assert_eq!(
            packet_io
                .tx_sink
                .take_backend_metrics()
                .tx_spoofed_mac_count(),
            1
        );
        assert_eq!(
            packet_io.tx_sink.take_backend_metrics(),
            VirtioNetworkBackendMetrics::default()
        );
    }
}
