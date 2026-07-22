//! Adapters between internal virtio-net packet traits and vmnet packet I/O.

use std::collections::TryReserveError;
use std::fmt;
use std::ops::{ControlFlow, Range};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bangbang_runtime::memory::GuestMemory;
pub(crate) use bangbang_runtime::mmds_network::{
    MmdsNetworkStackBuildError, MmdsNetworkStackError, MmdsNetworkStackHandle,
    MmdsOnlyVirtioNetworkPacketIo, MmdsOnlyVirtioNetworkPacketIoBuildError, MmdsPacketDetour,
};
use bangbang_runtime::network::{
    GuestMacAddress, VIRTIO_NET_MAX_BUFFER_SIZE, VIRTIO_NET_TX_HEADER_SIZE,
    VirtioNetworkBackendMetrics, VirtioNetworkPacketEnvelope, VirtioNetworkRxPacket,
    VirtioNetworkRxPacketSource, VirtioNetworkRxPacketSourceError, VirtioNetworkTxFrame,
    VirtioNetworkTxPacketCommit, VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSink,
    VirtioNetworkTxPacketSinkError, VirtioNetworkTxPacketStage,
};
use bangbang_runtime::network_packet::VirtioNetworkPacketPlan;
use bangbang_runtime::startup::{
    Arm64BootNetworkInterface, Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError,
    Arm64BootNetworkPacketIoProvider,
};

use crate::host_network::vmnet::{
    StartedVmnetPacketIoBackend, VMNET_MAX_BYTES_PER_OPERATION, VMNET_MAX_PACKETS_PER_OPERATION,
    VmnetError, VmnetInterfaceBackend, VmnetPacketAvailableCallback, VmnetPacketIoBackend,
    VmnetPacketIoError,
};
#[cfg(test)]
use crate::host_network::vmnet::{VmnetReadPacket, VmnetWritePacket};

pub const DEFAULT_VMNET_VIRTIO_NETWORK_RX_BUFFER_LEN: usize = VIRTIO_NET_MAX_BUFFER_SIZE as usize;
const VMNET_READINESS_READY_BIT: u64 = 1;
const VMNET_READINESS_MAX_EPOCH: u64 = u64::MAX >> 1;

struct VmnetPacketReadinessState {
    generation: u64,
    active: AtomicBool,
    event: AtomicU64,
    scheduled: AtomicBool,
    estimated_packets: AtomicUsize,
    signal: SyncSender<()>,
}

impl fmt::Debug for VmnetPacketReadinessState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetPacketReadinessState")
            .field("generation", &self.generation)
            .field("active", &self.active.load(Ordering::Acquire))
            .field("ready", &self.is_ready())
            .field("scheduled", &self.scheduled.load(Ordering::Acquire))
            .finish()
    }
}

impl VmnetPacketReadinessState {
    fn is_ready(&self) -> bool {
        self.event.load(Ordering::Acquire) & VMNET_READINESS_READY_BIT != 0
    }

    fn schedule_if_ready(&self) {
        if !self.active.load(Ordering::Acquire) || !self.is_ready() {
            return;
        }
        if self
            .scheduled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            match self.signal.try_send(()) {
                Ok(()) | Err(TrySendError::Full(())) | Err(TrySendError::Disconnected(())) => {}
            }
        }
    }

    fn publish(&self, estimate: Option<u64>) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        self.estimated_packets.store(
            estimate
                .and_then(|estimate| usize::try_from(estimate).ok())
                .unwrap_or(1)
                .clamp(1, VMNET_MAX_PACKETS_PER_OPERATION),
            Ordering::Release,
        );
        let previous = self
            .event
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |event| {
                let epoch = event >> 1;
                let next_epoch = epoch.saturating_add(1).min(VMNET_READINESS_MAX_EPOCH);
                Some((next_epoch << 1) | VMNET_READINESS_READY_BIT)
            })
            .unwrap_or_else(|event| event);
        if previous & VMNET_READINESS_READY_BIT == 0 {
            self.schedule_if_ready();
        }
    }

    fn snapshot(&self) -> u64 {
        self.event.load(Ordering::Acquire)
    }

    fn clear_if_unchanged(&self, snapshot: u64) {
        if snapshot >> 1 == VMNET_READINESS_MAX_EPOCH {
            return;
        }
        let _ = self.event.compare_exchange(
            snapshot,
            snapshot & !VMNET_READINESS_READY_BIT,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    fn retain_ready(&self) {
        self.event
            .fetch_or(VMNET_READINESS_READY_BIT, Ordering::AcqRel);
    }

    fn take_scheduled(&self) -> bool {
        self.scheduled.swap(false, Ordering::AcqRel)
    }

    fn retire(&self) {
        self.active.store(false, Ordering::Release);
        self.scheduled.store(false, Ordering::Release);
        self.event
            .fetch_and(!VMNET_READINESS_READY_BIT, Ordering::AcqRel);
    }
}

/// Non-clone ownership proof for one exact registry generation.
pub(crate) struct VmnetPacketReadinessLease {
    state: Arc<VmnetPacketReadinessState>,
}

impl fmt::Debug for VmnetPacketReadinessLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetPacketReadinessLease")
            .field("generation", &self.state.generation)
            .finish_non_exhaustive()
    }
}

impl VmnetPacketReadinessLease {
    pub(crate) fn new(
        generation: u64,
        signal: SyncSender<()>,
    ) -> (Self, VmnetPacketAvailableCallback) {
        let state = Arc::new(VmnetPacketReadinessState {
            generation,
            active: AtomicBool::new(true),
            event: AtomicU64::new(0),
            scheduled: AtomicBool::new(false),
            estimated_packets: AtomicUsize::new(1),
            signal,
        });
        let callback_state = Arc::clone(&state);
        let callback = VmnetPacketAvailableCallback::new(move |estimate| {
            callback_state.publish(estimate);
        });
        (Self { state }, callback)
    }

    fn consumer(&self) -> VmnetPacketReadinessConsumer {
        VmnetPacketReadinessConsumer {
            state: Arc::clone(&self.state),
        }
    }

    fn retire(&self) {
        self.state.retire();
    }

    pub(crate) fn take_scheduled(&self) -> bool {
        self.state.take_scheduled()
    }
}

impl Drop for VmnetPacketReadinessLease {
    fn drop(&mut self) {
        self.retire();
    }
}

#[derive(Clone)]
struct VmnetPacketReadinessConsumer {
    state: Arc<VmnetPacketReadinessState>,
}
#[derive(Debug)]
pub enum VmnetVirtioNetworkPacketIoBuildError {
    EmptyRxBuffer,
    RxBufferTooSmall,
    RxBufferAllocation { source: TryReserveError },
    TxBufferAllocation { source: TryReserveError },
    BatchMetadataAllocation { source: TryReserveError },
}

#[derive(Debug)]
pub enum VmnetVirtioNetworkPacketIoStopError {
    StatePoisoned,
    Stop { source: VmnetError },
}

impl fmt::Display for VmnetVirtioNetworkPacketIoStopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StatePoisoned => {
                f.write_str("vmnet virtio-net packet state is unavailable during stop")
            }
            Self::Stop { source } => write!(f, "failed to stop vmnet packet I/O: {source}"),
        }
    }
}

impl std::error::Error for VmnetVirtioNetworkPacketIoStopError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Stop { source } => Some(source),
            Self::StatePoisoned => None,
        }
    }
}

impl fmt::Display for VmnetVirtioNetworkPacketIoBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRxBuffer => f.write_str("vmnet virtio-net RX buffer must not be empty"),
            Self::RxBufferTooSmall => {
                f.write_str("prepared vmnet virtio-net RX buffer is smaller than the host bound")
            }
            Self::RxBufferAllocation { source } => {
                write!(f, "failed to reserve vmnet virtio-net RX buffer: {source}")
            }
            Self::TxBufferAllocation { source } => {
                write!(
                    f,
                    "failed to reserve vmnet virtio-net TX staging buffer: {source}"
                )
            }
            Self::BatchMetadataAllocation { source } => {
                write!(
                    f,
                    "failed to reserve vmnet virtio-net batch metadata: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VmnetVirtioNetworkPacketIoBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RxBufferAllocation { source }
            | Self::TxBufferAllocation { source }
            | Self::BatchMetadataAllocation { source } => Some(source),
            Self::EmptyRxBuffer | Self::RxBufferTooSmall => None,
        }
    }
}

pub struct PreparedVmnetVirtioNetworkRxBuffer {
    rx_buffer: Vec<u8>,
    tx_buffer: Vec<u8>,
    packet_lengths: Vec<usize>,
    packet_ranges: Vec<Range<usize>>,
}

impl PreparedVmnetVirtioNetworkRxBuffer {
    pub fn supported_maximum() -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        Self::with_aggregate_len(VMNET_MAX_BYTES_PER_OPERATION)
    }

    fn with_len(rx_buffer_len: usize) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        Self::with_aggregate_len(rx_buffer_len)
    }

    fn with_aggregate_len(
        rx_buffer_len: usize,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        if rx_buffer_len == 0 {
            return Err(VmnetVirtioNetworkPacketIoBuildError::EmptyRxBuffer);
        }
        let mut rx_buffer = Vec::new();
        rx_buffer
            .try_reserve_exact(rx_buffer_len)
            .map_err(
                |source| VmnetVirtioNetworkPacketIoBuildError::RxBufferAllocation { source },
            )?;
        rx_buffer.resize(rx_buffer_len, 0);
        let mut tx_buffer = Vec::new();
        tx_buffer
            .try_reserve_exact(VMNET_MAX_BYTES_PER_OPERATION)
            .map_err(
                |source| VmnetVirtioNetworkPacketIoBuildError::TxBufferAllocation { source },
            )?;
        let mut packet_lengths = Vec::new();
        packet_lengths
            .try_reserve_exact(VMNET_MAX_PACKETS_PER_OPERATION)
            .map_err(
                |source| VmnetVirtioNetworkPacketIoBuildError::BatchMetadataAllocation { source },
            )?;
        packet_lengths.resize(VMNET_MAX_PACKETS_PER_OPERATION, 0);
        let mut packet_ranges = Vec::new();
        packet_ranges
            .try_reserve_exact(VMNET_MAX_PACKETS_PER_OPERATION)
            .map_err(
                |source| VmnetVirtioNetworkPacketIoBuildError::BatchMetadataAllocation { source },
            )?;
        Ok(Self {
            rx_buffer,
            tx_buffer,
            packet_lengths,
            packet_ranges,
        })
    }

    #[cfg(test)]
    pub(crate) fn into_buffer_with_len(
        mut self,
        rx_buffer_len: usize,
    ) -> Result<Vec<u8>, VmnetVirtioNetworkPacketIoBuildError> {
        if rx_buffer_len == 0 {
            return Err(VmnetVirtioNetworkPacketIoBuildError::EmptyRxBuffer);
        }
        if rx_buffer_len > self.rx_buffer.len() {
            return Err(VmnetVirtioNetworkPacketIoBuildError::RxBufferTooSmall);
        }
        self.rx_buffer.truncate(rx_buffer_len);
        Ok(self.rx_buffer)
    }

    fn into_batch_parts(
        mut self,
        packet_capacity: usize,
        read_batch_size: usize,
        write_batch_size: usize,
    ) -> Result<PreparedVmnetVirtioNetworkBatchParts, VmnetVirtioNetworkPacketIoBuildError> {
        if packet_capacity == 0 || read_batch_size == 0 || write_batch_size == 0 {
            return Err(VmnetVirtioNetworkPacketIoBuildError::EmptyRxBuffer);
        }
        let aggregate_len = packet_capacity
            .checked_mul(read_batch_size)
            .filter(|len| *len <= VMNET_MAX_BYTES_PER_OPERATION)
            .ok_or(VmnetVirtioNetworkPacketIoBuildError::RxBufferTooSmall)?;
        if aggregate_len > self.rx_buffer.len()
            || read_batch_size > self.packet_lengths.len()
            || write_batch_size > self.packet_ranges.capacity()
        {
            return Err(VmnetVirtioNetworkPacketIoBuildError::RxBufferTooSmall);
        }
        self.rx_buffer.truncate(aggregate_len);
        self.packet_lengths.truncate(read_batch_size);
        Ok(PreparedVmnetVirtioNetworkBatchParts {
            rx_buffer: self.rx_buffer,
            tx_buffer: self.tx_buffer,
            packet_lengths: self.packet_lengths,
            packet_ranges: self.packet_ranges,
            packet_capacity,
            read_batch_size,
            write_batch_size,
        })
    }
}

struct PreparedVmnetVirtioNetworkBatchParts {
    rx_buffer: Vec<u8>,
    tx_buffer: Vec<u8>,
    packet_lengths: Vec<usize>,
    packet_ranges: Vec<Range<usize>>,
    packet_capacity: usize,
    read_batch_size: usize,
    write_batch_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmnetVirtioNetworkBatchLimits {
    packet_capacity: usize,
    read_batch_size: usize,
    write_batch_size: usize,
}

impl VmnetVirtioNetworkBatchLimits {
    pub(crate) const fn new(
        packet_capacity: usize,
        read_batch_size: usize,
        write_batch_size: usize,
    ) -> Self {
        Self {
            packet_capacity,
            read_batch_size,
            write_batch_size,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VmnetVirtioNetworkPacketProfile {
    limits: VmnetVirtioNetworkBatchLimits,
    packet_envelope: VirtioNetworkPacketEnvelope,
    guest_mac: Option<GuestMacAddress>,
}

impl VmnetVirtioNetworkPacketProfile {
    pub(crate) const fn new(
        limits: VmnetVirtioNetworkBatchLimits,
        packet_envelope: VirtioNetworkPacketEnvelope,
        guest_mac: Option<GuestMacAddress>,
    ) -> Self {
        Self {
            limits,
            packet_envelope,
            guest_mac,
        }
    }
}

impl fmt::Debug for PreparedVmnetVirtioNetworkRxBuffer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedVmnetVirtioNetworkRxBuffer(<owned>)")
    }
}

pub struct VmnetVirtioNetworkPacketIo<B>
where
    B: VmnetPacketIoBackend,
{
    // Drop first so panic/fallback teardown retires callback publication before
    // the shared backend disables, drains, and stops the interface.
    readiness_lease: Option<VmnetPacketReadinessLease>,
    tx_sink: VmnetVirtioNetworkTxPacketSink<B>,
    rx_source: VmnetVirtioNetworkRxPacketSource<B>,
}

impl<B> fmt::Debug for VmnetVirtioNetworkPacketIo<B>
where
    B: VmnetPacketIoBackend,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetVirtioNetworkPacketIo(<owned>)")
    }
}

impl<B> VmnetVirtioNetworkPacketIo<B>
where
    B: VmnetPacketIoBackend,
{
    pub fn new(
        backend: B,
        interface: B::Interface,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        Self::with_rx_buffer_len(
            backend,
            interface,
            DEFAULT_VMNET_VIRTIO_NETWORK_RX_BUFFER_LEN,
        )
    }

    pub fn with_rx_buffer_len(
        backend: B,
        interface: B::Interface,
        rx_buffer_len: usize,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        let prepared = PreparedVmnetVirtioNetworkRxBuffer::with_len(rx_buffer_len)?;
        Self::with_prepared_rx_buffer_and_mmds_detour(
            backend,
            interface,
            prepared,
            rx_buffer_len,
            None,
        )
    }

    #[cfg(test)]
    pub(crate) fn with_mmds_detour(
        backend: B,
        interface: B::Interface,
        mmds_detour: MmdsPacketDetour,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        let prepared = PreparedVmnetVirtioNetworkRxBuffer::supported_maximum()?;
        Self::with_prepared_rx_buffer_and_mmds_detour(
            backend,
            interface,
            prepared,
            DEFAULT_VMNET_VIRTIO_NETWORK_RX_BUFFER_LEN,
            Some(mmds_detour),
        )
    }

    pub(crate) fn with_prepared_rx_buffer_and_mmds_detour(
        backend: B,
        interface: B::Interface,
        prepared: PreparedVmnetVirtioNetworkRxBuffer,
        rx_buffer_len: usize,
        mmds_detour: Option<MmdsPacketDetour>,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        Self::with_prepared_batch_and_mmds_detour(
            backend,
            interface,
            prepared,
            VmnetVirtioNetworkBatchLimits::new(rx_buffer_len, 1, 1),
            mmds_detour,
            None,
        )
    }

    pub(crate) fn with_prepared_batch_and_mmds_detour(
        backend: B,
        interface: B::Interface,
        prepared: PreparedVmnetVirtioNetworkRxBuffer,
        limits: VmnetVirtioNetworkBatchLimits,
        mmds_detour: Option<MmdsPacketDetour>,
        readiness_lease: Option<VmnetPacketReadinessLease>,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        Self::with_prepared_batch_envelope_and_mmds_detour(
            backend,
            interface,
            prepared,
            VmnetVirtioNetworkPacketProfile::new(
                limits,
                VirtioNetworkPacketEnvelope::RawEthernet,
                None,
            ),
            mmds_detour,
            readiness_lease,
        )
    }

    pub(crate) fn with_prepared_batch_envelope_and_mmds_detour(
        backend: B,
        interface: B::Interface,
        prepared: PreparedVmnetVirtioNetworkRxBuffer,
        profile: VmnetVirtioNetworkPacketProfile,
        mmds_detour: Option<MmdsPacketDetour>,
        readiness_lease: Option<VmnetPacketReadinessLease>,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        let parts = prepared.into_batch_parts(
            profile.limits.packet_capacity,
            profile.limits.read_batch_size,
            profile.limits.write_batch_size,
        )?;
        let readiness = readiness_lease
            .as_ref()
            .map(VmnetPacketReadinessLease::consumer);
        let shared = Arc::new(Mutex::new(VmnetVirtioNetworkPacketIoState {
            backend,
            interface,
        }));
        let mmds_stack = mmds_detour.as_ref().map(MmdsPacketDetour::stack);

        let tx_sink = VmnetVirtioNetworkTxPacketSink {
            shared: Arc::clone(&shared),
            mmds_detour,
            staging_buffer: parts.tx_buffer,
            committed_ranges: parts.packet_ranges,
            committed_frames: Vec::new(),
            committed_packet_count: 0,
            committed_emitted_len: 0,
            staged_frame: None,
            maximum_packet_size: parts.packet_capacity,
            maximum_batch_size: parts.write_batch_size,
            packet_envelope: profile.packet_envelope,
            guest_mac: profile.guest_mac,
            backend_metrics: VirtioNetworkBackendMetrics::default(),
        };
        let rx_source = VmnetVirtioNetworkRxPacketSource {
            shared,
            read_buffer: parts.rx_buffer,
            packet_lengths: parts.packet_lengths,
            packet_capacity: parts.packet_capacity,
            read_batch_size: parts.read_batch_size,
            cached_packet_index: 0,
            cached_packet_count: 0,
            cached_packet_source: None,
            host_batch_attempted: None,
            mmds_stack,
            readiness,
            packet_envelope: profile.packet_envelope,
            backend_metrics: VirtioNetworkBackendMetrics::default(),
        };

        Ok(Self {
            tx_sink,
            rx_source,
            readiness_lease,
        })
    }

    pub fn tx_sink(&mut self) -> &mut VmnetVirtioNetworkTxPacketSink<B> {
        &mut self.tx_sink
    }

    pub fn rx_source(&mut self) -> &mut VmnetVirtioNetworkRxPacketSource<B> {
        &mut self.rx_source
    }

    pub fn as_packet_io(&mut self) -> Arm64BootNetworkPacketIo<'_> {
        let Self {
            tx_sink,
            rx_source,
            readiness_lease: _,
        } = self;
        Arm64BootNetworkPacketIo::new(tx_sink, rx_source)
    }

    pub(crate) fn take_scheduled_readiness(&self) -> bool {
        self.readiness_lease
            .as_ref()
            .is_some_and(VmnetPacketReadinessLease::take_scheduled)
    }

    pub(crate) fn has_persistent_readiness(&self) -> bool {
        self.rx_source.has_host_readiness() || self.rx_source.has_mmds_readiness()
    }

    pub(crate) fn mmds_retry_after(&self) -> Option<Duration> {
        self.rx_source.mmds_retry_after()
    }
}

impl<B> VmnetVirtioNetworkPacketIo<StartedVmnetPacketIoBackend<B>>
where
    B: VmnetInterfaceBackend
        + VmnetPacketIoBackend<Interface = <B as VmnetInterfaceBackend>::Interface>,
{
    pub(crate) fn enable_packet_available_callback(
        &mut self,
        callback: VmnetPacketAvailableCallback,
    ) -> Result<(), VmnetVirtioNetworkPacketIoStopError> {
        let mut state = self
            .tx_sink
            .shared
            .lock()
            .map_err(|_| VmnetVirtioNetworkPacketIoStopError::StatePoisoned)?;
        state
            .backend
            .enable_packet_available_callback(callback)
            .map_err(|source| VmnetVirtioNetworkPacketIoStopError::Stop { source })
    }

    pub fn stop(&mut self) -> Result<(), VmnetVirtioNetworkPacketIoStopError> {
        if let Some(readiness) = &self.readiness_lease {
            readiness.retire();
        }
        let mut state = self
            .tx_sink
            .shared
            .lock()
            .map_err(|_| VmnetVirtioNetworkPacketIoStopError::StatePoisoned)?;
        let result = state
            .backend
            .stop()
            .map_err(|source| VmnetVirtioNetworkPacketIoStopError::Stop { source });
        drop(state);
        if result.is_ok() {
            self.readiness_lease = None;
        }
        result
    }
}

pub struct VmnetVirtioNetworkPacketIoProviderEntry<B>
where
    B: VmnetPacketIoBackend,
{
    iface_id: String,
    packet_io: VmnetVirtioNetworkPacketIo<B>,
}

impl<B> fmt::Debug for VmnetVirtioNetworkPacketIoProviderEntry<B>
where
    B: VmnetPacketIoBackend,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetVirtioNetworkPacketIoProviderEntry")
            .field("iface_id", &"<redacted>")
            .field("packet_io", &"<owned>")
            .finish()
    }
}

impl<B> VmnetVirtioNetworkPacketIoProviderEntry<B>
where
    B: VmnetPacketIoBackend,
{
    pub fn new(iface_id: impl Into<String>, packet_io: VmnetVirtioNetworkPacketIo<B>) -> Self {
        Self {
            iface_id: iface_id.into(),
            packet_io,
        }
    }

    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmnetVirtioNetworkPacketIoProviderBuildError {
    DuplicateInterfaceId { iface_id: String },
}

impl fmt::Display for VmnetVirtioNetworkPacketIoProviderBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateInterfaceId { iface_id } => {
                write!(f, "duplicate vmnet packet I/O interface id {iface_id}")
            }
        }
    }
}

impl std::error::Error for VmnetVirtioNetworkPacketIoProviderBuildError {}

pub struct VmnetVirtioNetworkPacketIoProvider<B>
where
    B: VmnetPacketIoBackend,
{
    entries: Vec<VmnetVirtioNetworkPacketIoProviderEntry<B>>,
}

impl<B> fmt::Debug for VmnetVirtioNetworkPacketIoProvider<B>
where
    B: VmnetPacketIoBackend,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetVirtioNetworkPacketIoProvider")
            .field("entry_count", &self.entries.len())
            .finish()
    }
}

impl<B> VmnetVirtioNetworkPacketIoProvider<B>
where
    B: VmnetPacketIoBackend,
{
    pub fn new(
        entries: Vec<VmnetVirtioNetworkPacketIoProviderEntry<B>>,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoProviderBuildError> {
        for (index, entry) in entries.iter().enumerate() {
            if entries
                .iter()
                .skip(index + 1)
                .any(|candidate| candidate.iface_id == entry.iface_id)
            {
                return Err(
                    VmnetVirtioNetworkPacketIoProviderBuildError::DuplicateInterfaceId {
                        iface_id: entry.iface_id.clone(),
                    },
                );
            }
        }

        Ok(Self { entries })
    }
}

impl<B> Arm64BootNetworkPacketIoProvider for VmnetVirtioNetworkPacketIoProvider<B>
where
    B: VmnetPacketIoBackend,
{
    fn take_scheduled_packet_readiness(&mut self) -> bool {
        let mut scheduled = false;
        for entry in &self.entries {
            scheduled |= entry.packet_io.take_scheduled_readiness();
        }
        scheduled
    }

    fn has_packet_readiness(&self, interface: Arm64BootNetworkInterface<'_>) -> bool {
        self.entries
            .iter()
            .find(|entry| entry.iface_id == interface.iface_id())
            .is_some_and(|entry| entry.packet_io.has_persistent_readiness())
    }

    fn packet_retry_after(&self, interface: Arm64BootNetworkInterface<'_>) -> Option<Duration> {
        self.entries
            .iter()
            .find(|entry| entry.iface_id == interface.iface_id())
            .and_then(|entry| entry.packet_io.mmds_retry_after())
    }

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
                "missing vmnet packet I/O for interface {iface_id}"
            )));
        };

        let VmnetVirtioNetworkPacketIo {
            tx_sink, rx_source, ..
        } = &mut entry.packet_io;
        Ok(Arm64BootNetworkPacketIo::new(tx_sink, rx_source))
    }
}

struct VmnetVirtioNetworkPacketIoState<B>
where
    B: VmnetPacketIoBackend,
{
    backend: B,
    interface: B::Interface,
}

impl<B> fmt::Debug for VmnetVirtioNetworkPacketIoState<B>
where
    B: VmnetPacketIoBackend,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetVirtioNetworkPacketIoState(<owned>)")
    }
}

pub struct VmnetVirtioNetworkTxPacketSink<B>
where
    B: VmnetPacketIoBackend,
{
    shared: Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
    mmds_detour: Option<MmdsPacketDetour>,
    staging_buffer: Vec<u8>,
    committed_ranges: Vec<Range<usize>>,
    committed_frames: Vec<StagedVmnetTxFrame>,
    committed_packet_count: usize,
    committed_emitted_len: usize,
    staged_frame: Option<StagedVmnetTxFrame>,
    maximum_packet_size: usize,
    maximum_batch_size: usize,
    packet_envelope: VirtioNetworkPacketEnvelope,
    guest_mac: Option<GuestMacAddress>,
    backend_metrics: VirtioNetworkBackendMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedVmnetTxPacketKind {
    External,
    MmdsDetour,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StagedVmnetTxFrame {
    packet: VirtioNetworkPacketPlan,
    packet_count: usize,
    emitted_len: usize,
    kind: StagedVmnetTxPacketKind,
}

impl<B> fmt::Debug for VmnetVirtioNetworkTxPacketSink<B>
where
    B: VmnetPacketIoBackend,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetVirtioNetworkTxPacketSink")
            .field("shared", &"<owned>")
            .field(
                "mmds_detour",
                &self.mmds_detour.as_ref().map(|_| "<configured>"),
            )
            .field("committed_packets", &self.committed_packet_count)
            .field("committed_frames", &self.committed_frames.len())
            .field("staged_frame", &self.staged_frame.is_some())
            .field("packet_envelope", &self.packet_envelope)
            .field("guest_mac", &self.guest_mac.map(|_| "<configured>"))
            .finish()
    }
}

impl<B> VmnetVirtioNetworkTxPacketSink<B>
where
    B: VmnetPacketIoBackend,
{
    fn prepare_packet_plan(
        &self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkPacketPlan, VirtioNetworkTxPacketSinkError> {
        frame
            .prepare_packet(memory)
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))
    }

    fn preflight_packet_plan(
        &self,
        plan: &VirtioNetworkPacketPlan,
    ) -> Result<(usize, usize, StagedVmnetTxPacketKind), VirtioNetworkTxPacketSinkError> {
        let packet_count = plan
            .emitted_packet_count()
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        let emitted_len = plan
            .emitted_len(self.packet_envelope)
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        let mut kind = None;
        let visit = plan
            .visit_packets(self.packet_envelope, |packet| {
                if packet.is_empty() || packet.len() > self.maximum_packet_size {
                    return ControlFlow::Break(VirtioNetworkTxPacketSinkError::new(
                        "vmnet TX normalized packet exceeds the realized per-packet bound",
                    ));
                }
                let packet = match ethernet_packet_for_envelope(self.packet_envelope, packet) {
                    Ok(packet) => packet,
                    Err(source) => return ControlFlow::Break(source),
                };
                let current = if self
                    .mmds_detour
                    .as_ref()
                    .is_some_and(|detour| detour.would_detour_packet(packet))
                {
                    StagedVmnetTxPacketKind::MmdsDetour
                } else {
                    StagedVmnetTxPacketKind::External
                };
                if kind.is_some_and(|previous| previous != current) {
                    return ControlFlow::Break(VirtioNetworkTxPacketSinkError::new(
                        "MMDS classification changed within one normalized TX frame",
                    ));
                }
                kind = Some(current);
                ControlFlow::Continue(())
            })
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        if let ControlFlow::Break(source) = visit {
            return Err(source);
        }
        let kind = kind.ok_or_else(|| {
            VirtioNetworkTxPacketSinkError::new("vmnet TX normalization emitted no packet")
        })?;
        Ok((packet_count, emitted_len, kind))
    }

    fn should_flush_before_staging(
        &self,
        packet_count: usize,
        emitted_len: usize,
    ) -> Result<bool, VirtioNetworkTxPacketSinkError> {
        let required_packets = self
            .committed_packet_count
            .checked_add(packet_count)
            .ok_or_else(|| {
                VirtioNetworkTxPacketSinkError::new("vmnet TX batch count overflowed")
            })?;
        let required_len = self
            .committed_emitted_len
            .checked_add(emitted_len)
            .ok_or_else(|| VirtioNetworkTxPacketSinkError::new("vmnet TX batch size overflowed"))?;
        Ok(!self.committed_frames.is_empty()
            && (required_packets > self.maximum_batch_size
                || required_len > VMNET_MAX_BYTES_PER_OPERATION))
    }

    fn install_staged_plan(
        &mut self,
        packet: VirtioNetworkPacketPlan,
        packet_count: usize,
        emitted_len: usize,
        kind: StagedVmnetTxPacketKind,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        if kind == StagedVmnetTxPacketKind::External {
            self.committed_frames.try_reserve(1).map_err(|source| {
                VirtioNetworkTxPacketSinkError::new(format!(
                    "failed to reserve vmnet TX frame metadata: {source}"
                ))
            })?;
        }
        if let (Some(expected), Some(observed)) = (self.guest_mac, packet.source_mac())
            && expected.octets() != observed
        {
            self.backend_metrics.record_spoofed_mac();
        }
        self.staged_frame = Some(StagedVmnetTxFrame {
            packet,
            packet_count,
            emitted_len,
            kind,
        });
        Ok(VirtioNetworkTxPacketStage::Staged {
            flush_before_commit: kind == StagedVmnetTxPacketKind::MmdsDetour,
        })
    }

    fn stage_owned_plan(
        &mut self,
        packet: VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        if self.staged_frame.is_some() {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet TX staging already owns an uncommitted frame",
            ));
        }
        let (packet_count, emitted_len, kind) = self.preflight_packet_plan(&packet)?;
        if self.should_flush_before_staging(packet_count, emitted_len)? {
            return Ok(VirtioNetworkTxPacketStage::FlushRequired);
        }
        self.install_staged_plan(packet, packet_count, emitted_len, kind)
    }

    fn stage_borrowed_plan(
        &mut self,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        if self.staged_frame.is_some() {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet TX staging already owns an uncommitted frame",
            ));
        }
        let (packet_count, emitted_len, kind) = self.preflight_packet_plan(packet)?;
        if self.should_flush_before_staging(packet_count, emitted_len)? {
            return Ok(VirtioNetworkTxPacketStage::FlushRequired);
        }
        let packet = packet
            .try_clone_owned()
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        self.install_staged_plan(packet, packet_count, emitted_len, kind)
    }

    fn detour_packet_plan(
        &mut self,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let Some(detour) = self.mmds_detour.as_mut() else {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet TX MMDS staging lost its detour owner",
            ));
        };
        let envelope = self.packet_envelope;
        let visit = packet
            .visit_packets(envelope, |packet| {
                let packet = match ethernet_packet_for_envelope(envelope, packet) {
                    Ok(packet) => packet,
                    Err(source) => return ControlFlow::Break(source),
                };
                match detour.detour_packet(packet).map_err(tx_mmds_detour_error) {
                    Ok(true) => ControlFlow::Continue(()),
                    Ok(false) => ControlFlow::Break(VirtioNetworkTxPacketSinkError::new(
                        "MMDS side-effect classification changed at commit",
                    )),
                    Err(source) => ControlFlow::Break(source),
                }
            })
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        match visit {
            ControlFlow::Continue(()) => Ok(VirtioNetworkTxPacketDisposition::Detoured),
            ControlFlow::Break(source) => Err(source),
        }
    }

    fn transmit_owned_plan(
        &mut self,
        packet: VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        if self.staged_frame.is_some() || !self.committed_frames.is_empty() {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet immediate TX cannot overlap staged batch ownership",
            ));
        }
        match self.stage_owned_plan(packet)? {
            VirtioNetworkTxPacketStage::Staged { .. } => {}
            VirtioNetworkTxPacketStage::FlushRequired => {
                return Err(VirtioNetworkTxPacketSinkError::new(
                    "vmnet immediate TX unexpectedly required a prior flush",
                ));
            }
        }
        match self.commit_staged_frame() {
            VirtioNetworkTxPacketCommit::Immediate(result) => result,
            VirtioNetworkTxPacketCommit::Deferred => {
                let mut results = Vec::new();
                results.try_reserve_exact(1).map_err(|source| {
                    VirtioNetworkTxPacketSinkError::new(format!(
                        "failed to reserve vmnet TX immediate result: {source}"
                    ))
                })?;
                self.flush_staged_frames(&mut results);
                results.into_iter().next().unwrap_or_else(|| {
                    Err(VirtioNetworkTxPacketSinkError::new(
                        "vmnet immediate TX produced no frame result",
                    ))
                })
            }
        }
    }
}

struct VmnetTxBatchWriter<'a, B>
where
    B: VmnetPacketIoBackend,
{
    backend: &'a mut B,
    interface: &'a mut B::Interface,
    buffer: &'a mut Vec<u8>,
    ranges: &'a mut Vec<Range<usize>>,
    maximum_packet_size: usize,
    maximum_batch_size: usize,
    completed_packets: usize,
    metrics: VirtioNetworkBackendMetrics,
}

impl<B> VmnetTxBatchWriter<'_, B>
where
    B: VmnetPacketIoBackend,
{
    fn push_packet(&mut self, packet: &[u8]) -> Result<(), VirtioNetworkTxPacketSinkError> {
        if packet.is_empty() || packet.len() > self.maximum_packet_size {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet TX normalized packet exceeds the realized per-packet bound",
            ));
        }
        let required_len =
            self.buffer.len().checked_add(packet.len()).ok_or_else(|| {
                VirtioNetworkTxPacketSinkError::new("vmnet TX batch size overflowed")
            })?;
        if !self.ranges.is_empty()
            && (self.ranges.len() == self.maximum_batch_size
                || required_len > VMNET_MAX_BYTES_PER_OPERATION)
        {
            self.flush()?;
        }
        if packet.len() > VMNET_MAX_BYTES_PER_OPERATION {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet TX normalized packet exceeds the operation byte bound",
            ));
        }
        self.buffer.try_reserve(packet.len()).map_err(|source| {
            VirtioNetworkTxPacketSinkError::new(format!(
                "failed to reserve vmnet TX batch bytes: {source}"
            ))
        })?;
        self.ranges.try_reserve(1).map_err(|source| {
            VirtioNetworkTxPacketSinkError::new(format!(
                "failed to reserve vmnet TX batch metadata: {source}"
            ))
        })?;
        let start = self.buffer.len();
        let end = start.checked_add(packet.len()).ok_or_else(|| {
            VirtioNetworkTxPacketSinkError::new("vmnet TX packet range overflowed")
        })?;
        self.buffer.extend_from_slice(packet);
        self.ranges.push(start..end);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), VirtioNetworkTxPacketSinkError> {
        if self.ranges.is_empty() {
            return Ok(());
        }
        let requested = self.ranges.len();
        let started = Instant::now();
        let write_result =
            self.backend
                .write_packet_batch(self.interface, self.buffer, self.ranges);
        let duration = started.elapsed();
        let completed = match write_result {
            Ok(completed) if completed <= requested => {
                self.metrics
                    .record_vmnet_write(requested, Ok(completed), duration);
                completed
            }
            Ok(_) => {
                self.metrics
                    .record_vmnet_write(requested, Err(()), duration);
                self.buffer.clear();
                self.ranges.clear();
                return Err(VirtioNetworkTxPacketSinkError::new(
                    "vmnet TX batch returned an out-of-range completed count",
                ));
            }
            Err(source) => {
                self.metrics
                    .record_vmnet_write(requested, Err(()), duration);
                self.buffer.clear();
                self.ranges.clear();
                return Err(tx_vmnet_error(source));
            }
        };
        self.buffer.clear();
        self.ranges.clear();
        self.completed_packets =
            self.completed_packets
                .checked_add(completed)
                .ok_or_else(|| {
                    VirtioNetworkTxPacketSinkError::new(
                        "vmnet TX completed packet count overflowed",
                    )
                })?;
        if completed != requested {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet TX batch completed only a prefix; the suffix was not retried",
            ));
        }
        Ok(())
    }

    fn discard_pending(&mut self) {
        self.buffer.clear();
        self.ranges.clear();
    }
}

impl<B> VirtioNetworkTxPacketSink for VmnetVirtioNetworkTxPacketSink<B>
where
    B: VmnetPacketIoBackend,
{
    fn transmit_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let packet = self.prepare_packet_plan(memory, frame)?;
        self.transmit_owned_plan(packet)
    }

    fn transmit_prepared_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        let packet = packet
            .try_clone_owned()
            .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
        self.transmit_owned_plan(packet)
    }

    fn supports_staged_batch(&self) -> bool {
        true
    }

    fn stage_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        let packet = self.prepare_packet_plan(memory, frame)?;
        self.stage_owned_plan(packet)
    }

    fn stage_prepared_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
        packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        self.stage_borrowed_plan(packet)
    }

    fn commit_staged_frame(&mut self) -> VirtioNetworkTxPacketCommit {
        let Some(staged) = self.staged_frame.take() else {
            return VirtioNetworkTxPacketCommit::Immediate(Err(
                VirtioNetworkTxPacketSinkError::new("vmnet TX commit has no staged frame"),
            ));
        };
        match staged.kind {
            StagedVmnetTxPacketKind::External => {
                let Some(committed_packet_count) =
                    self.committed_packet_count.checked_add(staged.packet_count)
                else {
                    return VirtioNetworkTxPacketCommit::Immediate(Err(
                        VirtioNetworkTxPacketSinkError::new(
                            "vmnet TX committed packet count overflowed",
                        ),
                    ));
                };
                let Some(committed_emitted_len) =
                    self.committed_emitted_len.checked_add(staged.emitted_len)
                else {
                    return VirtioNetworkTxPacketCommit::Immediate(Err(
                        VirtioNetworkTxPacketSinkError::new(
                            "vmnet TX committed byte count overflowed",
                        ),
                    ));
                };
                self.committed_packet_count = committed_packet_count;
                self.committed_emitted_len = committed_emitted_len;
                self.committed_frames.push(staged);
                VirtioNetworkTxPacketCommit::Deferred
            }
            StagedVmnetTxPacketKind::MmdsDetour => {
                let result = self.detour_packet_plan(&staged.packet);
                VirtioNetworkTxPacketCommit::Immediate(result)
            }
        }
    }

    fn discard_staged_frame(&mut self) {
        self.staged_frame = None;
    }

    fn flush_staged_frames(
        &mut self,
        results: &mut Vec<Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError>>,
    ) {
        if self.committed_frames.is_empty() {
            return;
        }
        let shared = Arc::clone(&self.shared);
        let mut completed_packets = 0_usize;
        let mut backend_metrics = VirtioNetworkBackendMetrics::default();
        let write_result = lock_state_for_tx(&shared).and_then(|mut state| {
            let VmnetVirtioNetworkPacketIoState { backend, interface } = &mut *state;
            let mut writer = VmnetTxBatchWriter {
                backend,
                interface,
                buffer: &mut self.staging_buffer,
                ranges: &mut self.committed_ranges,
                maximum_packet_size: self.maximum_packet_size,
                maximum_batch_size: self.maximum_batch_size,
                completed_packets: 0,
                metrics: VirtioNetworkBackendMetrics::default(),
            };
            let mut result = Ok(());
            for frame in &self.committed_frames {
                let visit =
                    frame.packet.visit_packets(self.packet_envelope, |packet| {
                        match writer.push_packet(packet) {
                            Ok(()) => ControlFlow::Continue(()),
                            Err(source) => ControlFlow::Break(source),
                        }
                    });
                match visit {
                    Ok(ControlFlow::Continue(())) => {}
                    Ok(ControlFlow::Break(source)) => {
                        result = Err(source);
                        break;
                    }
                    Err(source) => {
                        result = Err(VirtioNetworkTxPacketSinkError::new(source.to_string()));
                        break;
                    }
                }
            }
            if result.is_ok()
                && let Err(source) = writer.flush()
            {
                result = Err(source);
            }
            if result.is_err() {
                writer.discard_pending();
            }
            completed_packets = writer.completed_packets;
            backend_metrics = writer.metrics;
            result
        });
        self.backend_metrics = self.backend_metrics.merged_with(backend_metrics);
        let first_failure = write_result.err();
        let mut frame_end = 0_usize;
        for frame in &self.committed_frames {
            frame_end = frame_end.saturating_add(frame.packet_count);
            let succeeded = frame_end <= completed_packets;
            if succeeded {
                results.push(Ok(VirtioNetworkTxPacketDisposition::Forwarded));
            } else {
                results.push(Err(first_failure.clone().unwrap_or_else(|| {
                    VirtioNetworkTxPacketSinkError::new(
                        "vmnet TX normalized frame did not complete",
                    )
                })));
            }
        }
        self.committed_frames.clear();
        self.committed_packet_count = 0;
        self.committed_emitted_len = 0;
        self.staging_buffer.clear();
        self.committed_ranges.clear();
    }

    fn take_backend_metrics(&mut self) -> VirtioNetworkBackendMetrics {
        std::mem::take(&mut self.backend_metrics)
    }
}

pub struct VmnetVirtioNetworkRxPacketSource<B>
where
    B: VmnetPacketIoBackend,
{
    shared: Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
    read_buffer: Vec<u8>,
    packet_lengths: Vec<usize>,
    packet_capacity: usize,
    read_batch_size: usize,
    cached_packet_index: usize,
    cached_packet_count: usize,
    cached_packet_source: Option<CachedRxPacketSource>,
    host_batch_attempted: Option<bool>,
    mmds_stack: Option<MmdsNetworkStackHandle>,
    readiness: Option<VmnetPacketReadinessConsumer>,
    packet_envelope: VirtioNetworkPacketEnvelope,
    backend_metrics: VirtioNetworkBackendMetrics,
}

impl<B> fmt::Debug for VmnetVirtioNetworkRxPacketSource<B>
where
    B: VmnetPacketIoBackend,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetVirtioNetworkRxPacketSource")
            .field("shared", &"<owned>")
            .field("read_buffer", &"<owned>")
            .field(
                "cached_packet_count",
                &self
                    .cached_packet_count
                    .saturating_sub(self.cached_packet_index),
            )
            .field(
                "mmds_stack",
                &self.mmds_stack.as_ref().map(|_| "<configured>"),
            )
            .field("packet_envelope", &self.packet_envelope)
            .finish()
    }
}

impl<B> VmnetVirtioNetworkRxPacketSource<B>
where
    B: VmnetPacketIoBackend,
{
    fn cached_packet(&self) -> Option<VirtioNetworkRxPacket<'_>> {
        if self.cached_packet_index >= self.cached_packet_count {
            return None;
        }
        let len = *self.packet_lengths.get(self.cached_packet_index)?;
        let source = self.cached_packet_source?;
        let packet_start = match source {
            CachedRxPacketSource::MmdsResponse => 0,
            CachedRxPacketSource::Vmnet => {
                self.cached_packet_index.checked_mul(self.packet_capacity)?
            }
        };
        let envelope_len = if source == CachedRxPacketSource::Vmnet {
            self.packet_envelope.header_len()
        } else {
            0
        };
        let start = packet_start.checked_add(envelope_len)?;
        let end = packet_start.checked_add(len)?;
        self.read_buffer
            .get(start..end)
            .map(VirtioNetworkRxPacket::new)
    }

    fn has_host_readiness(&self) -> bool {
        self.cached_packet_index < self.cached_packet_count
            || self
                .readiness
                .as_ref()
                .is_some_and(|readiness| readiness.state.is_ready())
    }

    fn has_mmds_readiness(&self) -> bool {
        self.cached_packet_source == Some(CachedRxPacketSource::MmdsResponse)
            || self
                .mmds_stack
                .as_ref()
                .is_some_and(MmdsNetworkStackHandle::has_ready_frame)
    }

    fn mmds_retry_after(&self) -> Option<Duration> {
        if self.cached_packet_source == Some(CachedRxPacketSource::MmdsResponse) {
            return None;
        }
        self.mmds_stack
            .as_ref()
            .and_then(MmdsNetworkStackHandle::retry_after)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CachedRxPacketSource {
    MmdsResponse,
    Vmnet,
}

impl<B> VirtioNetworkRxPacketSource for VmnetVirtioNetworkRxPacketSource<B>
where
    B: VmnetPacketIoBackend,
{
    fn begin_rx_dispatch(&mut self) {
        self.host_batch_attempted = Some(false);
    }

    fn host_readiness_hint(&self) -> bool {
        self.has_host_readiness()
    }

    fn retry_after_tx_hint(&self) -> bool {
        self.has_host_readiness() || self.has_mmds_readiness()
    }

    fn peek_packet(
        &mut self,
    ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
        if self.cached_packet_index < self.cached_packet_count {
            return Ok(self.cached_packet());
        }

        if let Some(mmds_stack) = &self.mmds_stack
            && let Some(len) = mmds_stack
                .copy_next_frame_into(
                    self.read_buffer
                        .get_mut(..self.packet_capacity)
                        .ok_or_else(|| {
                            VirtioNetworkRxPacketSourceError::new(
                                "vmnet RX packet buffer is smaller than its realized capacity",
                            )
                        })?,
                )
                .map_err(rx_mmds_stack_error)?
        {
            let packet_len = self.packet_lengths.first_mut().ok_or_else(|| {
                VirtioNetworkRxPacketSourceError::new(
                    "vmnet RX packet length storage is unexpectedly empty",
                )
            })?;
            *packet_len = len;
            self.cached_packet_index = 0;
            self.cached_packet_count = 1;
            self.cached_packet_source = Some(CachedRxPacketSource::MmdsResponse);
            return Ok(self.cached_packet());
        }

        if self.host_batch_attempted == Some(true) {
            return Ok(None);
        }
        if let Some(host_batch_attempted) = self.host_batch_attempted.as_mut() {
            *host_batch_attempted = true;
        }

        let event_snapshot = self
            .readiness
            .as_ref()
            .map_or(0, |readiness| readiness.state.snapshot());
        let estimated_packets = self.readiness.as_ref().map_or(1, |readiness| {
            readiness
                .state
                .estimated_packets
                .load(Ordering::Acquire)
                .clamp(1, self.read_batch_size)
        });
        let started = Instant::now();
        let read_result = {
            let mut state = lock_state_for_rx(&self.shared)?;
            let VmnetVirtioNetworkPacketIoState { backend, interface } = &mut *state;

            backend.read_packet_batch(
                interface,
                &mut self.read_buffer,
                self.packet_capacity,
                estimated_packets,
                &mut self.packet_lengths,
            )
        };
        let duration = started.elapsed();
        let packet_count = match read_result {
            Ok(packet_count) => packet_count,
            Err(source) => {
                self.backend_metrics
                    .record_vmnet_read(estimated_packets, Err(()), duration);
                return Err(rx_vmnet_error(source));
            }
        };
        let validation = (|| {
            if packet_count > estimated_packets {
                return Err(rx_vmnet_error(VmnetPacketIoError::InvalidBatch {
                    message: "read backend returned more packets than requested",
                }));
            }
            for (packet_index, packet_len) in self
                .packet_lengths
                .iter()
                .copied()
                .take(packet_count)
                .enumerate()
            {
                validate_rx_packet_len(packet_len, self.packet_capacity)?;
                if self.packet_envelope == VirtioNetworkPacketEnvelope::DirectVirtioHeader {
                    validate_direct_rx_packet(
                        &self.read_buffer,
                        self.packet_capacity,
                        packet_index,
                        packet_len,
                    )?;
                }
            }
            Ok(())
        })();
        if let Err(source) = validation {
            self.backend_metrics
                .record_vmnet_read(estimated_packets, Err(()), duration);
            return Err(source);
        }
        self.backend_metrics
            .record_vmnet_read(estimated_packets, Ok(packet_count), duration);
        if let Some(readiness) = &self.readiness {
            if packet_count < estimated_packets {
                readiness.state.clear_if_unchanged(event_snapshot);
            } else if packet_count != 0 {
                readiness.state.retain_ready();
            }
        }
        self.cached_packet_index = 0;
        self.cached_packet_count = packet_count;
        self.cached_packet_source = (packet_count != 0).then_some(CachedRxPacketSource::Vmnet);
        if packet_count == 0
            && let Some(readiness) = &self.readiness
        {
            readiness.state.schedule_if_ready();
        }

        Ok(self.cached_packet())
    }

    fn consume_packet(&mut self) {
        if self.cached_packet_index >= self.cached_packet_count {
            return;
        }
        let Some(packet_len) = self.packet_lengths.get(self.cached_packet_index).copied() else {
            return;
        };
        let source = self.cached_packet_source;
        self.cached_packet_index += 1;
        if source == Some(CachedRxPacketSource::MmdsResponse)
            && let Some(mmds_stack) = &self.mmds_stack
        {
            let _ = mmds_stack.consume_frame(packet_len);
        }
        if self.cached_packet_index == self.cached_packet_count {
            self.cached_packet_index = 0;
            self.cached_packet_count = 0;
            self.cached_packet_source = None;
            if source == Some(CachedRxPacketSource::Vmnet)
                && let Some(readiness) = &self.readiness
            {
                readiness.state.schedule_if_ready();
            }
        }
    }

    fn take_backend_metrics(&mut self) -> VirtioNetworkBackendMetrics {
        std::mem::take(&mut self.backend_metrics)
    }
}

fn ethernet_packet_for_envelope(
    envelope: VirtioNetworkPacketEnvelope,
    packet: &[u8],
) -> Result<&[u8], VirtioNetworkTxPacketSinkError> {
    match envelope {
        VirtioNetworkPacketEnvelope::RawEthernet => Ok(packet),
        VirtioNetworkPacketEnvelope::DirectVirtioHeader => {
            let header_len = VIRTIO_NET_TX_HEADER_SIZE as usize;
            let header = packet.get(..header_len).ok_or_else(|| {
                VirtioNetworkTxPacketSinkError::new(
                    "vmnet direct TX packet is missing its virtio header",
                )
            })?;
            if header.iter().any(|byte| *byte != 0) {
                return Err(VirtioNetworkTxPacketSinkError::new(
                    "vmnet direct TX packet has a noncanonical virtio header",
                ));
            }
            packet.get(header_len..).ok_or_else(|| {
                VirtioNetworkTxPacketSinkError::new(
                    "vmnet direct TX packet is missing its Ethernet frame",
                )
            })
        }
    }
}

fn lock_state_for_tx<B>(
    shared: &Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
) -> Result<MutexGuard<'_, VmnetVirtioNetworkPacketIoState<B>>, VirtioNetworkTxPacketSinkError>
where
    B: VmnetPacketIoBackend,
{
    shared.lock().map_err(|_| {
        VirtioNetworkTxPacketSinkError::new("vmnet virtio-net packet state lock poisoned during TX")
    })
}

fn lock_state_for_rx<B>(
    shared: &Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
) -> Result<MutexGuard<'_, VmnetVirtioNetworkPacketIoState<B>>, VirtioNetworkRxPacketSourceError>
where
    B: VmnetPacketIoBackend,
{
    shared.lock().map_err(|_| {
        VirtioNetworkRxPacketSourceError::new(
            "vmnet virtio-net packet state lock poisoned during RX",
        )
    })
}

fn tx_vmnet_error(source: VmnetPacketIoError) -> VirtioNetworkTxPacketSinkError {
    VirtioNetworkTxPacketSinkError::new(format!("vmnet TX packet write failed: {source}"))
}

fn tx_mmds_detour_error(source: MmdsNetworkStackError) -> VirtioNetworkTxPacketSinkError {
    VirtioNetworkTxPacketSinkError::new(format!("MMDS packet detour failed: {source}"))
}

fn rx_vmnet_error(source: VmnetPacketIoError) -> VirtioNetworkRxPacketSourceError {
    VirtioNetworkRxPacketSourceError::new(format!("vmnet RX packet read failed: {source}"))
}

fn rx_mmds_stack_error(source: MmdsNetworkStackError) -> VirtioNetworkRxPacketSourceError {
    VirtioNetworkRxPacketSourceError::new(format!("MMDS network stack read failed: {source}"))
}

fn validate_rx_packet_len(
    packet_len: usize,
    buffer_len: usize,
) -> Result<(), VirtioNetworkRxPacketSourceError> {
    if packet_len == 0 {
        return Err(rx_vmnet_error(VmnetPacketIoError::InvalidBatch {
            message: "read backend returned an empty packet",
        }));
    }
    if packet_len <= buffer_len {
        return Ok(());
    }

    Err(rx_vmnet_error(
        VmnetPacketIoError::ReadPacketSizeExceedsBuffer {
            packet_size: packet_len,
            buffer_len,
        },
    ))
}

fn validate_direct_rx_packet(
    buffer: &[u8],
    packet_capacity: usize,
    packet_index: usize,
    packet_len: usize,
) -> Result<(), VirtioNetworkRxPacketSourceError> {
    let header_len = VIRTIO_NET_TX_HEADER_SIZE as usize;
    if packet_len <= header_len {
        return Err(VirtioNetworkRxPacketSourceError::new(
            "vmnet direct RX packet does not contain an Ethernet frame",
        ));
    }
    let start = packet_index.checked_mul(packet_capacity).ok_or_else(|| {
        VirtioNetworkRxPacketSourceError::new("vmnet direct RX packet offset overflowed")
    })?;
    let header_end = start.checked_add(header_len).ok_or_else(|| {
        VirtioNetworkRxPacketSourceError::new("vmnet direct RX header range overflowed")
    })?;
    let header = buffer.get(start..header_end).ok_or_else(|| {
        VirtioNetworkRxPacketSourceError::new("vmnet direct RX header is outside its buffer")
    })?;
    if header.iter().any(|byte| *byte != 0) {
        return Err(VirtioNetworkRxPacketSourceError::new(
            "vmnet direct RX header requests unsupported offload semantics",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::net::Ipv4Addr;
    use std::ptr;
    use std::sync::Arc;
    use std::sync::mpsc::{TryRecvError, sync_channel};

    use bangbang_runtime::fdt::{Arm64FdtRegion, Arm64FdtVirtioMmioDevice};
    use bangbang_runtime::interrupt::GuestInterruptLine;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::metrics::SharedMmdsMetrics;
    use bangbang_runtime::mmds::{
        DEFAULT_MMDS_IPV4_ADDRESS, DEFAULT_MMDS_MAC_ADDRESS, MmdsStateHandle,
    };
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::{
        GuestMacAddress, NetworkInterfaceConfigInput, NetworkInterfaceConfigs, NetworkMmioLayout,
        PreparedNetworkDevices, VIRTIO_NET_F_CSUM, VIRTIO_NET_F_HOST_TSO4,
        VIRTIO_NET_HDR_F_NEEDS_CSUM, VIRTIO_NET_HDR_GSO_TCPV4, VIRTIO_NET_TX_HEADER_SIZE,
        VirtioNetworkRxPacketSource, VirtioNetworkTxFrame, VirtioNetworkTxHeader,
        VirtioNetworkTxPacketCommit, VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSink,
        VirtioNetworkTxPacketStage,
    };
    use bangbang_runtime::network_packet::VirtioNetworkPacketPlan;
    use bangbang_runtime::startup::{Arm64BootNetworkDevice, Arm64BootNetworkPacketIoProvider};
    use bangbang_runtime::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESCRIPTOR_SIZE, read_descriptor_chain,
    };

    use super::{
        MmdsPacketDetour, VmnetPacketIoBackend, VmnetPacketIoError, VmnetReadPacket,
        VmnetVirtioNetworkPacketIo, VmnetVirtioNetworkPacketIoBuildError,
        VmnetVirtioNetworkPacketIoProvider, VmnetVirtioNetworkPacketIoProviderBuildError,
        VmnetVirtioNetworkPacketIoProviderEntry, VmnetWritePacket,
    };
    use crate::host_network::vmnet::{VmnetOperation, VmnetPacketCountExpectation};

    const DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x1000);
    const HEADER_ADDRESS: GuestAddress = GuestAddress::new(0x2000);
    const PAYLOAD_ADDRESS: GuestAddress = GuestAddress::new(0x3000);
    const SECOND_PAYLOAD_ADDRESS: GuestAddress = GuestAddress::new(0x4000);
    const ETHERNET_HEADER_LEN: usize = 14;
    const ETHERNET_ETHERTYPE_ARP: u16 = 0x0806;
    const ETHERNET_ETHERTYPE_IPV4: u16 = 0x0800;
    const IPV4_HEADER_LEN: usize = 20;
    const TCP_HEADER_LEN: usize = 20;
    const ARP_HARDWARE_TYPE_ETHERNET: u16 = 1;
    const ARP_OPERATION_REQUEST: u16 = 1;
    const ARP_OPERATION_REPLY: u16 = 2;
    const ARP_PROTOCOL_TYPE_IPV4: u16 = ETHERNET_ETHERTYPE_IPV4;
    const ARP_OPERATION_OFFSET: usize = 6;
    const ARP_TARGET_PROTOCOL_ADDRESS_OFFSET: usize = 24;
    const TEST_SOURCE_IPV4_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);
    const TEST_SOURCE_TCP_PORT: u16 = 49_152;
    const TEST_SOURCE_ETHERNET_ADDRESS: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
    const TEST_DESTINATION_ETHERNET_ADDRESS: [u8; 6] = [0xff; 6];

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeInterface;

    #[derive(Debug, Default)]
    struct FakeVmnetPacketIoBackend {
        write_error: Option<VmnetPacketIoError>,
        write_batch_completed: Option<usize>,
        read_results: VecDeque<Result<Option<Vec<u8>>, VmnetPacketIoError>>,
        written_packets: Vec<Vec<u8>>,
        read_calls: usize,
        read_batch_calls: usize,
        read_batch_requests: Vec<usize>,
        write_calls: usize,
        write_batch_calls: usize,
        write_batch_requests: Vec<usize>,
        write_batches: Vec<Vec<Vec<u8>>>,
    }

    impl FakeVmnetPacketIoBackend {
        fn with_write_error(mut self, error: VmnetPacketIoError) -> Self {
            self.write_error = Some(error);
            self
        }

        fn with_read_result(mut self, result: Result<Option<Vec<u8>>, VmnetPacketIoError>) -> Self {
            self.read_results.push_back(result);
            self
        }

        fn with_write_batch_completed(mut self, completed: usize) -> Self {
            self.write_batch_completed = Some(completed);
            self
        }
    }

    impl VmnetPacketIoBackend for FakeVmnetPacketIoBackend {
        type Interface = FakeInterface;

        fn read_packet(
            &mut self,
            _interface: &mut Self::Interface,
            packet: &mut VmnetReadPacket<'_>,
        ) -> Result<Option<usize>, VmnetPacketIoError> {
            self.read_calls += 1;
            let Some(result) = self.read_results.pop_front() else {
                return Ok(None);
            };
            let Some(bytes) = result? else {
                return Ok(None);
            };
            let len = bytes.len();
            assert!(len <= packet.iov().iov_len);

            // SAFETY: `VmnetReadPacket` owns an iovec pointing at the live
            // read buffer borrowed by the adapter for this synchronous call.
            unsafe {
                ptr::copy_nonoverlapping(bytes.as_ptr(), packet.iov().iov_base.cast::<u8>(), len);
            }

            Ok(Some(len))
        }

        fn write_packet(
            &mut self,
            _interface: &mut Self::Interface,
            packet: &mut VmnetWritePacket<'_>,
        ) -> Result<(), VmnetPacketIoError> {
            self.write_calls += 1;
            if let Some(error) = self.write_error.clone() {
                return Err(error);
            }

            let descriptor = packet.as_raw_descriptor();
            assert_eq!(descriptor.iov_count(), 1);
            assert!(!descriptor.iov_ptr().is_null());

            // SAFETY: `VmnetWritePacket` owns an iovec pointing at the packet
            // bytes borrowed by the adapter for this synchronous call.
            let iov = unsafe { &*descriptor.iov_ptr() };
            // SAFETY: The iovec base and length were created from a live packet
            // slice and are valid for this call.
            let bytes =
                unsafe { std::slice::from_raw_parts(iov.iov_base.cast::<u8>(), iov.iov_len) };
            self.written_packets.push(bytes.to_vec());

            Ok(())
        }

        fn read_packet_batch(
            &mut self,
            interface: &mut Self::Interface,
            buffer: &mut [u8],
            packet_capacity: usize,
            requested_packets: usize,
            packet_lengths: &mut [usize],
        ) -> Result<usize, VmnetPacketIoError> {
            self.read_batch_calls += 1;
            self.read_batch_requests.push(requested_packets);
            let mut completed = 0;
            for (packet_index, packet_len_slot) in packet_lengths
                .iter_mut()
                .take(requested_packets)
                .enumerate()
            {
                let start = packet_index
                    .checked_mul(packet_capacity)
                    .expect("test packet offset should not overflow");
                let end = start
                    .checked_add(packet_capacity)
                    .expect("test packet range should not overflow");
                let mut packet = VmnetReadPacket::new(
                    buffer
                        .get_mut(start..end)
                        .expect("test aggregate buffer should contain packet slot"),
                )
                .expect("test read descriptor should build");
                let Some(packet_len) = self.read_packet(interface, &mut packet)? else {
                    break;
                };
                *packet_len_slot = packet_len;
                completed += 1;
            }
            Ok(completed)
        }

        fn write_packet_batch(
            &mut self,
            _interface: &mut Self::Interface,
            buffer: &[u8],
            packet_ranges: &[std::ops::Range<usize>],
        ) -> Result<usize, VmnetPacketIoError> {
            self.write_batch_calls += 1;
            self.write_batch_requests.push(packet_ranges.len());
            if let Some(error) = self.write_error.clone() {
                return Err(error);
            }
            let batch = packet_ranges
                .iter()
                .map(|range| {
                    buffer
                        .get(range.clone())
                        .expect("test write range should fit aggregate buffer")
                        .to_vec()
                })
                .collect::<Vec<_>>();
            self.write_batches.push(batch.clone());
            let completed = self.write_batch_completed.unwrap_or(packet_ranges.len());
            for packet in batch.into_iter().take(completed.min(packet_ranges.len())) {
                self.write_calls += 1;
                self.written_packets.push(packet);
            }
            Ok(completed)
        }
    }

    #[derive(Debug)]
    struct OversizedReadBackend {
        packet_len: usize,
    }

    impl VmnetPacketIoBackend for OversizedReadBackend {
        type Interface = FakeInterface;

        fn read_packet(
            &mut self,
            _interface: &mut Self::Interface,
            _packet: &mut VmnetReadPacket<'_>,
        ) -> Result<Option<usize>, VmnetPacketIoError> {
            Ok(Some(self.packet_len))
        }

        fn write_packet(
            &mut self,
            _interface: &mut Self::Interface,
            _packet: &mut VmnetWritePacket<'_>,
        ) -> Result<(), VmnetPacketIoError> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct PublishingReadBackend {
        readiness: Arc<super::VmnetPacketReadinessState>,
        read_batch_calls: usize,
    }

    impl VmnetPacketIoBackend for PublishingReadBackend {
        type Interface = FakeInterface;

        fn read_packet(
            &mut self,
            _interface: &mut Self::Interface,
            _packet: &mut VmnetReadPacket<'_>,
        ) -> Result<Option<usize>, VmnetPacketIoError> {
            Ok(None)
        }

        fn write_packet(
            &mut self,
            _interface: &mut Self::Interface,
            _packet: &mut VmnetWritePacket<'_>,
        ) -> Result<(), VmnetPacketIoError> {
            Ok(())
        }

        fn read_packet_batch(
            &mut self,
            _interface: &mut Self::Interface,
            _buffer: &mut [u8],
            _packet_capacity: usize,
            _requested_packets: usize,
            _packet_lengths: &mut [usize],
        ) -> Result<usize, VmnetPacketIoError> {
            self.read_batch_calls += 1;
            self.readiness.publish(Some(1));
            Ok(0)
        }
    }

    #[derive(Debug)]
    struct ExcessiveReadBatchCountBackend;

    impl VmnetPacketIoBackend for ExcessiveReadBatchCountBackend {
        type Interface = FakeInterface;

        fn read_packet(
            &mut self,
            _interface: &mut Self::Interface,
            _packet: &mut VmnetReadPacket<'_>,
        ) -> Result<Option<usize>, VmnetPacketIoError> {
            Ok(None)
        }

        fn write_packet(
            &mut self,
            _interface: &mut Self::Interface,
            _packet: &mut VmnetWritePacket<'_>,
        ) -> Result<(), VmnetPacketIoError> {
            Ok(())
        }

        fn read_packet_batch(
            &mut self,
            _interface: &mut Self::Interface,
            _buffer: &mut [u8],
            _packet_capacity: usize,
            requested_packets: usize,
            _packet_lengths: &mut [usize],
        ) -> Result<usize, VmnetPacketIoError> {
            Ok(requested_packets.saturating_add(1))
        }
    }

    fn fake_interface() -> FakeInterface {
        FakeInterface
    }

    fn tx_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x10_000)
                .expect("test memory range should be valid"),
        ])
        .expect("test memory layout should be valid");

        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    fn write_descriptor(
        memory: &mut GuestMemory,
        index: u16,
        address: GuestAddress,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let descriptor_address = GuestAddress::new(
            DESCRIPTOR_TABLE.raw_value() + (u64::from(index) * VIRTQUEUE_DESCRIPTOR_SIZE as u64),
        );
        let mut bytes = [0_u8; VIRTQUEUE_DESCRIPTOR_SIZE];
        bytes[0..8].copy_from_slice(&address.raw_value().to_le_bytes());
        bytes[8..12].copy_from_slice(&len.to_le_bytes());
        bytes[12..14].copy_from_slice(&flags.to_le_bytes());
        bytes[14..16].copy_from_slice(&next.to_le_bytes());
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("test descriptor should write");
    }

    fn tx_frame(
        memory: &mut GuestMemory,
        payloads: &[(&[u8], GuestAddress)],
    ) -> VirtioNetworkTxFrame {
        let header = [0_u8; VIRTIO_NET_TX_HEADER_SIZE as usize];
        memory
            .write_slice(&header, HEADER_ADDRESS)
            .expect("test TX header should write");
        write_descriptor(
            memory,
            0,
            HEADER_ADDRESS,
            VIRTIO_NET_TX_HEADER_SIZE,
            VIRTQUEUE_DESC_F_NEXT,
            1,
        );

        for (index, (payload, address)) in payloads.iter().enumerate() {
            memory
                .write_slice(payload, *address)
                .expect("test TX payload should write");
            let descriptor_index =
                u16::try_from(index + 1).expect("test descriptor index should fit u16");
            let next_index = descriptor_index
                .checked_add(1)
                .expect("test descriptor next index should fit u16");
            let has_next = index + 1 < payloads.len();
            let flags = if has_next { VIRTQUEUE_DESC_F_NEXT } else { 0 };
            write_descriptor(
                memory,
                descriptor_index,
                *address,
                u32::try_from(payload.len()).expect("test payload length should fit u32"),
                flags,
                next_index,
            );
        }

        let chain =
            read_descriptor_chain(memory, DESCRIPTOR_TABLE, 8, 0).expect("test chain should read");
        VirtioNetworkTxFrame::parse(memory, &chain).expect("test TX frame should parse")
    }

    fn tcp_ipv4_packet(
        destination_ipv4_address: Ipv4Addr,
        destination_port: u16,
        payload: &[u8],
    ) -> Vec<u8> {
        let ipv4_total_len = IPV4_HEADER_LEN + TCP_HEADER_LEN + payload.len();
        let packet_len = ETHERNET_HEADER_LEN + ipv4_total_len;
        let mut packet = vec![0_u8; packet_len];
        packet[12..14].copy_from_slice(&ETHERNET_ETHERTYPE_IPV4.to_be_bytes());
        let ipv4 = ETHERNET_HEADER_LEN;
        packet[ipv4] = 0x45;
        packet[ipv4 + 2..ipv4 + 4].copy_from_slice(
            &u16::try_from(ipv4_total_len)
                .expect("test IPv4 packet length should fit u16")
                .to_be_bytes(),
        );
        packet[ipv4 + 8] = 64;
        packet[ipv4 + 9] = 6;
        packet[ipv4 + 12..ipv4 + 16].copy_from_slice(&TEST_SOURCE_IPV4_ADDRESS.octets());
        packet[ipv4 + 16..ipv4 + 20].copy_from_slice(&destination_ipv4_address.octets());
        let tcp = ipv4 + IPV4_HEADER_LEN;
        packet[tcp..tcp + 2].copy_from_slice(&TEST_SOURCE_TCP_PORT.to_be_bytes());
        packet[tcp + 2..tcp + 4].copy_from_slice(&destination_port.to_be_bytes());
        packet[tcp + 12] = 5 << 4;
        packet[tcp + TCP_HEADER_LEN..].copy_from_slice(payload);
        packet
    }

    fn mmds_arp_request() -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&TEST_DESTINATION_ETHERNET_ADDRESS);
        packet.extend_from_slice(&TEST_SOURCE_ETHERNET_ADDRESS);
        packet.extend_from_slice(&ETHERNET_ETHERTYPE_ARP.to_be_bytes());
        packet.extend_from_slice(&ARP_HARDWARE_TYPE_ETHERNET.to_be_bytes());
        packet.extend_from_slice(&ARP_PROTOCOL_TYPE_IPV4.to_be_bytes());
        packet.push(6);
        packet.push(4);
        packet.extend_from_slice(&ARP_OPERATION_REQUEST.to_be_bytes());
        packet.extend_from_slice(&TEST_SOURCE_ETHERNET_ADDRESS);
        packet.extend_from_slice(&TEST_SOURCE_IPV4_ADDRESS.octets());
        packet.extend_from_slice(&[0; 6]);
        packet.extend_from_slice(&DEFAULT_MMDS_IPV4_ADDRESS.octets());
        packet
    }

    fn packet_io(
        backend: FakeVmnetPacketIoBackend,
    ) -> VmnetVirtioNetworkPacketIo<FakeVmnetPacketIoBackend> {
        VmnetVirtioNetworkPacketIo::with_rx_buffer_len(backend, fake_interface(), 2048)
            .expect("packet I/O should build")
    }

    fn batch_packet_io<B>(
        backend: B,
        packet_capacity: usize,
        read_batch_size: usize,
        write_batch_size: usize,
        mmds_detour: Option<MmdsPacketDetour>,
        readiness_lease: Option<super::VmnetPacketReadinessLease>,
    ) -> VmnetVirtioNetworkPacketIo<B>
    where
        B: VmnetPacketIoBackend<Interface = FakeInterface>,
    {
        VmnetVirtioNetworkPacketIo::with_prepared_batch_and_mmds_detour(
            backend,
            fake_interface(),
            super::PreparedVmnetVirtioNetworkRxBuffer::supported_maximum()
                .expect("test batch buffers should prepare"),
            super::VmnetVirtioNetworkBatchLimits::new(
                packet_capacity,
                read_batch_size,
                write_batch_size,
            ),
            mmds_detour,
            readiness_lease,
        )
        .expect("batch packet I/O should build")
    }

    fn provider_entry(
        iface_id: &str,
        backend: FakeVmnetPacketIoBackend,
    ) -> VmnetVirtioNetworkPacketIoProviderEntry<FakeVmnetPacketIoBackend> {
        VmnetVirtioNetworkPacketIoProviderEntry::new(iface_id, packet_io(backend))
    }

    fn network_device(iface_id: &str) -> Arm64BootNetworkDevice {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new(iface_id, iface_id, "tap0"))
            .expect("network config should insert");
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network device should prepare");
        let (_dispatcher, mut registrations) = prepared
            .register_mmio(NetworkMmioLayout::new(
                GuestAddress::new(0x6000_0000),
                MmioRegionId::new(1000),
            ))
            .expect("network MMIO should register")
            .into_parts();
        let registration = registrations
            .pop()
            .expect("one network registration should be present");

        Arm64BootNetworkDevice {
            registration,
            fdt_device: Arm64FdtVirtioMmioDevice {
                region: Arm64FdtRegion {
                    base: 0x6000_0000,
                    size: 0x1000,
                },
                interrupt_line: GuestInterruptLine::new(33)
                    .expect("test interrupt line should be valid"),
            },
        }
    }

    fn unexpected_count_error(operation: VmnetOperation) -> VmnetPacketIoError {
        VmnetPacketIoError::UnexpectedPacketCount {
            operation,
            expected: VmnetPacketCountExpectation::One,
            actual: 0,
        }
    }

    #[test]
    fn builds_packet_io_with_default_rx_buffer() {
        let mut packet_io =
            VmnetVirtioNetworkPacketIo::new(FakeVmnetPacketIoBackend::default(), fake_interface())
                .expect("default packet I/O should build");

        assert!(
            packet_io
                .rx_source()
                .peek_packet()
                .expect("peek should succeed")
                .is_none()
        );
    }

    #[test]
    fn rejects_empty_rx_buffer() {
        let error = VmnetVirtioNetworkPacketIo::with_rx_buffer_len(
            FakeVmnetPacketIoBackend::default(),
            fake_interface(),
            0,
        )
        .expect_err("empty RX buffer should fail");

        assert!(matches!(
            error,
            VmnetVirtioNetworkPacketIoBuildError::EmptyRxBuffer
        ));
    }

    #[test]
    fn prepared_rx_buffer_truncates_supported_storage_without_reallocation() {
        let prepared = super::PreparedVmnetVirtioNetworkRxBuffer::supported_maximum()
            .expect("supported maximum RX buffer should allocate");
        let debug = format!("{prepared:?}");
        assert_eq!(debug, "PreparedVmnetVirtioNetworkRxBuffer(<owned>)");
        assert!(!debug.contains(&super::DEFAULT_VMNET_VIRTIO_NETWORK_RX_BUFFER_LEN.to_string()));

        let buffer = prepared
            .into_buffer_with_len(2048)
            .expect("prepared RX buffer should accept smaller host bound");

        assert_eq!(buffer.len(), 2048);
        assert!(
            buffer.capacity() >= super::DEFAULT_VMNET_VIRTIO_NETWORK_RX_BUFFER_LEN,
            "truncation must retain the storage reserved before vmnet startup"
        );
        assert!(buffer.iter().all(|byte| *byte == 0));
    }

    #[test]
    fn packet_io_owner_debug_omits_backend_buffers_and_interface_ids() {
        let packet_io = packet_io(
            FakeVmnetPacketIoBackend::default()
                .with_read_result(Ok(Some(vec![0x5a, 0xa5, 0x5a, 0xa5]))),
        );
        assert_eq!(
            format!("{packet_io:?}"),
            "VmnetVirtioNetworkPacketIo(<owned>)"
        );

        let provider = VmnetVirtioNetworkPacketIoProvider::new(vec![
            VmnetVirtioNetworkPacketIoProviderEntry::new("private-interface", packet_io),
        ])
        .expect("provider should build");
        let debug = format!("{provider:?}");
        assert!(debug.contains("entry_count: 1"));
        assert!(!debug.contains("private-interface"));
        assert!(!debug.contains("90"));
        assert!(!debug.contains("165"));
    }

    #[test]
    fn prepared_rx_buffer_rejects_empty_and_larger_host_bounds_without_values() {
        let empty = super::PreparedVmnetVirtioNetworkRxBuffer::with_len(8)
            .expect("prepared RX buffer should allocate")
            .into_buffer_with_len(0)
            .expect_err("empty realized RX bound should fail");
        assert!(matches!(
            empty,
            VmnetVirtioNetworkPacketIoBuildError::EmptyRxBuffer
        ));

        let too_small = super::PreparedVmnetVirtioNetworkRxBuffer::with_len(8)
            .expect("prepared RX buffer should allocate")
            .into_buffer_with_len(9)
            .expect_err("larger realized RX bound should fail");
        assert!(matches!(
            too_small,
            VmnetVirtioNetworkPacketIoBuildError::RxBufferTooSmall
        ));
        assert_eq!(
            too_small.to_string(),
            "prepared vmnet virtio-net RX buffer is smaller than the host bound"
        );
        assert!(!too_small.to_string().contains('8'));
        assert!(!too_small.to_string().contains('9'));
    }

    #[test]
    fn packet_readiness_coalesces_storms_and_survives_full_or_disconnected_signals() {
        let (signal, receiver) = sync_channel(1);
        let (lease, _callback) = super::VmnetPacketReadinessLease::new(1, signal);
        let state = Arc::clone(&lease.state);

        for estimate in [None, Some(2), Some(500), Some(3)] {
            state.publish(estimate);
        }

        assert_eq!(receiver.try_recv(), Ok(()));
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
        assert!(lease.take_scheduled());
        assert!(!lease.take_scheduled());
        assert!(state.is_ready());
        assert_eq!(
            state
                .estimated_packets
                .load(std::sync::atomic::Ordering::Acquire),
            3
        );

        let (full_signal, full_receiver) = sync_channel(1);
        full_signal
            .try_send(())
            .expect("test channel should accept its preexisting signal");
        let (full_lease, _callback) = super::VmnetPacketReadinessLease::new(2, full_signal);
        let full_state = Arc::clone(&full_lease.state);
        full_state.publish(Some(4));
        assert!(full_state.is_ready());
        assert!(full_lease.take_scheduled());
        assert_eq!(full_receiver.try_recv(), Ok(()));
        assert_eq!(full_receiver.try_recv(), Err(TryRecvError::Empty));

        let (disconnected_signal, disconnected_receiver) = sync_channel(1);
        drop(disconnected_receiver);
        let (disconnected_lease, _callback) =
            super::VmnetPacketReadinessLease::new(3, disconnected_signal);
        let disconnected_state = Arc::clone(&disconnected_lease.state);
        disconnected_state.publish(Some(1));
        assert!(disconnected_state.is_ready());
        assert!(disconnected_lease.take_scheduled());
    }

    #[test]
    fn retired_packet_readiness_generation_cannot_wake_replacement() {
        let (signal, receiver) = sync_channel(1);
        let (old_lease, _old_callback) = super::VmnetPacketReadinessLease::new(40, signal.clone());
        let old_state = Arc::clone(&old_lease.state);
        drop(old_lease);

        old_state.publish(Some(1));
        assert!(!old_state.is_ready());
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));

        let (replacement, _replacement_callback) =
            super::VmnetPacketReadinessLease::new(41, signal);
        replacement.state.publish(Some(1));
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(replacement.take_scheduled());
        assert!(replacement.state.is_ready());
    }

    #[test]
    fn rx_source_caches_one_host_batch_and_defers_another_batch_to_next_pass() {
        let (signal, receiver) = sync_channel(1);
        let (lease, _callback) = super::VmnetPacketReadinessLease::new(50, signal);
        let state = Arc::clone(&lease.state);
        state.publish(Some(2));
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(lease.take_scheduled());
        let backend = FakeVmnetPacketIoBackend::default()
            .with_read_result(Ok(Some(vec![0x10])))
            .with_read_result(Ok(Some(vec![0x20, 0x21])))
            .with_read_result(Ok(Some(vec![0x30])));
        let mut packet_io = batch_packet_io(backend, 64, 2, 2, None, Some(lease));

        {
            let source = packet_io.rx_source();
            source.begin_rx_dispatch();
            assert_eq!(
                source
                    .peek_packet()
                    .expect("first batch packet should read")
                    .expect("first batch packet should exist")
                    .bytes(),
                [0x10]
            );
            source.consume_packet();
            assert_eq!(
                source
                    .peek_packet()
                    .expect("second cached packet should read")
                    .expect("second cached packet should exist")
                    .bytes(),
                [0x20, 0x21]
            );
            source.consume_packet();
            assert!(
                source
                    .peek_packet()
                    .expect("same owner pass should remain valid")
                    .is_none(),
                "one owner pass must issue at most one host batch"
            );
            assert!(source.host_readiness_hint());
        }
        assert!(packet_io.take_scheduled_readiness());
        assert_eq!(receiver.try_recv(), Ok(()));

        {
            let source = packet_io.rx_source();
            source.begin_rx_dispatch();
            assert_eq!(
                source
                    .peek_packet()
                    .expect("next owner pass should read another batch")
                    .expect("third packet should exist")
                    .bytes(),
                [0x30]
            );
            assert!(source.host_readiness_hint());
            source.consume_packet();
            assert!(!source.host_readiness_hint());
        }
        assert!(!packet_io.has_persistent_readiness());
        let shared = packet_io
            .rx_source
            .shared
            .lock()
            .expect("test packet state should lock");
        assert_eq!(shared.backend.read_batch_calls, 2);
        assert_eq!(shared.backend.read_batch_requests, [2, 2]);
        drop(shared);
        let backend_metrics = packet_io.rx_source().take_backend_metrics();
        assert_eq!(backend_metrics.vmnet_read_count(), 2);
        assert_eq!(backend_metrics.vmnet_read_fails(), 0);
        assert_eq!(backend_metrics.vmnet_read_packets_count(), 3);
        assert_eq!(backend_metrics.vmnet_read_partial_batches(), 1);
        assert_eq!(backend_metrics.vmnet_read_latency().samples(), 2);
        assert_eq!(
            packet_io.rx_source().take_backend_metrics(),
            super::VirtioNetworkBackendMetrics::default()
        );
    }

    #[test]
    fn zero_packet_read_preserves_callback_published_during_batch() {
        let (signal, receiver) = sync_channel(1);
        let (lease, _callback) = super::VmnetPacketReadinessLease::new(60, signal);
        let state = Arc::clone(&lease.state);
        state.publish(Some(1));
        assert_eq!(receiver.try_recv(), Ok(()));
        assert!(lease.take_scheduled());
        let backend = PublishingReadBackend {
            readiness: Arc::clone(&state),
            read_batch_calls: 0,
        };
        let mut packet_io = batch_packet_io(backend, 64, 1, 1, None, Some(lease));

        {
            let source = packet_io.rx_source();
            source.begin_rx_dispatch();
            assert!(
                source
                    .peek_packet()
                    .expect("zero-packet read should succeed")
                    .is_none()
            );
        }

        assert!(packet_io.has_persistent_readiness());
        assert!(packet_io.take_scheduled_readiness());
        assert_eq!(receiver.try_recv(), Ok(()));
        let shared = packet_io
            .rx_source
            .shared
            .lock()
            .expect("test packet state should lock");
        assert_eq!(shared.backend.read_batch_calls, 1);
    }

    #[test]
    fn tx_sink_stages_guest_bytes_and_maps_short_batch_prefix() {
        let backend = FakeVmnetPacketIoBackend::default().with_write_batch_completed(1);
        let mut packet_io = batch_packet_io(backend, 2_048, 1, 2, None, None);
        let mut memory = tx_memory();
        let first_packet = [0x10, 0x11, 0x12, 0x13];
        let first_frame = tx_frame(&mut memory, &[(&first_packet, PAYLOAD_ADDRESS)]);

        assert_eq!(
            packet_io
                .tx_sink()
                .stage_frame(&memory, &first_frame)
                .expect("first packet should stage"),
            VirtioNetworkTxPacketStage::Staged {
                flush_before_commit: false
            }
        );
        memory
            .write_slice(&[0xff; 4], PAYLOAD_ADDRESS)
            .expect("guest packet mutation should write after staging");
        assert_eq!(
            packet_io.tx_sink().commit_staged_frame(),
            VirtioNetworkTxPacketCommit::Deferred
        );

        let second_packet = [0x20, 0x21, 0x22];
        let second_frame = tx_frame(&mut memory, &[(&second_packet, SECOND_PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .stage_frame(&memory, &second_frame)
            .expect("second packet should stage");
        assert_eq!(
            packet_io.tx_sink().commit_staged_frame(),
            VirtioNetworkTxPacketCommit::Deferred
        );
        let mut results = Vec::new();
        packet_io.tx_sink().flush_staged_frames(&mut results);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0], Ok(VirtioNetworkTxPacketDisposition::Forwarded));
        assert_eq!(
            results[1]
                .as_ref()
                .expect_err("short batch suffix should fail")
                .message(),
            "vmnet TX batch completed only a prefix; the suffix was not retried"
        );
        let shared = packet_io
            .tx_sink
            .shared
            .lock()
            .expect("test packet state should lock");
        assert_eq!(shared.backend.write_batch_calls, 1);
        assert_eq!(shared.backend.write_batch_requests, [2]);
        assert_eq!(
            shared.backend.write_batches,
            [vec![first_packet.to_vec(), second_packet.to_vec()]]
        );
        assert_eq!(shared.backend.written_packets, [first_packet.to_vec()]);
        drop(shared);
        let backend_metrics = packet_io.tx_sink().take_backend_metrics();
        assert_eq!(backend_metrics.vmnet_write_count(), 1);
        assert_eq!(backend_metrics.vmnet_write_fails(), 0);
        assert_eq!(backend_metrics.vmnet_write_packets_count(), 1);
        assert_eq!(backend_metrics.vmnet_write_partial_batches(), 1);
        assert_eq!(backend_metrics.vmnet_write_latency().samples(), 1);
        assert_eq!(
            packet_io.tx_sink().take_backend_metrics(),
            super::VirtioNetworkBackendMetrics::default()
        );
    }

    #[test]
    fn tx_sink_rejects_out_of_range_batch_count_as_backend_failure() {
        let backend = FakeVmnetPacketIoBackend::default().with_write_batch_completed(2);
        let mut packet_io = batch_packet_io(backend, 2_048, 1, 2, None, None);
        let mut memory = tx_memory();
        let packet = [0x10, 0x11, 0x12, 0x13];
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);

        packet_io
            .tx_sink()
            .stage_frame(&memory, &frame)
            .expect("packet should stage");
        assert_eq!(
            packet_io.tx_sink().commit_staged_frame(),
            VirtioNetworkTxPacketCommit::Deferred
        );
        let mut results = Vec::new();
        packet_io.tx_sink().flush_staged_frames(&mut results);

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0]
                .as_ref()
                .expect_err("out-of-range batch count should fail")
                .message(),
            "vmnet TX batch returned an out-of-range completed count"
        );
        let backend_metrics = packet_io.tx_sink().take_backend_metrics();
        assert_eq!(backend_metrics.vmnet_write_count(), 1);
        assert_eq!(backend_metrics.vmnet_write_fails(), 1);
        assert_eq!(backend_metrics.vmnet_write_packets_count(), 0);
        assert_eq!(backend_metrics.vmnet_write_partial_batches(), 0);
        assert_eq!(backend_metrics.vmnet_write_latency().samples(), 1);
    }

    #[test]
    fn packet_io_retains_mmds_arp_reply_until_rx_consumes_it() {
        let detour = MmdsPacketDetour::try_new_for_test(
            MmdsStateHandle::default(),
            DEFAULT_MMDS_IPV4_ADDRESS,
            SharedMmdsMetrics::default(),
            7,
        )
        .expect("MMDS session should build");
        let stack = detour.stack();
        let mut packet_io = VmnetVirtioNetworkPacketIo::with_mmds_detour(
            FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0x33]))),
            fake_interface(),
            detour,
        )
        .expect("packet I/O with MMDS should build");
        let request = mmds_arp_request();
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&request, PAYLOAD_ADDRESS)]);

        assert!(!packet_io.has_persistent_readiness());
        assert_eq!(
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("MMDS ARP should detour"),
            VirtioNetworkTxPacketDisposition::Detoured
        );
        assert!(packet_io.has_persistent_readiness());
        assert_eq!(packet_io.mmds_retry_after(), None);

        let first = packet_io
            .rx_source()
            .peek_packet()
            .expect("MMDS ARP reply peek should succeed")
            .expect("MMDS ARP reply should be available")
            .bytes()
            .to_vec();
        let repeated = packet_io
            .rx_source()
            .peek_packet()
            .expect("repeated MMDS ARP reply peek should succeed")
            .expect("retained MMDS ARP reply should remain available")
            .bytes()
            .to_vec();
        assert_eq!(first, repeated);
        assert_eq!(&first[..6], &TEST_SOURCE_ETHERNET_ADDRESS);
        assert_eq!(&first[6..12], &DEFAULT_MMDS_MAC_ADDRESS.octets());
        assert_eq!(
            u16::from_be_bytes(first[12..14].try_into().expect("ethertype should exist")),
            ETHERNET_ETHERTYPE_ARP
        );
        let arp = &first[ETHERNET_HEADER_LEN..];
        assert_eq!(
            u16::from_be_bytes(
                arp[ARP_OPERATION_OFFSET..ARP_OPERATION_OFFSET + 2]
                    .try_into()
                    .expect("ARP operation should exist")
            ),
            ARP_OPERATION_REPLY
        );
        assert_eq!(
            &arp[ARP_TARGET_PROTOCOL_ADDRESS_OFFSET..ARP_TARGET_PROTOCOL_ADDRESS_OFFSET + 4],
            &TEST_SOURCE_IPV4_ADDRESS.octets()
        );

        packet_io.rx_source().consume_packet();
        assert_eq!(
            stack
                .retained_frame_len()
                .expect("MMDS session should remain readable"),
            None
        );
        let host_packet = packet_io
            .rx_source()
            .peek_packet()
            .expect("host packet peek should succeed")
            .expect("host packet should follow consumed MMDS reply");
        assert_eq!(host_packet.bytes(), &[0x33]);
    }

    #[test]
    fn tx_sink_writes_single_segment_payload() {
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&[0xaa, 0xbb, 0xcc], PAYLOAD_ADDRESS)]);
        let mut packet_io = packet_io(FakeVmnetPacketIoBackend::default());

        let disposition = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("TX should write vmnet packet");

        assert_eq!(disposition, VirtioNetworkTxPacketDisposition::Forwarded);
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [vec![0xaa, 0xbb, 0xcc]]);
    }

    #[test]
    fn tx_sink_direct_path_rejects_frame_above_realized_packet_bound() {
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&[0xaa, 0xbb, 0xcc, 0xdd], PAYLOAD_ADDRESS)]);
        let mut packet_io =
            batch_packet_io(FakeVmnetPacketIoBackend::default(), 3, 1, 1, None, None);

        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect_err("oversized direct TX frame should fail");

        assert_eq!(
            error.message(),
            "vmnet TX normalized packet exceeds the realized per-packet bound"
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        assert!(state.backend.written_packets.is_empty());
    }

    #[test]
    fn tx_sink_streams_large_tso_plan_through_bounded_batches() {
        let mut memory = tx_memory();
        let dummy_frame = tx_frame(&mut memory, &[(&[0xaa], PAYLOAD_ADDRESS)]);
        let payload = vec![0x5a; 60_000];
        let packet = tcp_ipv4_packet(Ipv4Addr::new(198, 51, 100, 2), 443, &payload);
        let header = VirtioNetworkTxHeader::new()
            .with_flags(VIRTIO_NET_HDR_F_NEEDS_CSUM)
            .with_gso_type(VIRTIO_NET_HDR_GSO_TCPV4)
            .with_gso_size(16)
            .with_checksum_start((ETHERNET_HEADER_LEN + IPV4_HEADER_LEN) as u16)
            .with_checksum_offset(16);
        let plan = VirtioNetworkPacketPlan::prepare(
            header,
            (1_u64 << VIRTIO_NET_F_CSUM) | (1_u64 << VIRTIO_NET_F_HOST_TSO4),
            packet,
        )
        .expect("large TSO plan should validate");
        let expected_packets = plan
            .emitted_packet_count()
            .expect("large TSO packet count should fit");
        assert!(
            plan.emitted_len(super::VirtioNetworkPacketEnvelope::RawEthernet)
                .expect("large TSO emitted length should fit")
                > crate::host_network::vmnet::VMNET_MAX_BYTES_PER_OPERATION
        );
        let mut packet_io = batch_packet_io(
            FakeVmnetPacketIoBackend::default(),
            2_048,
            1,
            200,
            None,
            None,
        );

        assert_eq!(
            packet_io
                .tx_sink()
                .stage_prepared_frame(&memory, &dummy_frame, &plan)
                .expect("large TSO plan should stage without expansion"),
            VirtioNetworkTxPacketStage::Staged {
                flush_before_commit: false
            }
        );
        assert_eq!(
            packet_io.tx_sink().commit_staged_frame(),
            VirtioNetworkTxPacketCommit::Deferred
        );
        let mut results = Vec::new();
        packet_io.tx_sink().flush_staged_frames(&mut results);

        assert_eq!(results, [Ok(VirtioNetworkTxPacketDisposition::Forwarded)]);
        let state = packet_io
            .tx_sink
            .shared
            .lock()
            .expect("test packet state should lock");
        assert_eq!(state.backend.written_packets.len(), expected_packets);
        assert!(
            state
                .backend
                .write_batch_requests
                .iter()
                .all(|requested| *requested <= 200)
        );
        assert!(state.backend.write_batch_calls > 1);
    }

    #[test]
    fn tx_sink_observes_spoofed_source_mac_without_filtering() {
        let mut memory = tx_memory();
        let packet = tcp_ipv4_packet(Ipv4Addr::new(198, 51, 100, 2), 443, b"payload");
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let prepared = super::PreparedVmnetVirtioNetworkRxBuffer::supported_maximum()
            .expect("test packet buffers should prepare");
        let mut packet_io =
            VmnetVirtioNetworkPacketIo::with_prepared_batch_envelope_and_mmds_detour(
                FakeVmnetPacketIoBackend::default(),
                fake_interface(),
                prepared,
                super::VmnetVirtioNetworkPacketProfile::new(
                    super::VmnetVirtioNetworkBatchLimits::new(2_048, 1, 1),
                    super::VirtioNetworkPacketEnvelope::RawEthernet,
                    Some(GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 1])),
                ),
                None,
                None,
            )
            .expect("profiled packet I/O should build");

        assert_eq!(
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("spoof observation must not filter TX"),
            VirtioNetworkTxPacketDisposition::Forwarded
        );
        let state = packet_io
            .tx_sink
            .shared
            .lock()
            .expect("test packet state should lock");
        assert_eq!(state.backend.written_packets, [packet]);
        drop(state);
        let metrics = packet_io.tx_sink().take_backend_metrics();
        assert_eq!(metrics.tx_spoofed_mac_count(), 1);
        assert_eq!(metrics.vmnet_write_packets_count(), 1);
    }

    #[test]
    fn tx_sink_writes_multi_segment_payload() {
        let mut memory = tx_memory();
        let frame = tx_frame(
            &mut memory,
            &[
                (&[0xaa, 0xbb][..], PAYLOAD_ADDRESS),
                (&[0xcc, 0xdd][..], SECOND_PAYLOAD_ADDRESS),
            ],
        );
        let mut packet_io = packet_io(FakeVmnetPacketIoBackend::default());

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("TX should write vmnet packet");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(
            state.backend.written_packets,
            [vec![0xaa, 0xbb, 0xcc, 0xdd]]
        );
    }

    #[test]
    fn tx_sink_reports_guest_memory_read_failure() {
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&[0xaa, 0xbb], PAYLOAD_ADDRESS)]);
        let unmapped_payload_layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0x4000), 0x4000)
                .expect("unmapped payload range should be valid"),
        ])
        .expect("unmapped payload layout should be valid");
        let unmapped_payload_memory =
            GuestMemory::allocate(&unmapped_payload_layout).expect("test memory should allocate");
        let mut packet_io = packet_io(FakeVmnetPacketIoBackend::default());

        let error = packet_io
            .tx_sink()
            .transmit_frame(&unmapped_payload_memory, &frame)
            .expect_err("unmapped payload should fail");

        assert!(
            error
                .message()
                .contains("failed to read virtio-net TX payload descriptor 1")
        );
    }

    #[test]
    fn tx_sink_reports_vmnet_write_failure() {
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&[0xaa], PAYLOAD_ADDRESS)]);
        let backend = FakeVmnetPacketIoBackend::default()
            .with_write_error(unexpected_count_error(VmnetOperation::WritePackets));
        let mut packet_io = packet_io(backend);

        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect_err("vmnet write failure should surface");

        assert!(error.message().contains(
            "vmnet TX packet write failed: vmnet_write returned an unexpected packet count"
        ));
    }

    #[test]
    fn rx_source_returns_none_when_vmnet_has_no_packet() {
        let backend = FakeVmnetPacketIoBackend::default().with_read_result(Ok(None));
        let mut packet_io = packet_io(backend);

        assert!(
            packet_io
                .rx_source()
                .peek_packet()
                .expect("peek should succeed")
                .is_none()
        );
        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 1);
    }

    #[test]
    fn rx_source_caches_peeked_packet_until_consumed() {
        let backend = FakeVmnetPacketIoBackend::default()
            .with_read_result(Ok(Some(vec![0x11, 0x22])))
            .with_read_result(Ok(Some(vec![0x33])));
        let mut packet_io = packet_io(backend);

        let first = packet_io
            .rx_source()
            .peek_packet()
            .expect("first peek should succeed")
            .expect("packet should be present");
        assert_eq!(first.bytes(), &[0x11, 0x22]);
        let second = packet_io
            .rx_source()
            .peek_packet()
            .expect("second peek should succeed")
            .expect("cached packet should still be present");
        assert_eq!(second.bytes(), &[0x11, 0x22]);

        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 1);
    }

    #[test]
    fn rx_source_reads_next_packet_after_consume() {
        let backend = FakeVmnetPacketIoBackend::default()
            .with_read_result(Ok(Some(vec![0x11])))
            .with_read_result(Ok(Some(vec![0x22])));
        let mut packet_io = packet_io(backend);

        let first = packet_io
            .rx_source()
            .peek_packet()
            .expect("first peek should succeed")
            .expect("first packet should be present");
        assert_eq!(first.bytes(), &[0x11]);
        packet_io.rx_source().consume_packet();
        let second = packet_io
            .rx_source()
            .peek_packet()
            .expect("second peek should succeed")
            .expect("second packet should be present");
        assert_eq!(second.bytes(), &[0x22]);
    }

    #[test]
    fn rx_source_reports_vmnet_read_failure() {
        let backend = FakeVmnetPacketIoBackend::default()
            .with_read_result(Err(unexpected_count_error(VmnetOperation::ReadPackets)));
        let mut packet_io = packet_io(backend);

        let error = packet_io
            .rx_source()
            .peek_packet()
            .expect_err("vmnet read failure should surface");

        assert!(error.message().contains(
            "vmnet RX packet read failed: vmnet_read returned an unexpected packet count"
        ));
    }

    #[test]
    fn rx_source_rejects_backend_packet_len_larger_than_buffer() {
        let backend = OversizedReadBackend { packet_len: 2049 };
        let mut packet_io =
            VmnetVirtioNetworkPacketIo::with_rx_buffer_len(backend, fake_interface(), 2048)
                .expect("packet I/O should build");

        let error = packet_io
            .rx_source()
            .peek_packet()
            .expect_err("oversized backend read should fail");

        assert!(
            error.message().contains(
                "vmnet RX packet read failed: vmnet_read returned a packet larger than the validated read buffer"
            )
        );
        let backend_metrics = packet_io.rx_source().take_backend_metrics();
        assert_eq!(backend_metrics.vmnet_read_count(), 1);
        assert_eq!(backend_metrics.vmnet_read_fails(), 1);
        assert_eq!(backend_metrics.vmnet_read_packets_count(), 0);
        assert_eq!(backend_metrics.vmnet_read_partial_batches(), 0);
        assert_eq!(backend_metrics.vmnet_read_latency().samples(), 1);
    }

    #[test]
    fn rx_source_rejects_empty_backend_packet_as_backend_failure() {
        let backend = FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![])));
        let mut packet_io = packet_io(backend);

        let error = packet_io
            .rx_source()
            .peek_packet()
            .expect_err("empty backend packet should fail");

        assert!(error.message().contains(
            "vmnet RX packet read failed: invalid vmnet packet batch: read backend returned an empty packet"
        ));
        let backend_metrics = packet_io.rx_source().take_backend_metrics();
        assert_eq!(backend_metrics.vmnet_read_count(), 1);
        assert_eq!(backend_metrics.vmnet_read_fails(), 1);
        assert_eq!(backend_metrics.vmnet_read_packets_count(), 0);
        assert_eq!(backend_metrics.vmnet_read_latency().samples(), 1);
    }

    #[test]
    fn rx_source_rejects_out_of_range_batch_count_as_backend_failure() {
        let mut packet_io =
            batch_packet_io(ExcessiveReadBatchCountBackend, 2_048, 2, 1, None, None);

        let error = packet_io
            .rx_source()
            .peek_packet()
            .expect_err("out-of-range batch count should fail");

        assert!(error.message().contains(
            "vmnet RX packet read failed: invalid vmnet packet batch: read backend returned more packets than requested"
        ));
        let backend_metrics = packet_io.rx_source().take_backend_metrics();
        assert_eq!(backend_metrics.vmnet_read_count(), 1);
        assert_eq!(backend_metrics.vmnet_read_fails(), 1);
        assert_eq!(backend_metrics.vmnet_read_packets_count(), 0);
        assert_eq!(backend_metrics.vmnet_read_partial_batches(), 0);
        assert_eq!(backend_metrics.vmnet_read_latency().samples(), 1);
    }

    #[test]
    fn tx_sink_reports_poisoned_state_lock() {
        let mut packet_io = packet_io(FakeVmnetPacketIoBackend::default());
        let shared = Arc::clone(&packet_io.tx_sink().shared);
        let _ = std::thread::spawn(move || {
            let _guard = shared
                .lock()
                .expect("test lock should succeed before poison");
            panic!("poison test lock");
        })
        .join();
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&[0xaa], PAYLOAD_ADDRESS)]);

        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect_err("poisoned lock should fail");

        assert!(error.message().contains("state lock poisoned during TX"));
    }

    #[test]
    fn rx_source_reports_poisoned_state_lock() {
        let mut packet_io = packet_io(FakeVmnetPacketIoBackend::default());
        let shared = Arc::clone(&packet_io.rx_source().shared);
        let _ = std::thread::spawn(move || {
            let _guard = shared
                .lock()
                .expect("test lock should succeed before poison");
            panic!("poison test lock");
        })
        .join();

        let error = packet_io
            .rx_source()
            .peek_packet()
            .expect_err("poisoned lock should fail");

        assert!(error.message().contains("state lock poisoned during RX"));
    }

    #[test]
    fn packet_io_owner_exposes_distinct_tx_and_rx_handles() {
        fn assert_send<T: Send>() {}

        assert_send::<VmnetVirtioNetworkPacketIo<FakeVmnetPacketIoBackend>>();
    }

    #[test]
    fn provider_returns_packet_io_for_matching_interfaces() {
        let mut provider = VmnetVirtioNetworkPacketIoProvider::new(vec![
            provider_entry("eth0", FakeVmnetPacketIoBackend::default()),
            provider_entry("eth1", FakeVmnetPacketIoBackend::default()),
        ])
        .expect("provider should build");
        let eth0_device = network_device("eth0");
        let eth1_device = network_device("eth1");

        provider
            .packet_io(eth0_device.interface())
            .expect("eth0 provider entry should return packet I/O");
        provider
            .packet_io(eth1_device.interface())
            .expect("eth1 provider entry should return packet I/O");
    }

    #[test]
    fn provider_rejects_duplicate_interface_ids() {
        let error = VmnetVirtioNetworkPacketIoProvider::new(vec![
            provider_entry("eth0", FakeVmnetPacketIoBackend::default()),
            provider_entry("eth0", FakeVmnetPacketIoBackend::default()),
        ])
        .expect_err("duplicate interface ids should fail");

        assert!(matches!(
            error,
            VmnetVirtioNetworkPacketIoProviderBuildError::DuplicateInterfaceId { ref iface_id }
                if iface_id == "eth0"
        ));
    }

    #[test]
    fn provider_reports_missing_interface_id() {
        let mut provider = VmnetVirtioNetworkPacketIoProvider::new(vec![provider_entry(
            "eth0",
            FakeVmnetPacketIoBackend::default(),
        )])
        .expect("provider should build");
        let device = network_device("eth1");

        let error = provider
            .packet_io(device.interface())
            .expect_err("missing provider entry should fail");

        assert_eq!(
            error.message(),
            "missing vmnet packet I/O for interface eth1"
        );
    }

    #[test]
    fn provider_keeps_per_interface_packet_state_independent() {
        let mut provider = VmnetVirtioNetworkPacketIoProvider::new(vec![
            provider_entry(
                "eth0",
                FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0x10]))),
            ),
            provider_entry(
                "eth1",
                FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0x20]))),
            ),
        ])
        .expect("provider should build");
        let mut memory = tx_memory();
        let eth0_frame = tx_frame(&mut memory, &[(&[0xa0], PAYLOAD_ADDRESS)]);
        let eth1_frame = tx_frame(&mut memory, &[(&[0xb0], SECOND_PAYLOAD_ADDRESS)]);

        {
            let eth0 = provider
                .entries
                .iter_mut()
                .find(|entry| entry.iface_id() == "eth0")
                .expect("eth0 entry should exist");
            eth0.packet_io
                .tx_sink()
                .transmit_frame(&memory, &eth0_frame)
                .expect("eth0 TX should succeed");
            let packet = eth0
                .packet_io
                .rx_source()
                .peek_packet()
                .expect("eth0 RX should succeed")
                .expect("eth0 packet should exist");
            assert_eq!(packet.bytes(), &[0x10]);
        }

        {
            let eth1 = provider
                .entries
                .iter_mut()
                .find(|entry| entry.iface_id() == "eth1")
                .expect("eth1 entry should exist");
            eth1.packet_io
                .tx_sink()
                .transmit_frame(&memory, &eth1_frame)
                .expect("eth1 TX should succeed");
            let packet = eth1
                .packet_io
                .rx_source()
                .peek_packet()
                .expect("eth1 RX should succeed")
                .expect("eth1 packet should exist");
            assert_eq!(packet.bytes(), &[0x20]);
        }

        {
            let eth0 = provider
                .entries
                .iter_mut()
                .find(|entry| entry.iface_id() == "eth0")
                .expect("eth0 entry should exist");
            let eth0_state = eth0
                .packet_io
                .tx_sink()
                .shared
                .lock()
                .expect("eth0 state lock should succeed");
            assert_eq!(eth0_state.backend.written_packets, [vec![0xa0]]);
        }

        {
            let eth1 = provider
                .entries
                .iter_mut()
                .find(|entry| entry.iface_id() == "eth1")
                .expect("eth1 entry should exist");
            let eth1_state = eth1
                .packet_io
                .tx_sink()
                .shared
                .lock()
                .expect("eth1 state lock should succeed");
            assert_eq!(eth1_state.backend.written_packets, [vec![0xb0]]);
        }
    }
}
