//! Adapters between internal virtio-net packet traits and vmnet packet I/O.

use std::collections::TryReserveError;
use std::fmt;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Mutex, MutexGuard};

use bangbang_runtime::memory::{GuestMemory, GuestMemoryAccessError};
#[cfg(test)]
pub(crate) use bangbang_runtime::mmds_network::{
    DEFAULT_MMDS_REQUEST_BUFFER_CAPACITY, DEFAULT_MMDS_REQUEST_BUFFER_LEN_LIMIT,
};
pub(crate) use bangbang_runtime::mmds_network::{
    MmdsOnlyVirtioNetworkPacketIo, MmdsOnlyVirtioNetworkPacketIoBuildError, MmdsPacketDetour,
    MmdsResponseQueue,
};
#[cfg(test)]
pub(crate) use bangbang_runtime::mmds_network::{
    MmdsOnlyVirtioNetworkPacketIoProvider, MmdsOnlyVirtioNetworkPacketIoProviderBuildError,
    MmdsOnlyVirtioNetworkPacketIoProviderEntry,
};
use bangbang_runtime::mmds_network::{MmdsPacketDetourError, MmdsResponseQueueError};
use bangbang_runtime::network::{
    VIRTIO_NET_MAX_BUFFER_SIZE, VirtioNetworkRxPacket, VirtioNetworkRxPacketSource,
    VirtioNetworkRxPacketSourceError, VirtioNetworkTxFrame, VirtioNetworkTxPacketCommit,
    VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSink, VirtioNetworkTxPacketSinkError,
    VirtioNetworkTxPacketStage,
};
use bangbang_runtime::startup::{
    Arm64BootNetworkInterface, Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError,
    Arm64BootNetworkPacketIoProvider,
};

#[cfg(test)]
use crate::host_network::vmnet::VmnetReadPacket;
use crate::host_network::vmnet::{
    StartedVmnetPacketIoBackend, VMNET_MAX_BYTES_PER_OPERATION, VMNET_MAX_PACKETS_PER_OPERATION,
    VmnetError, VmnetInterfaceBackend, VmnetPacketAvailableCallback, VmnetPacketDescriptorError,
    VmnetPacketIoBackend, VmnetPacketIoError, VmnetWritePacket,
};

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
        let parts = prepared.into_batch_parts(
            limits.packet_capacity,
            limits.read_batch_size,
            limits.write_batch_size,
        )?;
        let readiness = readiness_lease
            .as_ref()
            .map(VmnetPacketReadinessLease::consumer);
        let shared = Arc::new(Mutex::new(VmnetVirtioNetworkPacketIoState {
            backend,
            interface,
        }));
        let mmds_response_queue = mmds_detour.as_ref().map(MmdsPacketDetour::response_queue);

        Ok(Self {
            tx_sink: VmnetVirtioNetworkTxPacketSink::new(
                Arc::clone(&shared),
                mmds_detour,
                parts.tx_buffer,
                parts.packet_ranges,
                parts.packet_capacity,
                parts.write_batch_size,
            ),
            rx_source: VmnetVirtioNetworkRxPacketSource::new(
                shared,
                parts.rx_buffer,
                parts.packet_lengths,
                parts.packet_capacity,
                parts.read_batch_size,
                mmds_response_queue,
                readiness,
            ),
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
        self.rx_source.has_host_readiness()
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
    staged_packet: Option<StagedVmnetTxPacket>,
    maximum_packet_size: usize,
    maximum_batch_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StagedVmnetTxPacketKind {
    External,
    MmdsDetour,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StagedVmnetTxPacket {
    range: Range<usize>,
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
            .field("committed_packets", &self.committed_ranges.len())
            .field("staged_packet", &self.staged_packet.is_some())
            .finish()
    }
}

impl<B> VmnetVirtioNetworkTxPacketSink<B>
where
    B: VmnetPacketIoBackend,
{
    fn new(
        shared: Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
        mmds_detour: Option<MmdsPacketDetour>,
        staging_buffer: Vec<u8>,
        committed_ranges: Vec<Range<usize>>,
        maximum_packet_size: usize,
        maximum_batch_size: usize,
    ) -> Self {
        Self {
            shared,
            mmds_detour,
            staging_buffer,
            committed_ranges,
            staged_packet: None,
            maximum_packet_size,
            maximum_batch_size,
        }
    }

    fn validate_frame_packet_len(
        &self,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<usize, VirtioNetworkTxPacketSinkError> {
        let packet_len = usize::try_from(frame.payload_len()).map_err(|_| {
            tx_error(VmnetVirtioNetworkTxCopyError::PayloadLengthTooLarge {
                len: frame.payload_len(),
            })
        })?;
        if packet_len == 0 || packet_len > self.maximum_packet_size {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet TX frame exceeds the realized per-packet bound",
            ));
        }
        Ok(packet_len)
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
        self.validate_frame_packet_len(frame)?;
        let packet = copy_tx_frame_payload(memory, frame).map_err(tx_error)?;
        if let Some(mmds_detour) = &mut self.mmds_detour
            && mmds_detour
                .detour_packet(&packet)
                .map_err(tx_mmds_detour_error)?
        {
            return Ok(VirtioNetworkTxPacketDisposition::Detoured);
        }

        let mut packet = VmnetWritePacket::new(&packet).map_err(tx_descriptor_error)?;
        let mut state = lock_state_for_tx(&self.shared)?;
        let VmnetVirtioNetworkPacketIoState { backend, interface } = &mut *state;

        backend
            .write_packet(interface, &mut packet)
            .map(|()| VirtioNetworkTxPacketDisposition::Forwarded)
            .map_err(tx_vmnet_error)
    }

    fn supports_staged_batch(&self) -> bool {
        true
    }

    fn stage_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        if self.staged_packet.is_some() {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet TX staging already owns an uncommitted frame",
            ));
        }
        let packet_len = self.validate_frame_packet_len(frame)?;
        let required_len = self
            .staging_buffer
            .len()
            .checked_add(packet_len)
            .ok_or_else(|| VirtioNetworkTxPacketSinkError::new("vmnet TX batch size overflowed"))?;
        if !self.committed_ranges.is_empty()
            && (self.committed_ranges.len() == self.maximum_batch_size
                || required_len > VMNET_MAX_BYTES_PER_OPERATION)
        {
            return Ok(VirtioNetworkTxPacketStage::FlushRequired);
        }
        if required_len > VMNET_MAX_BYTES_PER_OPERATION
            || required_len > self.staging_buffer.capacity()
        {
            return Err(VirtioNetworkTxPacketSinkError::new(
                "vmnet TX frame exceeds the empty aggregate byte bound",
            ));
        }

        let start = self.staging_buffer.len();
        if let Err(source) = copy_tx_frame_payload_into(memory, frame, &mut self.staging_buffer) {
            self.staging_buffer.truncate(start);
            return Err(tx_error(source));
        }
        let range = start..self.staging_buffer.len();
        let packet = self.staging_buffer.get(range.clone()).ok_or_else(|| {
            VirtioNetworkTxPacketSinkError::new("vmnet TX staged packet range became invalid")
        })?;
        let kind = if self
            .mmds_detour
            .as_ref()
            .is_some_and(|detour| detour.would_detour_packet(packet))
        {
            StagedVmnetTxPacketKind::MmdsDetour
        } else {
            StagedVmnetTxPacketKind::External
        };
        self.staged_packet = Some(StagedVmnetTxPacket { range, kind });
        Ok(VirtioNetworkTxPacketStage::Staged {
            flush_before_commit: kind == StagedVmnetTxPacketKind::MmdsDetour,
        })
    }

    fn commit_staged_frame(&mut self) -> VirtioNetworkTxPacketCommit {
        let Some(staged) = self.staged_packet.take() else {
            return VirtioNetworkTxPacketCommit::Immediate(Err(
                VirtioNetworkTxPacketSinkError::new("vmnet TX commit has no staged frame"),
            ));
        };
        match staged.kind {
            StagedVmnetTxPacketKind::External => {
                self.committed_ranges.push(staged.range);
                VirtioNetworkTxPacketCommit::Deferred
            }
            StagedVmnetTxPacketKind::MmdsDetour => {
                let Some(packet) = self.staging_buffer.get(staged.range.clone()) else {
                    self.staging_buffer.truncate(staged.range.start);
                    return VirtioNetworkTxPacketCommit::Immediate(Err(
                        VirtioNetworkTxPacketSinkError::new(
                            "vmnet TX staged MMDS packet range became invalid",
                        ),
                    ));
                };
                let result = self
                    .mmds_detour
                    .as_mut()
                    .ok_or_else(|| {
                        VirtioNetworkTxPacketSinkError::new(
                            "vmnet TX MMDS staging lost its detour owner",
                        )
                    })
                    .and_then(|detour| {
                        detour
                            .detour_packet(packet)
                            .map_err(tx_mmds_detour_error)
                            .and_then(|detoured| {
                                if detoured {
                                    Ok(VirtioNetworkTxPacketDisposition::Detoured)
                                } else {
                                    Err(VirtioNetworkTxPacketSinkError::new(
                                        "MMDS side-effect classification changed at commit",
                                    ))
                                }
                            })
                    });
                self.staging_buffer.truncate(staged.range.start);
                VirtioNetworkTxPacketCommit::Immediate(result)
            }
        }
    }

    fn discard_staged_frame(&mut self) {
        if let Some(staged) = self.staged_packet.take() {
            self.staging_buffer.truncate(staged.range.start);
        }
    }

    fn flush_staged_frames(
        &mut self,
        results: &mut Vec<Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError>>,
    ) {
        let packet_count = self.committed_ranges.len();
        if packet_count == 0 {
            return;
        }
        let write_result = lock_state_for_tx(&self.shared).and_then(|mut state| {
            let VmnetVirtioNetworkPacketIoState { backend, interface } = &mut *state;
            backend
                .write_packet_batch(interface, &self.staging_buffer, &self.committed_ranges)
                .map_err(tx_vmnet_error)
        });
        match write_result {
            Ok(completed) if completed <= packet_count => {
                for packet_index in 0..packet_count {
                    if packet_index < completed {
                        results.push(Ok(VirtioNetworkTxPacketDisposition::Forwarded));
                    } else {
                        results.push(Err(VirtioNetworkTxPacketSinkError::new(
                            "vmnet TX batch completed only a prefix; the suffix was not retried",
                        )));
                    }
                }
            }
            Ok(_) => {
                let source = VirtioNetworkTxPacketSinkError::new(
                    "vmnet TX batch returned an out-of-range completed count",
                );
                results.extend((0..packet_count).map(|_| Err(source.clone())));
            }
            Err(source) => {
                results.extend((0..packet_count).map(|_| Err(source.clone())));
            }
        }
        self.committed_ranges.clear();
        if let Some(staged) = self.staged_packet.as_mut() {
            let retained_len = staged.range.len();
            self.staging_buffer.copy_within(staged.range.clone(), 0);
            self.staging_buffer.truncate(retained_len);
            staged.range = 0..retained_len;
        } else {
            self.staging_buffer.clear();
        }
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
    mmds_response_queue: Option<MmdsResponseQueue>,
    readiness: Option<VmnetPacketReadinessConsumer>,
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
                "mmds_response_queue",
                &self.mmds_response_queue.as_ref().map(|_| "<configured>"),
            )
            .finish()
    }
}

impl<B> VmnetVirtioNetworkRxPacketSource<B>
where
    B: VmnetPacketIoBackend,
{
    fn new(
        shared: Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
        read_buffer: Vec<u8>,
        packet_lengths: Vec<usize>,
        packet_capacity: usize,
        read_batch_size: usize,
        mmds_response_queue: Option<MmdsResponseQueue>,
        readiness: Option<VmnetPacketReadinessConsumer>,
    ) -> Self {
        Self {
            shared,
            read_buffer,
            packet_lengths,
            packet_capacity,
            read_batch_size,
            cached_packet_index: 0,
            cached_packet_count: 0,
            cached_packet_source: None,
            host_batch_attempted: None,
            mmds_response_queue,
            readiness,
        }
    }

    fn cached_packet(&self) -> Option<VirtioNetworkRxPacket<'_>> {
        if self.cached_packet_index >= self.cached_packet_count {
            return None;
        }
        let len = *self.packet_lengths.get(self.cached_packet_index)?;
        let start = match self.cached_packet_source? {
            CachedRxPacketSource::MmdsResponse => 0,
            CachedRxPacketSource::Vmnet => {
                self.cached_packet_index.checked_mul(self.packet_capacity)?
            }
        };
        let end = start.checked_add(len)?;
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
        self.has_host_readiness()
            || self
                .mmds_response_queue
                .as_ref()
                .is_some_and(MmdsResponseQueue::may_have_response)
    }

    fn peek_packet(
        &mut self,
    ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
        if self.cached_packet_index < self.cached_packet_count {
            return Ok(self.cached_packet());
        }

        if let Some(mmds_response_queue) = &self.mmds_response_queue
            && let Some(len) = mmds_response_queue
                .copy_front_into(
                    self.read_buffer
                        .get_mut(..self.packet_capacity)
                        .ok_or_else(|| {
                            VirtioNetworkRxPacketSourceError::new(
                                "vmnet RX packet buffer is smaller than its realized capacity",
                            )
                        })?,
                )
                .map_err(rx_mmds_response_queue_error)?
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
        let packet_count = {
            let mut state = lock_state_for_rx(&self.shared)?;
            let VmnetVirtioNetworkPacketIoState { backend, interface } = &mut *state;

            backend
                .read_packet_batch(
                    interface,
                    &mut self.read_buffer,
                    self.packet_capacity,
                    estimated_packets,
                    &mut self.packet_lengths,
                )
                .map_err(rx_vmnet_error)?
        };
        for packet_len in self.packet_lengths.iter().copied().take(packet_count) {
            validate_rx_packet_len(packet_len, self.packet_capacity)?;
        }
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
            && let Some(mmds_response_queue) = &self.mmds_response_queue
        {
            match mmds_response_queue.pop_front() {
                Ok(true) => mmds_response_queue.record_transmitted(packet_len),
                Ok(false) | Err(_) => mmds_response_queue.record_transmit_error(),
            }
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
}

#[derive(Debug)]
enum VmnetVirtioNetworkTxCopyError {
    PayloadLengthTooLarge {
        len: u64,
    },
    PacketAllocation {
        len: usize,
        source: TryReserveError,
    },
    SegmentLengthTooLarge {
        descriptor_index: u16,
        len: u32,
    },
    SegmentRead {
        descriptor_index: u16,
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for VmnetVirtioNetworkTxCopyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadLengthTooLarge { len } => {
                write!(
                    f,
                    "virtio-net TX payload length {len} does not fit host usize"
                )
            }
            Self::PacketAllocation { len, source } => {
                write!(
                    f,
                    "failed to reserve vmnet TX packet buffer of {len} bytes: {source}"
                )
            }
            Self::SegmentLengthTooLarge {
                descriptor_index,
                len,
            } => write!(
                f,
                "virtio-net TX payload descriptor {descriptor_index} length {len} does not fit host usize"
            ),
            Self::SegmentRead {
                descriptor_index,
                source,
            } => write!(
                f,
                "failed to read virtio-net TX payload descriptor {descriptor_index}: {source}"
            ),
        }
    }
}

impl std::error::Error for VmnetVirtioNetworkTxCopyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PacketAllocation { source, .. } => Some(source),
            Self::SegmentRead { source, .. } => Some(source),
            Self::PayloadLengthTooLarge { .. } | Self::SegmentLengthTooLarge { .. } => None,
        }
    }
}

fn copy_tx_frame_payload(
    memory: &GuestMemory,
    frame: &VirtioNetworkTxFrame,
) -> Result<Vec<u8>, VmnetVirtioNetworkTxCopyError> {
    let packet_len = usize::try_from(frame.payload_len()).map_err(|_| {
        VmnetVirtioNetworkTxCopyError::PayloadLengthTooLarge {
            len: frame.payload_len(),
        }
    })?;
    let mut packet = Vec::new();
    packet.try_reserve_exact(packet_len).map_err(|source| {
        VmnetVirtioNetworkTxCopyError::PacketAllocation {
            len: packet_len,
            source,
        }
    })?;

    copy_tx_frame_payload_into(memory, frame, &mut packet)?;
    Ok(packet)
}

fn copy_tx_frame_payload_into(
    memory: &GuestMemory,
    frame: &VirtioNetworkTxFrame,
    packet: &mut Vec<u8>,
) -> Result<(), VmnetVirtioNetworkTxCopyError> {
    let initial_len = packet.len();

    for segment in frame.payload_segments() {
        let segment_len = usize::try_from(segment.len()).map_err(|_| {
            VmnetVirtioNetworkTxCopyError::SegmentLengthTooLarge {
                descriptor_index: segment.descriptor_index(),
                len: segment.len(),
            }
        })?;
        let start = packet.len();
        let end = start.checked_add(segment_len).ok_or(
            VmnetVirtioNetworkTxCopyError::PayloadLengthTooLarge {
                len: frame.payload_len(),
            },
        )?;
        packet.resize(end, 0);
        let segment_buffer = packet.get_mut(start..end).ok_or(
            VmnetVirtioNetworkTxCopyError::PayloadLengthTooLarge {
                len: frame.payload_len(),
            },
        )?;
        memory
            .read_slice(segment_buffer, segment.address())
            .map_err(|source| VmnetVirtioNetworkTxCopyError::SegmentRead {
                descriptor_index: segment.descriptor_index(),
                source,
            })?;
    }

    debug_assert_eq!(
        packet.len().saturating_sub(initial_len) as u64,
        frame.payload_len()
    );
    Ok(())
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

fn tx_error(source: VmnetVirtioNetworkTxCopyError) -> VirtioNetworkTxPacketSinkError {
    VirtioNetworkTxPacketSinkError::new(source.to_string())
}

fn tx_descriptor_error(source: VmnetPacketDescriptorError) -> VirtioNetworkTxPacketSinkError {
    VirtioNetworkTxPacketSinkError::new(format!(
        "failed to build vmnet TX packet descriptor: {source}"
    ))
}

fn tx_vmnet_error(source: VmnetPacketIoError) -> VirtioNetworkTxPacketSinkError {
    VirtioNetworkTxPacketSinkError::new(format!("vmnet TX packet write failed: {source}"))
}

fn tx_mmds_detour_error(source: MmdsPacketDetourError) -> VirtioNetworkTxPacketSinkError {
    VirtioNetworkTxPacketSinkError::new(format!("MMDS packet detour failed: {source}"))
}

fn rx_vmnet_error(source: VmnetPacketIoError) -> VirtioNetworkRxPacketSourceError {
    VirtioNetworkRxPacketSourceError::new(format!("vmnet RX packet read failed: {source}"))
}

fn rx_mmds_response_queue_error(
    source: MmdsResponseQueueError,
) -> VirtioNetworkRxPacketSourceError {
    VirtioNetworkRxPacketSourceError::new(format!("MMDS response queue read failed: {source}"))
}

fn validate_rx_packet_len(
    packet_len: usize,
    buffer_len: usize,
) -> Result<(), VirtioNetworkRxPacketSourceError> {
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
    use bangbang_runtime::metrics::{MmdsMetrics, SharedMmdsMetrics};
    use bangbang_runtime::mmds::{
        DEFAULT_MMDS_IPV4_ADDRESS, DEFAULT_MMDS_MAC_ADDRESS, MMDS_GUEST_TCP_PORT, MmdsConfigInput,
        MmdsStateHandle, MmdsVersion,
    };
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::{
        NetworkInterfaceConfigInput, NetworkInterfaceConfigs, NetworkMmioLayout,
        PreparedNetworkDevices, VIRTIO_NET_TX_HEADER_SIZE, VirtioNetworkRxPacketSource,
        VirtioNetworkTxFrame, VirtioNetworkTxPacketCommit, VirtioNetworkTxPacketDisposition,
        VirtioNetworkTxPacketSink, VirtioNetworkTxPacketStage,
    };
    use bangbang_runtime::startup::{Arm64BootNetworkDevice, Arm64BootNetworkPacketIoProvider};
    use bangbang_runtime::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESCRIPTOR_SIZE, read_descriptor_chain,
    };

    use super::{
        MmdsOnlyVirtioNetworkPacketIo, MmdsOnlyVirtioNetworkPacketIoProvider,
        MmdsOnlyVirtioNetworkPacketIoProviderBuildError,
        MmdsOnlyVirtioNetworkPacketIoProviderEntry, MmdsPacketDetour, MmdsResponseQueue,
        VmnetPacketIoBackend, VmnetPacketIoError, VmnetReadPacket, VmnetVirtioNetworkPacketIo,
        VmnetVirtioNetworkPacketIoBuildError, VmnetVirtioNetworkPacketIoProvider,
        VmnetVirtioNetworkPacketIoProviderBuildError, VmnetVirtioNetworkPacketIoProviderEntry,
        VmnetWritePacket,
    };
    use crate::host_network::vmnet::{VmnetOperation, VmnetPacketCountExpectation};

    const DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x1000);
    const HEADER_ADDRESS: GuestAddress = GuestAddress::new(0x2000);
    const PAYLOAD_ADDRESS: GuestAddress = GuestAddress::new(0x3000);
    const SECOND_PAYLOAD_ADDRESS: GuestAddress = GuestAddress::new(0x4000);
    const THIRD_PAYLOAD_ADDRESS: GuestAddress = GuestAddress::new(0x5000);
    const ETHERNET_HEADER_LEN: usize = 14;
    const ETHERNET_ETHERTYPE_ARP: u16 = 0x0806;
    const ETHERNET_ETHERTYPE_IPV4: u16 = 0x0800;
    const IPV4_HEADER_LEN: usize = 20;
    const TCP_HEADER_LEN: usize = 20;
    const TCP_SEQUENCE_NUMBER_OFFSET: usize = 4;
    const TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET: usize = 8;
    const TCP_FLAGS_OFFSET: usize = 13;
    const TCP_FLAG_ACK: u8 = 0x10;
    const TCP_FLAG_FIN: u8 = 0x01;
    const TCP_FLAG_PSH: u8 = 0x08;
    const TCP_FLAG_RST: u8 = 0x04;
    const TCP_FLAG_SYN: u8 = 0x02;
    const ARP_HARDWARE_TYPE_ETHERNET: u16 = 1;
    const ARP_OPERATION_REQUEST: u16 = 1;
    const ARP_OPERATION_REPLY: u16 = 2;
    const ARP_PROTOCOL_TYPE_IPV4: u16 = ETHERNET_ETHERTYPE_IPV4;
    const ARP_HEADER_LEN: usize = 28;
    const ARP_OPERATION_OFFSET: usize = 6;
    const ARP_SENDER_HARDWARE_ADDRESS_OFFSET: usize = 8;
    const ARP_SENDER_PROTOCOL_ADDRESS_OFFSET: usize = 14;
    const ARP_TARGET_HARDWARE_ADDRESS_OFFSET: usize = 18;
    const ARP_TARGET_PROTOCOL_ADDRESS_OFFSET: usize = 24;
    const TEST_SOURCE_IPV4_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);
    const TEST_SOURCE_TCP_PORT: u16 = 49_152;
    const TEST_SOURCE_ETHERNET_ADDRESS: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
    const TEST_DESTINATION_ETHERNET_ADDRESS: [u8; 6] = [0xff; 6];
    const MMDS_SYN_ACK_ACKNOWLEDGEMENT_NUMBER: u32 = 1;

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
        tcp_ipv4_packet_from_source(
            destination_ipv4_address,
            destination_port,
            TEST_SOURCE_TCP_PORT,
            0,
            payload,
        )
    }

    fn tcp_ipv4_packet_from_source(
        destination_ipv4_address: Ipv4Addr,
        destination_port: u16,
        source_port: u16,
        sequence_number: u32,
        payload: &[u8],
    ) -> Vec<u8> {
        let ipv4_total_len = IPV4_HEADER_LEN + TCP_HEADER_LEN + payload.len();
        let packet_len = ETHERNET_HEADER_LEN + ipv4_total_len;
        let mut packet = vec![0_u8; packet_len];

        packet[12..14].copy_from_slice(&0x0800_u16.to_be_bytes());
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
        packet[tcp..tcp + 2].copy_from_slice(&source_port.to_be_bytes());
        packet[tcp + 2..tcp + 4].copy_from_slice(&destination_port.to_be_bytes());
        packet[tcp + 4..tcp + 8].copy_from_slice(&sequence_number.to_be_bytes());
        packet[tcp + 12] = 5 << 4;

        let payload_start = tcp + TCP_HEADER_LEN;
        packet[payload_start..].copy_from_slice(payload);
        packet
    }

    fn mmds_tcp_packet(payload: &[u8]) -> Vec<u8> {
        mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, 0, payload)
    }

    fn mmds_tcp_packet_from_source(
        source_port: u16,
        sequence_number: u32,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut packet = tcp_ipv4_packet_from_source(
            DEFAULT_MMDS_IPV4_ADDRESS,
            MMDS_GUEST_TCP_PORT,
            source_port,
            sequence_number,
            payload,
        );
        if !payload.is_empty() {
            set_tcp_flags(&mut packet, TCP_FLAG_PSH | TCP_FLAG_ACK);
            set_tcp_acknowledgement_number(&mut packet, MMDS_SYN_ACK_ACKNOWLEDGEMENT_NUMBER);
        }
        packet
    }

    fn set_tcp_flags(packet: &mut [u8], flags: u8) {
        let tcp = ETHERNET_HEADER_LEN + IPV4_HEADER_LEN;
        packet[tcp + TCP_FLAGS_OFFSET] = flags;
    }

    fn set_tcp_acknowledgement_number(packet: &mut [u8], acknowledgement_number: u32) {
        let tcp = ETHERNET_HEADER_LEN + IPV4_HEADER_LEN;
        packet
            [tcp + TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET..tcp + TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET + 4]
            .copy_from_slice(&acknowledgement_number.to_be_bytes());
    }

    fn mmds_tcp_syn_packet(sequence_number: u32) -> Vec<u8> {
        let mut packet = mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, sequence_number, b"");
        set_tcp_flags(&mut packet, TCP_FLAG_SYN);
        packet
    }

    fn mmds_tcp_fin_close_packet(
        sequence_number: u32,
        acknowledgement_number: u32,
        flags: u8,
    ) -> Vec<u8> {
        let mut packet = mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, sequence_number, b"");
        set_tcp_flags(&mut packet, flags);
        set_tcp_acknowledgement_number(&mut packet, acknowledgement_number);
        packet
    }

    fn mmds_tcp_empty_control_packet(
        sequence_number: u32,
        acknowledgement_number: u32,
        flags: u8,
    ) -> Vec<u8> {
        let mut packet = mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, sequence_number, b"");
        set_tcp_flags(&mut packet, flags);
        set_tcp_acknowledgement_number(&mut packet, acknowledgement_number);
        packet
    }

    fn arp_ipv4_request(target_ipv4_address: Ipv4Addr, operation: u16) -> Vec<u8> {
        arp_ipv4_request_from(TEST_SOURCE_IPV4_ADDRESS, target_ipv4_address, operation)
    }

    fn arp_ipv4_request_from(
        source_ipv4_address: Ipv4Addr,
        target_ipv4_address: Ipv4Addr,
        operation: u16,
    ) -> Vec<u8> {
        let mut packet = Vec::new();
        packet.extend_from_slice(&TEST_DESTINATION_ETHERNET_ADDRESS);
        packet.extend_from_slice(&TEST_SOURCE_ETHERNET_ADDRESS);
        packet.extend_from_slice(&ETHERNET_ETHERTYPE_ARP.to_be_bytes());
        packet.extend_from_slice(&ARP_HARDWARE_TYPE_ETHERNET.to_be_bytes());
        packet.extend_from_slice(&ARP_PROTOCOL_TYPE_IPV4.to_be_bytes());
        packet.push(6);
        packet.push(4);
        packet.extend_from_slice(&operation.to_be_bytes());
        packet.extend_from_slice(&TEST_SOURCE_ETHERNET_ADDRESS);
        packet.extend_from_slice(&source_ipv4_address.octets());
        packet.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        packet.extend_from_slice(&target_ipv4_address.octets());
        packet
    }

    fn mmds_arp_request() -> Vec<u8> {
        arp_ipv4_request(DEFAULT_MMDS_IPV4_ADDRESS, ARP_OPERATION_REQUEST)
    }

    fn v2_mmds_state_handle() -> MmdsStateHandle {
        let handle = MmdsStateHandle::default();
        let configured_network_interface =
            NetworkInterfaceConfigInput::new("eth0", "eth0", "vmnet:shared")
                .validate()
                .expect("network interface should validate");
        handle
            .with_mut(|state| {
                state.put_config(
                    MmdsConfigInput::new(vec!["eth0".to_string()]).with_version(MmdsVersion::V2),
                    &[configured_network_interface],
                )
            })
            .expect("MMDS state should lock")
            .expect("MMDS config should initialize");
        handle
    }

    fn mmds_response_frame_tcp_payload(response_frame: &[u8]) -> &[u8] {
        response_frame
            .get(ETHERNET_HEADER_LEN + IPV4_HEADER_LEN + TCP_HEADER_LEN..)
            .expect("MMDS response frame should include TCP payload")
    }

    fn ethernet_ethertype(frame: &[u8]) -> u16 {
        u16::from_be_bytes(
            frame[12..14]
                .try_into()
                .expect("Ethernet frame should include ethertype"),
        )
    }

    fn mmds_response_frame_acknowledgement_number(response_frame: &[u8]) -> u32 {
        let tcp = ETHERNET_HEADER_LEN + IPV4_HEADER_LEN;
        u32::from_be_bytes(
            response_frame[tcp + TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET
                ..tcp + TCP_ACKNOWLEDGEMENT_NUMBER_OFFSET + 4]
                .try_into()
                .expect("MMDS response frame should include TCP acknowledgement number"),
        )
    }

    fn mmds_response_frame_sequence_number(response_frame: &[u8]) -> u32 {
        let tcp = ETHERNET_HEADER_LEN + IPV4_HEADER_LEN;
        u32::from_be_bytes(
            response_frame[tcp + TCP_SEQUENCE_NUMBER_OFFSET..tcp + TCP_SEQUENCE_NUMBER_OFFSET + 4]
                .try_into()
                .expect("MMDS response frame should include TCP sequence number"),
        )
    }

    fn mmds_response_frame_tcp_flags(response_frame: &[u8]) -> u8 {
        let tcp = ETHERNET_HEADER_LEN + IPV4_HEADER_LEN;
        *response_frame
            .get(tcp + TCP_FLAGS_OFFSET)
            .expect("MMDS response frame should include TCP flags")
    }

    fn mmds_arp_reply_target_protocol_address(response_frame: &[u8]) -> Ipv4Addr {
        let arp = response_frame
            .get(ETHERNET_HEADER_LEN..)
            .expect("ARP reply should include payload");
        let octets: [u8; 4] = arp
            .get(ARP_TARGET_PROTOCOL_ADDRESS_OFFSET..ARP_TARGET_PROTOCOL_ADDRESS_OFFSET + 4)
            .expect("ARP reply should include target protocol address")
            .try_into()
            .expect("ARP target protocol address should be 4 bytes");
        Ipv4Addr::from(octets)
    }

    fn assert_mmds_arp_reply(response_frame: &[u8], target_ipv4_address: Ipv4Addr) {
        assert_eq!(response_frame.len(), ETHERNET_HEADER_LEN + ARP_HEADER_LEN);
        assert_eq!(
            response_frame
                .get(0..6)
                .expect("ARP reply should include destination MAC"),
            TEST_SOURCE_ETHERNET_ADDRESS
        );
        assert_eq!(
            response_frame
                .get(6..12)
                .expect("ARP reply should include source MAC"),
            DEFAULT_MMDS_MAC_ADDRESS.octets()
        );
        assert_eq!(ethernet_ethertype(response_frame), ETHERNET_ETHERTYPE_ARP);

        let arp = response_frame
            .get(ETHERNET_HEADER_LEN..)
            .expect("ARP reply should include payload");
        assert_eq!(
            u16::from_be_bytes(
                arp[ARP_OPERATION_OFFSET..ARP_OPERATION_OFFSET + 2]
                    .try_into()
                    .expect("ARP reply should include operation")
            ),
            ARP_OPERATION_REPLY
        );
        assert_eq!(
            arp.get(ARP_SENDER_HARDWARE_ADDRESS_OFFSET..ARP_SENDER_HARDWARE_ADDRESS_OFFSET + 6)
                .expect("ARP reply should include sender hardware address"),
            DEFAULT_MMDS_MAC_ADDRESS.octets()
        );
        assert_eq!(
            arp.get(ARP_SENDER_PROTOCOL_ADDRESS_OFFSET..ARP_SENDER_PROTOCOL_ADDRESS_OFFSET + 4)
                .expect("ARP reply should include sender protocol address"),
            target_ipv4_address.octets()
        );
        assert_eq!(
            arp.get(ARP_TARGET_HARDWARE_ADDRESS_OFFSET..ARP_TARGET_HARDWARE_ADDRESS_OFFSET + 6)
                .expect("ARP reply should include target hardware address"),
            TEST_SOURCE_ETHERNET_ADDRESS
        );
        assert_eq!(
            arp.get(ARP_TARGET_PROTOCOL_ADDRESS_OFFSET..ARP_TARGET_PROTOCOL_ADDRESS_OFFSET + 4)
                .expect("ARP reply should include target protocol address"),
            TEST_SOURCE_IPV4_ADDRESS.octets()
        );
    }

    fn mmds_response_body(response_frame: &[u8]) -> &[u8] {
        let response = mmds_response_frame_tcp_payload(response_frame);
        let separator = b"\r\n\r\n";
        let body_start = response
            .windows(separator.len())
            .position(|window| window == separator)
            .expect("HTTP response should include header terminator")
            + separator.len();
        &response[body_start..]
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

    fn packet_io_with_mmds_detour(
        backend: FakeVmnetPacketIoBackend,
        mmds_state: MmdsStateHandle,
        response_queue: MmdsResponseQueue,
    ) -> VmnetVirtioNetworkPacketIo<FakeVmnetPacketIoBackend> {
        packet_io_with_mmds_detour_and_metrics(
            backend,
            mmds_state,
            response_queue,
            SharedMmdsMetrics::default(),
        )
    }

    fn packet_io_with_mmds_detour_and_metrics(
        backend: FakeVmnetPacketIoBackend,
        mmds_state: MmdsStateHandle,
        response_queue: MmdsResponseQueue,
        metrics: SharedMmdsMetrics,
    ) -> VmnetVirtioNetworkPacketIo<FakeVmnetPacketIoBackend> {
        let detour = MmdsPacketDetour::new(
            mmds_state,
            DEFAULT_MMDS_IPV4_ADDRESS,
            response_queue,
            metrics,
        );
        VmnetVirtioNetworkPacketIo::with_mmds_detour(backend, fake_interface(), detour)
            .expect("packet I/O with MMDS detour should build")
    }

    fn mmds_only_packet_io(
        mmds_state: MmdsStateHandle,
        response_queue: MmdsResponseQueue,
    ) -> MmdsOnlyVirtioNetworkPacketIo {
        let detour = MmdsPacketDetour::new(
            mmds_state,
            DEFAULT_MMDS_IPV4_ADDRESS,
            response_queue,
            SharedMmdsMetrics::default(),
        );
        MmdsOnlyVirtioNetworkPacketIo::new(detour).expect("MMDS-only packet I/O should build")
    }

    fn push_mmds_response(response_queue: &MmdsResponseQueue, response: &[u8]) {
        response_queue
            .push_with(|| Ok(response.to_vec()))
            .expect("test MMDS response should queue");
    }

    fn poison_mmds_response_queue(response_queue: &MmdsResponseQueue) {
        response_queue.poison_for_test();
    }

    fn provider_entry(
        iface_id: &str,
        backend: FakeVmnetPacketIoBackend,
    ) -> VmnetVirtioNetworkPacketIoProviderEntry<FakeVmnetPacketIoBackend> {
        VmnetVirtioNetworkPacketIoProviderEntry::new(iface_id, packet_io(backend))
    }

    fn provider_entry_with_mmds_detour(
        iface_id: &str,
        backend: FakeVmnetPacketIoBackend,
        mmds_state: MmdsStateHandle,
        response_queue: MmdsResponseQueue,
    ) -> VmnetVirtioNetworkPacketIoProviderEntry<FakeVmnetPacketIoBackend> {
        VmnetVirtioNetworkPacketIoProviderEntry::new(
            iface_id,
            packet_io_with_mmds_detour(backend, mmds_state, response_queue),
        )
    }

    fn mmds_only_provider_entry(
        iface_id: &str,
        response_queue: MmdsResponseQueue,
    ) -> MmdsOnlyVirtioNetworkPacketIoProviderEntry {
        mmds_only_provider_entry_with_state(
            iface_id,
            MmdsStateHandle::default(),
            response_queue,
            SharedMmdsMetrics::default(),
        )
    }

    fn mmds_only_provider_entry_with_state(
        iface_id: &str,
        mmds_state: MmdsStateHandle,
        response_queue: MmdsResponseQueue,
        metrics: SharedMmdsMetrics,
    ) -> MmdsOnlyVirtioNetworkPacketIoProviderEntry {
        let detour = MmdsPacketDetour::new(
            mmds_state,
            DEFAULT_MMDS_IPV4_ADDRESS,
            response_queue,
            metrics,
        );
        MmdsOnlyVirtioNetworkPacketIoProviderEntry::new(
            iface_id,
            MmdsOnlyVirtioNetworkPacketIo::new(detour).expect("MMDS-only packet I/O should build"),
        )
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
    }

    #[test]
    fn tx_sink_flushes_external_packets_around_staged_mmds_effect() {
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let detour = MmdsPacketDetour::new(
            MmdsStateHandle::default(),
            DEFAULT_MMDS_IPV4_ADDRESS,
            response_queue.clone(),
            SharedMmdsMetrics::default(),
        );
        let mut packet_io = batch_packet_io(
            FakeVmnetPacketIoBackend::default(),
            2_048,
            1,
            3,
            Some(detour),
            None,
        );
        let mut memory = tx_memory();
        let first_external = [0x10, 0x11];
        let first_frame = tx_frame(&mut memory, &[(&first_external, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .stage_frame(&memory, &first_frame)
            .expect("first external packet should stage");
        assert_eq!(
            packet_io.tx_sink().commit_staged_frame(),
            VirtioNetworkTxPacketCommit::Deferred
        );

        let mmds_packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let mmds_frame = tx_frame(&mut memory, &[(&mmds_packet, SECOND_PAYLOAD_ADDRESS)]);
        assert_eq!(
            packet_io
                .tx_sink()
                .stage_frame(&memory, &mmds_frame)
                .expect("MMDS packet should stage"),
            VirtioNetworkTxPacketStage::Staged {
                flush_before_commit: true
            }
        );
        let mut first_flush = Vec::new();
        packet_io.tx_sink().flush_staged_frames(&mut first_flush);
        assert_eq!(
            first_flush,
            [Ok(VirtioNetworkTxPacketDisposition::Forwarded)]
        );
        assert_eq!(
            packet_io.tx_sink().commit_staged_frame(),
            VirtioNetworkTxPacketCommit::Immediate(Ok(VirtioNetworkTxPacketDisposition::Detoured))
        );

        let second_external = [0x20, 0x21, 0x22];
        let second_frame = tx_frame(&mut memory, &[(&second_external, THIRD_PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .stage_frame(&memory, &second_frame)
            .expect("second external packet should stage");
        assert_eq!(
            packet_io.tx_sink().commit_staged_frame(),
            VirtioNetworkTxPacketCommit::Deferred
        );
        let mut second_flush = Vec::new();
        packet_io.tx_sink().flush_staged_frames(&mut second_flush);
        assert_eq!(
            second_flush,
            [Ok(VirtioNetworkTxPacketDisposition::Forwarded)]
        );

        let shared = packet_io
            .tx_sink
            .shared
            .lock()
            .expect("test packet state should lock");
        assert_eq!(
            shared.backend.write_batches,
            [
                vec![first_external.to_vec()],
                vec![second_external.to_vec()]
            ]
        );
        drop(shared);
        assert_eq!(
            response_queue
                .responses()
                .expect("MMDS response queue should remain readable")
                .len(),
            1
        );
    }

    #[test]
    fn mmds_batch_classifier_matches_detour_for_packet_fixtures() {
        let mut acknowledgement = mmds_tcp_packet(b"");
        set_tcp_flags(&mut acknowledgement, TCP_FLAG_ACK);
        set_tcp_acknowledgement_number(&mut acknowledgement, MMDS_SYN_ACK_ACKNOWLEDGEMENT_NUMBER);
        let mut payload_without_ack = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        set_tcp_flags(&mut payload_without_ack, TCP_FLAG_PSH);
        let fixtures = [
            vec![0],
            tcp_ipv4_packet(
                Ipv4Addr::new(192, 0, 2, 99),
                MMDS_GUEST_TCP_PORT,
                b"GET /\r\n\r\n",
            ),
            arp_ipv4_request(Ipv4Addr::new(192, 0, 2, 99), ARP_OPERATION_REQUEST),
            mmds_arp_request(),
            mmds_tcp_syn_packet(7),
            acknowledgement,
            mmds_tcp_fin_close_packet(8, 1, TCP_FLAG_FIN | TCP_FLAG_ACK),
            mmds_tcp_empty_control_packet(9, 1, TCP_FLAG_RST | TCP_FLAG_ACK),
            mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n"),
            mmds_tcp_packet(b"GET /meta-data/host"),
            payload_without_ack,
        ];

        for (index, packet) in fixtures.iter().enumerate() {
            let queue = MmdsResponseQueue::with_capacity(4);
            let mut detour = MmdsPacketDetour::new(
                MmdsStateHandle::default(),
                DEFAULT_MMDS_IPV4_ADDRESS,
                queue,
                SharedMmdsMetrics::default(),
            );
            let classified = detour.would_detour_packet(packet);
            let actual = detour
                .detour_packet(packet)
                .unwrap_or_else(|error| panic!("fixture {index} should classify cleanly: {error}"));

            assert_eq!(
                classified, actual,
                "fixture {index} classifier result diverged from detour"
            );
        }
    }

    #[test]
    fn mmds_batch_classifier_has_no_queue_or_metric_side_effects() {
        let response_queue = MmdsResponseQueue::with_capacity(4);
        let metrics = SharedMmdsMetrics::default();
        let mut detour = MmdsPacketDetour::new(
            MmdsStateHandle::default(),
            DEFAULT_MMDS_IPV4_ADDRESS,
            response_queue.clone(),
            metrics.clone(),
        );
        let packets = [
            mmds_arp_request(),
            mmds_tcp_syn_packet(10),
            mmds_tcp_packet(b"GET /meta-data/host"),
            mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n"),
        ];

        for packet in &packets {
            assert!(detour.would_detour_packet(packet));
        }

        assert!(
            response_queue
                .responses()
                .expect("classifier should leave response queue readable")
                .is_empty()
        );
        assert_eq!(metrics.snapshot(), MmdsMetrics::default());

        assert!(
            detour
                .detour_packet(&mmds_tcp_syn_packet(10))
                .expect("actual SYN detour should succeed")
        );
        assert_eq!(
            response_queue
                .responses()
                .expect("actual detour should leave response queue readable")
                .len(),
            1
        );
        assert_ne!(metrics.snapshot(), MmdsMetrics::default());
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
            "vmnet TX frame exceeds the realized per-packet bound"
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
    fn tx_sink_detours_mmds_packet_and_retains_response_frame() {
        let payload = b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n";
        let packet = mmds_tcp_packet(payload);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        let disposition = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS TX should detour");

        assert_eq!(disposition, VirtioNetworkTxPacketDisposition::Detoured);
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        drop(state);

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert!(
            std::str::from_utf8(mmds_response_frame_tcp_payload(&responses[0]))
                .expect("response should be UTF-8")
                .starts_with("HTTP/1.1 400 Bad Request")
        );
        assert_eq!(
            mmds_response_body(&responses[0]),
            b"The MMDS data store is not initialized."
        );
    }

    #[test]
    fn tx_sink_records_mmds_guest_request_metrics() {
        let packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let metrics = SharedMmdsMetrics::default();
        let response_queue = MmdsResponseQueue::with_capacity(1);
        let mut packet_io = packet_io_with_mmds_detour_and_metrics(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue,
            metrics.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS GET should detour");

        assert_eq!(
            metrics.snapshot(),
            MmdsMetrics::default().with_rx_accepted(1).with_rx_count(1)
        );
    }

    #[test]
    fn rx_source_records_mmds_response_tx_metrics_on_consume() {
        let packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let metrics = SharedMmdsMetrics::default();
        let response_queue = MmdsResponseQueue::with_capacity(1);
        let mut packet_io = packet_io_with_mmds_detour_and_metrics(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue,
            metrics.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS GET should detour");
        let response_len = packet_io
            .rx_source()
            .peek_packet()
            .expect("MMDS response peek should succeed")
            .expect("MMDS response should be present")
            .bytes()
            .len();
        packet_io.rx_source().consume_packet();

        assert_eq!(
            metrics.snapshot(),
            MmdsMetrics::default()
                .with_rx_accepted(1)
                .with_rx_count(1)
                .with_tx_bytes(u64::try_from(response_len).expect("response length fits in u64"))
                .with_tx_count(1)
                .with_tx_frames(1)
        );
    }

    #[test]
    fn tx_sink_records_mmds_v2_guest_token_rejections() {
        let missing_token_packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let invalid_token_packet =
            mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\nX-metadata-token: bad\r\n\r\n");
        let mut memory = tx_memory();
        let missing_token_frame =
            tx_frame(&mut memory, &[(&missing_token_packet, PAYLOAD_ADDRESS)]);
        let invalid_token_frame = tx_frame(
            &mut memory,
            &[(&invalid_token_packet, SECOND_PAYLOAD_ADDRESS)],
        );
        let metrics = SharedMmdsMetrics::default();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour_and_metrics(
            FakeVmnetPacketIoBackend::default(),
            v2_mmds_state_handle(),
            response_queue,
            metrics.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &missing_token_frame)
            .expect("MMDS GET without token should detour");
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &invalid_token_frame)
            .expect("MMDS GET with invalid token should detour");

        assert_eq!(
            metrics.snapshot(),
            MmdsMetrics::default()
                .with_rx_accepted(2)
                .with_rx_invalid_token(1)
                .with_rx_no_token(1)
                .with_rx_count(2)
        );
    }

    #[test]
    fn packet_io_delivers_detoured_mmds_response_through_rx_source() {
        let payload = b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n";
        let packet = mmds_tcp_packet(payload);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0xaa]))),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS TX should detour");
        let response = packet_io
            .rx_source()
            .peek_packet()
            .expect("MMDS response RX should succeed")
            .expect("MMDS response should be present");

        assert!(
            std::str::from_utf8(mmds_response_frame_tcp_payload(response.bytes()))
                .expect("response should be UTF-8")
                .starts_with("HTTP/1.1 400 Bad Request")
        );
        assert_eq!(
            mmds_response_body(response.bytes()),
            b"The MMDS data store is not initialized."
        );
        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 0);
        drop(state);

        packet_io.rx_source().consume_packet();
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn mmds_only_packet_io_delivers_detoured_mmds_response_through_rx_source() {
        let payload = b"GET /meta-data/hostname HTTP/1.1\r\nAccept: application/json\r\n\r\n";
        let packet = mmds_tcp_packet(payload);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = mmds_only_packet_io(MmdsStateHandle::default(), response_queue.clone());

        assert!(!packet_io.rx_source.retry_after_tx_hint());
        let disposition = packet_io
            .tx_sink
            .transmit_frame(&memory, &frame)
            .expect("MMDS-only TX should detour");
        assert_eq!(disposition, VirtioNetworkTxPacketDisposition::Detoured);
        assert!(packet_io.rx_source.retry_after_tx_hint());
        let response = packet_io
            .rx_source
            .peek_packet()
            .expect("MMDS-only response RX should succeed")
            .expect("MMDS-only response should be present");

        assert!(
            std::str::from_utf8(mmds_response_frame_tcp_payload(response.bytes()))
                .expect("response should be UTF-8")
                .starts_with("HTTP/1.1 400 Bad Request")
        );
        assert_eq!(
            mmds_response_body(response.bytes()),
            b"The MMDS data store is not initialized."
        );

        packet_io.rx_source.consume_packet();
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_detours_mmds_arp_request_and_retains_reply_frame() {
        let packet = mmds_arp_request();
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS ARP request should detour");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        drop(state);
        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_mmds_arp_reply(&responses[0], DEFAULT_MMDS_IPV4_ADDRESS);
    }

    #[test]
    fn packet_io_delivers_detoured_mmds_arp_reply_through_rx_source() {
        let packet = mmds_arp_request();
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0xaa]))),
            MmdsStateHandle::default(),
            response_queue,
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS ARP request should detour");
        let response = packet_io
            .rx_source()
            .peek_packet()
            .expect("MMDS ARP reply RX should succeed")
            .expect("MMDS ARP reply should be present");

        assert_mmds_arp_reply(response.bytes(), DEFAULT_MMDS_IPV4_ADDRESS);
        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 0);
    }

    #[test]
    fn tx_sink_detours_only_configured_mmds_arp_ipv4_address() {
        let configured_mmds_address = Ipv4Addr::new(169, 254, 169, 253);
        let configured_packet = arp_ipv4_request(configured_mmds_address, ARP_OPERATION_REQUEST);
        let default_packet = mmds_arp_request();
        let mut memory = tx_memory();
        let configured_frame = tx_frame(&mut memory, &[(&configured_packet, PAYLOAD_ADDRESS)]);
        let default_frame = tx_frame(&mut memory, &[(&default_packet, SECOND_PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let detour = MmdsPacketDetour::new(
            MmdsStateHandle::default(),
            configured_mmds_address,
            response_queue.clone(),
            SharedMmdsMetrics::default(),
        );
        let mut packet_io = VmnetVirtioNetworkPacketIo::with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            fake_interface(),
            detour,
        )
        .expect("packet I/O with MMDS detour should build");

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &configured_frame)
            .expect("configured MMDS ARP request should detour");
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &default_frame)
            .expect("default MMDS ARP request should forward");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [default_packet]);
        drop(state);
        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_mmds_arp_reply(&responses[0], configured_mmds_address);
    }

    #[test]
    fn tx_sink_forwards_non_mmds_or_malformed_arp_when_detour_configured() {
        let wrong_target = arp_ipv4_request(Ipv4Addr::new(192, 0, 2, 99), ARP_OPERATION_REQUEST);
        let arp_reply = arp_ipv4_request(DEFAULT_MMDS_IPV4_ADDRESS, ARP_OPERATION_REPLY);
        let truncated = mmds_arp_request()
            .into_iter()
            .take(ETHERNET_HEADER_LEN + ARP_HEADER_LEN - 1)
            .collect::<Vec<_>>();
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        for packet in [&wrong_target, &arp_reply, &truncated] {
            let frame = tx_frame(&mut memory, &[(packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("non-MMDS ARP packet should forward");
        }

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 3);
        assert_eq!(
            state.backend.written_packets,
            [wrong_target, arp_reply, truncated]
        );
        drop(state);
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_mmds_arp_queue_overflow_does_not_mutate_token_state() {
        let packet = mmds_arp_request();
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(0);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue,
        );

        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect_err("queue overflow should fail MMDS ARP TX");

        assert!(
            error
                .message()
                .contains("MMDS packet detour failed: MMDS response queue is full at capacity 0")
        );
        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
    }

    #[test]
    fn tx_sink_prioritizes_mmds_arp_reply_before_queued_tcp_response() {
        let tcp_packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let arp_packet = mmds_arp_request();
        let mut memory = tx_memory();
        let tcp_frame = tx_frame(&mut memory, &[(&tcp_packet, PAYLOAD_ADDRESS)]);
        let arp_frame = tx_frame(&mut memory, &[(&arp_packet, SECOND_PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &tcp_frame)
            .expect("MMDS TCP request should detour");
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &arp_frame)
            .expect("MMDS ARP request should detour");

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 2);
        assert_eq!(ethernet_ethertype(&responses[0]), ETHERNET_ETHERTYPE_ARP);
        assert_mmds_arp_reply(&responses[0], DEFAULT_MMDS_IPV4_ADDRESS);
        assert_eq!(ethernet_ethertype(&responses[1]), ETHERNET_ETHERTYPE_IPV4);
        assert_eq!(
            mmds_response_body(&responses[1]),
            b"The MMDS data store is not initialized."
        );
    }

    #[test]
    fn tx_sink_preserves_mmds_arp_reply_order_before_queued_tcp_response() {
        let tcp_packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let first_arp_source_ipv4 = Ipv4Addr::new(192, 0, 2, 10);
        let second_arp_source_ipv4 = Ipv4Addr::new(192, 0, 2, 11);
        let first_arp_packet = arp_ipv4_request_from(
            first_arp_source_ipv4,
            DEFAULT_MMDS_IPV4_ADDRESS,
            ARP_OPERATION_REQUEST,
        );
        let second_arp_packet = arp_ipv4_request_from(
            second_arp_source_ipv4,
            DEFAULT_MMDS_IPV4_ADDRESS,
            ARP_OPERATION_REQUEST,
        );
        let mut memory = tx_memory();
        let tcp_frame = tx_frame(&mut memory, &[(&tcp_packet, PAYLOAD_ADDRESS)]);
        let first_arp_frame = tx_frame(&mut memory, &[(&first_arp_packet, SECOND_PAYLOAD_ADDRESS)]);
        let second_arp_frame =
            tx_frame(&mut memory, &[(&second_arp_packet, THIRD_PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(3);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &tcp_frame)
            .expect("MMDS TCP request should detour");
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &first_arp_frame)
            .expect("first MMDS ARP request should detour");
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &second_arp_frame)
            .expect("second MMDS ARP request should detour");

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 3);
        assert_eq!(ethernet_ethertype(&responses[0]), ETHERNET_ETHERTYPE_ARP);
        assert_eq!(
            mmds_arp_reply_target_protocol_address(&responses[0]),
            first_arp_source_ipv4
        );
        assert_eq!(ethernet_ethertype(&responses[1]), ETHERNET_ETHERTYPE_ARP);
        assert_eq!(
            mmds_arp_reply_target_protocol_address(&responses[1]),
            second_arp_source_ipv4
        );
        assert_eq!(ethernet_ethertype(&responses[2]), ETHERNET_ETHERTYPE_IPV4);
    }

    #[test]
    fn rx_source_retry_after_tx_hint_is_true_for_queued_mmds_response_without_vmnet_read() {
        let response_queue = MmdsResponseQueue::with_capacity(2);
        push_mmds_response(&response_queue, &[0xaa, 0xbb]);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0xcc]))),
            MmdsStateHandle::default(),
            response_queue,
        );

        assert!(packet_io.rx_source().retry_after_tx_hint());

        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 0);
    }

    #[test]
    fn rx_source_retry_after_tx_hint_is_false_for_empty_mmds_queue_without_vmnet_read() {
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0xcc]))),
            MmdsStateHandle::default(),
            response_queue,
        );

        assert!(!packet_io.rx_source().retry_after_tx_hint());

        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 0);
    }

    #[test]
    fn rx_source_retry_after_tx_hint_is_false_for_contended_mmds_queue_without_vmnet_read() {
        let response_queue = MmdsResponseQueue::with_capacity(2);
        push_mmds_response(&response_queue, &[0xaa, 0xbb]);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0xcc]))),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );
        response_queue
            .with_lock_for_test(|| {
                assert!(!packet_io.rx_source().retry_after_tx_hint());
                let state = packet_io
                    .rx_source()
                    .shared
                    .lock()
                    .expect("test state lock should succeed");
                assert_eq!(state.backend.read_calls, 0);
                drop(state);
            })
            .expect("test response queue lock should succeed");
        assert!(packet_io.rx_source().retry_after_tx_hint());
    }

    #[test]
    fn rx_source_retry_after_tx_hint_is_true_for_poisoned_mmds_queue() {
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0xcc]))),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        poison_mmds_response_queue(&response_queue);

        assert!(packet_io.rx_source().retry_after_tx_hint());
        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 0);
    }

    #[test]
    fn rx_source_retry_after_tx_hint_tracks_cached_packet() {
        let backend = FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0xaa])));
        let mut packet_io = packet_io(backend);

        assert!(!packet_io.rx_source().retry_after_tx_hint());
        assert!(
            packet_io
                .rx_source()
                .peek_packet()
                .expect("vmnet RX should succeed")
                .is_some()
        );
        assert!(packet_io.rx_source().retry_after_tx_hint());

        packet_io.rx_source().consume_packet();
        assert!(!packet_io.rx_source().retry_after_tx_hint());
    }

    #[test]
    fn tx_sink_detours_only_configured_mmds_ipv4_address() {
        let configured_mmds_address = Ipv4Addr::new(169, 254, 169, 253);
        let payload = b"GET /meta-data/hostname HTTP/1.1\r\n\r\n";
        let mut configured_packet =
            tcp_ipv4_packet(configured_mmds_address, MMDS_GUEST_TCP_PORT, payload);
        set_tcp_flags(&mut configured_packet, TCP_FLAG_PSH | TCP_FLAG_ACK);
        set_tcp_acknowledgement_number(&mut configured_packet, MMDS_SYN_ACK_ACKNOWLEDGEMENT_NUMBER);
        let default_packet = mmds_tcp_packet(payload);
        let mut memory = tx_memory();
        let configured_frame = tx_frame(&mut memory, &[(&configured_packet, PAYLOAD_ADDRESS)]);
        let default_frame = tx_frame(&mut memory, &[(&default_packet, SECOND_PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let detour = MmdsPacketDetour::new(
            MmdsStateHandle::default(),
            configured_mmds_address,
            response_queue.clone(),
            SharedMmdsMetrics::default(),
        );
        let mut packet_io = VmnetVirtioNetworkPacketIo::with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            fake_interface(),
            detour,
        )
        .expect("packet I/O with MMDS detour should build");

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &configured_frame)
            .expect("configured MMDS address TX should detour");
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &default_frame)
            .expect("default MMDS address TX should forward");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [default_packet]);
        drop(state);

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
    }

    #[test]
    fn tx_sink_forwards_non_mmds_packet_when_detour_configured() {
        let packet = tcp_ipv4_packet(
            Ipv4Addr::new(192, 0, 2, 99),
            MMDS_GUEST_TCP_PORT,
            b"GET /meta-data/hostname HTTP/1.1\r\n\r\n",
        );
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        let disposition = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("non-MMDS TX should forward");

        assert_eq!(disposition, VirtioNetworkTxPacketDisposition::Forwarded);
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [packet]);
        drop(state);
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_detours_mmds_syn_and_retains_syn_ack_frame() {
        let packet = mmds_tcp_syn_packet(u32::MAX);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS SYN should detour");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        drop(state);
        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_eq!(mmds_response_frame_sequence_number(&responses[0]), 0);
        assert_eq!(mmds_response_frame_acknowledgement_number(&responses[0]), 0);
        assert_eq!(
            mmds_response_frame_tcp_flags(&responses[0]),
            TCP_FLAG_SYN | TCP_FLAG_ACK
        );
        assert!(
            mmds_response_frame_tcp_payload(&responses[0]).is_empty(),
            "SYN-ACK should not carry MMDS HTTP payload"
        );
    }

    #[test]
    fn tx_sink_records_mmds_connection_metrics() {
        let syn_packet = mmds_tcp_syn_packet(0x0102_0304);
        let fin_packet =
            mmds_tcp_fin_close_packet(0x1112_1314, 0x2122_2324, TCP_FLAG_FIN | TCP_FLAG_ACK);
        let mut memory = tx_memory();
        let syn_frame = tx_frame(&mut memory, &[(&syn_packet, PAYLOAD_ADDRESS)]);
        let fin_frame = tx_frame(&mut memory, &[(&fin_packet, SECOND_PAYLOAD_ADDRESS)]);
        let metrics = SharedMmdsMetrics::default();
        let response_queue = MmdsResponseQueue::with_capacity(3);
        let mut packet_io = packet_io_with_mmds_detour_and_metrics(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue,
            metrics.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &syn_frame)
            .expect("MMDS SYN should detour");
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &fin_frame)
            .expect("MMDS FIN should detour");

        assert_eq!(
            metrics.snapshot(),
            MmdsMetrics::default()
                .with_rx_accepted(2)
                .with_rx_count(2)
                .with_connections_created(1)
                .with_connections_destroyed(1)
        );
    }

    #[test]
    fn packet_io_delivers_detoured_mmds_syn_ack_through_rx_source() {
        let packet = mmds_tcp_syn_packet(0x0102_0304);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0xaa]))),
            MmdsStateHandle::default(),
            response_queue,
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS SYN should detour");
        let response = packet_io
            .rx_source()
            .peek_packet()
            .expect("MMDS SYN-ACK RX should succeed")
            .expect("MMDS SYN-ACK should be present");

        assert_eq!(
            mmds_response_frame_acknowledgement_number(response.bytes()),
            0x0102_0305
        );
        assert_eq!(
            mmds_response_frame_tcp_flags(response.bytes()),
            TCP_FLAG_SYN | TCP_FLAG_ACK
        );
        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 0);
    }

    #[test]
    fn tx_sink_consumes_mmds_ack_only_without_response() {
        let mut packet = mmds_tcp_packet(b"");
        set_tcp_flags(&mut packet, TCP_FLAG_ACK);
        set_tcp_acknowledgement_number(&mut packet, 1);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS ACK-only TX should detour");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        assert!(state.backend.written_packets.is_empty());
        drop(state);
        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_forwards_mmds_ack_only_with_unexpected_acknowledgement() {
        let mut packet = mmds_tcp_packet(b"");
        set_tcp_flags(&mut packet, TCP_FLAG_ACK);
        set_tcp_acknowledgement_number(&mut packet, 0);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS ACK-only TX should forward unexpected acknowledgement");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [packet]);
        drop(state);
        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_forwards_mmds_payload_with_unexpected_acknowledgement() {
        let mut packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        set_tcp_acknowledgement_number(&mut packet, 0);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS payload with unexpected ACK should forward");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [packet]);
        drop(state);
        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_forwards_mmds_payload_without_ack_flag() {
        let mut packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        set_tcp_flags(&mut packet, TCP_FLAG_PSH);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS payload without ACK flag should forward");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [packet]);
        drop(state);
        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_detours_mmds_fin_close_packets_and_retains_close_frames() {
        let sequence_number = 0x0102_0304;
        let acknowledgement_number = 0x1112_1314;
        for flags in [TCP_FLAG_FIN, TCP_FLAG_FIN | TCP_FLAG_ACK] {
            let packet = mmds_tcp_fin_close_packet(sequence_number, acknowledgement_number, flags);
            let mut memory = tx_memory();
            let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
            let response_queue = MmdsResponseQueue::with_capacity(2);
            let mut packet_io = packet_io_with_mmds_detour(
                FakeVmnetPacketIoBackend::default(),
                MmdsStateHandle::default(),
                response_queue.clone(),
            );

            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("MMDS FIN close should detour");

            let state = packet_io
                .tx_sink()
                .shared
                .lock()
                .expect("test state lock should succeed");
            assert_eq!(state.backend.write_calls, 0);
            assert!(state.backend.written_packets.is_empty());
            drop(state);
            let responses = response_queue
                .responses()
                .expect("MMDS response queue should read");
            assert_eq!(responses.len(), 2);
            assert_eq!(mmds_response_frame_tcp_flags(&responses[0]), TCP_FLAG_ACK);
            assert_eq!(
                mmds_response_frame_tcp_flags(&responses[1]),
                TCP_FLAG_FIN | TCP_FLAG_ACK
            );
            for response in responses {
                assert_eq!(
                    mmds_response_frame_sequence_number(&response),
                    acknowledgement_number
                );
                assert_eq!(
                    mmds_response_frame_acknowledgement_number(&response),
                    sequence_number.wrapping_add(1)
                );
                assert!(
                    mmds_response_frame_tcp_payload(&response).is_empty(),
                    "MMDS FIN close response should not carry HTTP payload"
                );
            }
        }
    }

    #[test]
    fn tx_sink_detours_mmds_reset_candidate_and_retains_reset_frame() {
        let sequence_number = 0x0102_0304;
        let acknowledgement_number = 0x1112_1314;
        let packet = mmds_tcp_empty_control_packet(
            sequence_number,
            acknowledgement_number,
            TCP_FLAG_PSH | TCP_FLAG_ACK,
        );
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS reset candidate should detour");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        assert!(state.backend.written_packets.is_empty());
        drop(state);
        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            mmds_response_frame_sequence_number(&responses[0]),
            acknowledgement_number
        );
        assert_eq!(mmds_response_frame_acknowledgement_number(&responses[0]), 0);
        assert_eq!(mmds_response_frame_tcp_flags(&responses[0]), TCP_FLAG_RST);
        assert!(
            mmds_response_frame_tcp_payload(&responses[0]).is_empty(),
            "MMDS reset response should not carry HTTP payload"
        );
    }

    #[test]
    fn tx_sink_consumes_mmds_guest_rst_without_response() {
        let packet = mmds_tcp_empty_control_packet(0x0102_0304, 0x1112_1314, TCP_FLAG_RST);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS guest RST should detour without response");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        assert!(state.backend.written_packets.is_empty());
        drop(state);
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_consumes_payload_carrying_mmds_guest_rst_without_response() {
        for flags in [
            TCP_FLAG_RST,
            TCP_FLAG_RST | TCP_FLAG_ACK,
            TCP_FLAG_RST | TCP_FLAG_PSH,
        ] {
            let mut packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
            set_tcp_flags(&mut packet, flags);
            let mut memory = tx_memory();
            let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
            let response_queue = MmdsResponseQueue::with_capacity(2);
            let mmds_state = v2_mmds_state_handle();
            let state_before = mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock");
            let mut packet_io = packet_io_with_mmds_detour(
                FakeVmnetPacketIoBackend::default(),
                mmds_state.clone(),
                response_queue.clone(),
            );

            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("MMDS payload RST should detour without response");

            let state = packet_io
                .tx_sink()
                .shared
                .lock()
                .expect("test state lock should succeed");
            assert_eq!(state.backend.write_calls, 0);
            assert!(state.backend.written_packets.is_empty());
            drop(state);
            assert_eq!(
                mmds_state
                    .with(Clone::clone)
                    .expect("MMDS state should lock"),
                state_before
            );
            assert!(
                response_queue
                    .responses()
                    .expect("MMDS response queue should read")
                    .is_empty()
            );
        }
    }

    #[test]
    fn tx_sink_forwards_mmds_payload_control_flags_without_mmds_response() {
        for flags in [
            TCP_FLAG_SYN,
            TCP_FLAG_SYN | TCP_FLAG_ACK,
            TCP_FLAG_FIN,
            TCP_FLAG_FIN | TCP_FLAG_ACK,
            TCP_FLAG_FIN | TCP_FLAG_PSH | TCP_FLAG_ACK,
            TCP_FLAG_SYN | TCP_FLAG_FIN | TCP_FLAG_ACK,
        ] {
            let mut packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
            set_tcp_flags(&mut packet, flags);
            let mut memory = tx_memory();
            let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
            let response_queue = MmdsResponseQueue::with_capacity(2);
            let mmds_state = v2_mmds_state_handle();
            let state_before = mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock");
            let mut packet_io = packet_io_with_mmds_detour(
                FakeVmnetPacketIoBackend::default(),
                mmds_state.clone(),
                response_queue.clone(),
            );

            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("MMDS payload control packet should forward");

            let state = packet_io
                .tx_sink()
                .shared
                .lock()
                .expect("test state lock should succeed");
            assert_eq!(state.backend.write_calls, 1);
            assert_eq!(state.backend.written_packets, [packet]);
            drop(state);
            assert_eq!(
                mmds_state
                    .with(Clone::clone)
                    .expect("MMDS state should lock"),
                state_before
            );
            assert!(
                response_queue
                    .responses()
                    .expect("MMDS response queue should read")
                    .is_empty()
            );
        }
    }

    #[test]
    fn tx_sink_keeps_payload_carrying_mmds_packet_out_of_reset_path() {
        let mut packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        set_tcp_flags(&mut packet, TCP_FLAG_PSH | TCP_FLAG_ACK);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS payload request should detour");

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            mmds_response_frame_tcp_flags(&responses[0]),
            TCP_FLAG_PSH | TCP_FLAG_ACK
        );
        assert!(
            !mmds_response_frame_tcp_payload(&responses[0]).is_empty(),
            "MMDS payload request should receive HTTP response bytes, not a reset"
        );
    }

    #[test]
    fn tx_sink_reports_mmds_fin_close_queue_overflow_without_partial_enqueue() {
        let packet = mmds_tcp_fin_close_packet(0, 1, TCP_FLAG_FIN | TCP_FLAG_ACK);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(1);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect_err("FIN close queue overflow should fail TX");

        assert!(
            error
                .message()
                .contains("MMDS packet detour failed: MMDS response queue is full at capacity 1")
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        drop(state);
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_reports_mmds_reset_queue_overflow_without_vmnet_write() {
        let packet =
            mmds_tcp_empty_control_packet(0x0102_0304, 0x1112_1314, TCP_FLAG_PSH | TCP_FLAG_ACK);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(0);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect_err("MMDS reset queue overflow should fail TX");

        assert!(
            error
                .message()
                .contains("MMDS packet detour failed: MMDS response queue is full at capacity 0")
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        assert!(state.backend.written_packets.is_empty());
        drop(state);
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_reports_mmds_response_queue_overflow_without_vmnet_write() {
        let packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(0);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect_err("queue overflow should fail TX");

        assert!(
            error
                .message()
                .contains("MMDS packet detour failed: MMDS response queue is full at capacity 0")
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        drop(state);
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_reports_mmds_syn_queue_overflow_without_vmnet_write() {
        let packet = mmds_tcp_syn_packet(0);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(0);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect_err("SYN-ACK queue overflow should fail TX");

        assert!(
            error
                .message()
                .contains("MMDS packet detour failed: MMDS response queue is full at capacity 0")
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        drop(state);
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_mmds_queue_overflow_does_not_mutate_token_state() {
        let packet = mmds_tcp_packet(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        );
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(0);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect_err("queue overflow should fail TX");

        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
    }

    #[test]
    fn tx_sink_detour_token_put_mutates_shared_mmds_state() {
        let packet = mmds_tcp_packet(
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        );
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = v2_mmds_state_handle();
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS token PUT should detour");

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        let token =
            std::str::from_utf8(mmds_response_body(&responses[0])).expect("token should be UTF-8");
        assert_eq!(token.len(), 64);
        assert!(
            mmds_state
                .with(|state| state.is_guest_token_valid(token))
                .expect("MMDS state should lock")
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
    }

    #[test]
    fn tx_sink_buffers_split_mmds_get_until_request_header_complete() {
        let first_payload = b"GET /meta-data/host";
        let second_payload = b"name HTTP/1.1\r\n\r\n";
        let first_sequence_number = 0x1000;
        let second_sequence_number = first_sequence_number
            + u32::try_from(first_payload.len()).expect("test payload length should fit u32");
        let first_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, first_sequence_number, first_payload);
        let second_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            second_sequence_number,
            second_payload,
        );
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        let first_frame = tx_frame(&mut memory, &[(&first_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &first_frame)
            .expect("first MMDS split GET fragment should detour");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        drop(state);
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );

        let second_frame = tx_frame(&mut memory, &[(&second_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &second_frame)
            .expect("second MMDS split GET fragment should complete request");

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            mmds_response_body(&responses[0]),
            b"The MMDS data store is not initialized."
        );
        assert_eq!(
            mmds_response_frame_acknowledgement_number(&responses[0]),
            first_sequence_number
                + u32::try_from(first_payload.len() + second_payload.len())
                    .expect("test payload length should fit u32")
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
    }

    #[test]
    fn tx_sink_buffers_three_part_mmds_get_until_request_header_complete() {
        let first_payload = b"GET /meta-";
        let second_payload = b"data/host";
        let third_payload = b"name HTTP/1.1\r\n\r\n";
        let first_sequence_number = 0x1000;
        let second_sequence_number = first_sequence_number
            + u32::try_from(first_payload.len()).expect("test payload length should fit u32");
        let third_sequence_number = second_sequence_number
            + u32::try_from(second_payload.len()).expect("test payload length should fit u32");
        let first_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, first_sequence_number, first_payload);
        let second_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            second_sequence_number,
            second_payload,
        );
        let third_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, third_sequence_number, third_payload);
        let mut memory = tx_memory();
        let metrics = SharedMmdsMetrics::default();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour_and_metrics(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
            metrics.clone(),
        );

        for packet in [&first_packet, &second_packet, &third_packet] {
            let frame = tx_frame(&mut memory, &[(packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("MMDS split GET fragment should detour");
        }

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            mmds_response_body(&responses[0]),
            b"The MMDS data store is not initialized."
        );
        assert_eq!(
            metrics.snapshot(),
            MmdsMetrics::default().with_rx_accepted(3).with_rx_count(3)
        );
    }

    #[test]
    fn tx_sink_forwards_first_split_mmds_payload_with_unexpected_acknowledgement() {
        let mut packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, 0x1000, b"GET /meta-data/host");
        set_tcp_acknowledgement_number(&mut packet, 0);
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = MmdsStateHandle::default();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS first split payload with unexpected ACK should forward");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [packet]);
        drop(state);
        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn tx_sink_forwards_later_split_mmds_payload_with_unexpected_acknowledgement() {
        let first_payload = b"GET /meta-data/host";
        let second_payload = b"name HTTP/1.1\r\n\r\n";
        let first_sequence_number = 0x1000;
        let second_sequence_number = first_sequence_number
            + u32::try_from(first_payload.len()).expect("test payload length should fit u32");
        let first_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, first_sequence_number, first_payload);
        let mut invalid_second_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            second_sequence_number,
            second_payload,
        );
        set_tcp_acknowledgement_number(&mut invalid_second_packet, 0);
        let valid_second_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            second_sequence_number,
            second_payload,
        );
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = MmdsStateHandle::default();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        let first_frame = tx_frame(&mut memory, &[(&first_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &first_frame)
            .expect("first MMDS split GET fragment should detour");

        let invalid_second_frame =
            tx_frame(&mut memory, &[(&invalid_second_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &invalid_second_frame)
            .expect("MMDS later split payload with unexpected ACK should forward");

        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [invalid_second_packet]);
        drop(state);
        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );

        let valid_second_frame = tx_frame(&mut memory, &[(&valid_second_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &valid_second_frame)
            .expect("valid second MMDS split GET fragment should still complete request");

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            mmds_response_body(&responses[0]),
            b"The MMDS data store is not initialized."
        );
    }

    #[test]
    fn tx_sink_forwards_later_split_mmds_payload_control_without_buffer_append() {
        for flags in [TCP_FLAG_SYN | TCP_FLAG_ACK, TCP_FLAG_FIN | TCP_FLAG_ACK] {
            let first_payload = b"GET /meta-data/host";
            let second_payload = b"name HTTP/1.1\r\n\r\n";
            let first_sequence_number = 0x1000;
            let second_sequence_number = first_sequence_number
                + u32::try_from(first_payload.len()).expect("test payload length should fit u32");
            let first_packet = mmds_tcp_packet_from_source(
                TEST_SOURCE_TCP_PORT,
                first_sequence_number,
                first_payload,
            );
            let mut invalid_second_packet = mmds_tcp_packet_from_source(
                TEST_SOURCE_TCP_PORT,
                second_sequence_number,
                second_payload,
            );
            set_tcp_flags(&mut invalid_second_packet, flags);
            let valid_second_packet = mmds_tcp_packet_from_source(
                TEST_SOURCE_TCP_PORT,
                second_sequence_number,
                second_payload,
            );
            let mut memory = tx_memory();
            let response_queue = MmdsResponseQueue::with_capacity(2);
            let mut packet_io = packet_io_with_mmds_detour(
                FakeVmnetPacketIoBackend::default(),
                MmdsStateHandle::default(),
                response_queue.clone(),
            );

            let first_frame = tx_frame(&mut memory, &[(&first_packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &first_frame)
                .expect("first MMDS split GET fragment should detour");

            let invalid_second_frame =
                tx_frame(&mut memory, &[(&invalid_second_packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &invalid_second_frame)
                .expect("MMDS later split payload control should forward");

            let state = packet_io
                .tx_sink()
                .shared
                .lock()
                .expect("test state lock should succeed");
            assert_eq!(state.backend.write_calls, 1);
            assert_eq!(state.backend.written_packets, [invalid_second_packet]);
            drop(state);
            assert!(
                response_queue
                    .responses()
                    .expect("MMDS response queue should read")
                    .is_empty()
            );

            let valid_second_frame =
                tx_frame(&mut memory, &[(&valid_second_packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &valid_second_frame)
                .expect("valid second MMDS split GET fragment should still complete request");

            let responses = response_queue
                .responses()
                .expect("MMDS response queue should read");
            assert_eq!(responses.len(), 1);
            assert_eq!(
                mmds_response_body(&responses[0]),
                b"The MMDS data store is not initialized."
            );
        }
    }

    #[test]
    fn tx_sink_buffers_split_mmds_token_put_without_premature_state_mutation() {
        let first_payload = b"PUT /latest/api/token HTTP/1.1\r\n";
        let second_payload = b"X-metadata-token-ttl-seconds: 60\r\n\r\n";
        let first_packet = mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, 0, first_payload);
        let second_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            u32::try_from(first_payload.len()).expect("test payload length should fit u32"),
            second_payload,
        );
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        let first_frame = tx_frame(&mut memory, &[(&first_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &first_frame)
            .expect("first MMDS split token PUT fragment should detour");

        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );

        let second_frame = tx_frame(&mut memory, &[(&second_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &second_frame)
            .expect("second MMDS split token PUT fragment should complete request");

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        let token =
            std::str::from_utf8(mmds_response_body(&responses[0])).expect("token should be UTF-8");
        assert_eq!(token.len(), 64);
        assert!(
            mmds_state
                .with(|state| state.is_guest_token_valid(token))
                .expect("MMDS state should lock")
        );
    }

    #[test]
    fn tx_sink_rejects_mmds_sequence_gap_for_split_token_put_without_state_mutation() {
        let first_payload = b"PUT /latest/api/token HTTP/1.1\r\n";
        let second_payload = b"X-metadata-token-ttl-seconds: 60\r\n\r\n";
        let first_sequence_number = 0x1000;
        let expected_sequence_number = first_sequence_number
            + u32::try_from(first_payload.len()).expect("test payload length should fit u32");
        let actual_sequence_number = expected_sequence_number + 1;
        let first_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, first_sequence_number, first_payload);
        let skipped_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            actual_sequence_number,
            second_payload,
        );
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        let first_frame = tx_frame(&mut memory, &[(&first_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &first_frame)
            .expect("first MMDS split token PUT fragment should detour");

        let skipped_frame = tx_frame(&mut memory, &[(&skipped_packet, PAYLOAD_ADDRESS)]);
        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &skipped_frame)
            .expect_err("non-contiguous MMDS fragment should fail TX");

        assert!(error.message().contains(&format!(
            "expected TCP sequence number {expected_sequence_number}"
        )));
        assert!(
            error
                .message()
                .contains(&format!("received {actual_sequence_number}"))
        );
        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
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
    fn tx_sink_drops_mmds_sequence_buffer_after_duplicate_fragment() {
        let first_payload = b"GET /meta-data/host";
        let second_payload = b"name HTTP/1.1\r\n";
        let third_payload = b"\r\n";
        let first_sequence_number = 0x1000;
        let second_sequence_number = first_sequence_number
            + u32::try_from(first_payload.len()).expect("test payload length should fit u32");
        let third_sequence_number = second_sequence_number
            + u32::try_from(second_payload.len()).expect("test payload length should fit u32");
        let first_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, first_sequence_number, first_payload);
        let duplicate_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, first_sequence_number, first_payload);
        let second_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            second_sequence_number,
            second_payload,
        );
        let third_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, third_sequence_number, third_payload);
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        let first_frame = tx_frame(&mut memory, &[(&first_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &first_frame)
            .expect("first MMDS split GET fragment should detour");

        let duplicate_frame = tx_frame(&mut memory, &[(&duplicate_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &duplicate_frame)
            .expect_err("duplicate MMDS split fragment should fail TX");

        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );

        for packet in [&second_packet, &third_packet] {
            let frame = tx_frame(&mut memory, &[(packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("suffix after dropped buffer should become a new request");
        }

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            mmds_response_body(&responses[0]),
            b"MMDS guest HTTP request is malformed."
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
    fn tx_sink_buffers_split_mmds_sequence_wraparound() {
        let first_payload = b"GET /meta-data/host";
        let second_payload = b"name HTTP/1.1\r\n\r\n";
        let first_payload_len =
            u32::try_from(first_payload.len()).expect("test payload length should fit u32");
        let first_sequence_number = u32::MAX.wrapping_sub(first_payload_len).wrapping_add(1);
        let second_sequence_number = 0;
        let first_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, first_sequence_number, first_payload);
        let second_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            second_sequence_number,
            second_payload,
        );
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        for packet in [&first_packet, &second_packet] {
            let frame = tx_frame(&mut memory, &[(packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("MMDS split GET fragment should detour");
        }

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        assert_eq!(
            mmds_response_body(&responses[0]),
            b"The MMDS data store is not initialized."
        );
        assert_eq!(
            mmds_response_frame_acknowledgement_number(&responses[0]),
            first_sequence_number.wrapping_add(
                u32::try_from(first_payload.len() + second_payload.len())
                    .expect("test payload length should fit u32")
            )
        );
    }

    #[test]
    fn tx_sink_isolates_split_mmds_buffers_by_guest_connection() {
        let first_source_port = TEST_SOURCE_TCP_PORT;
        let second_source_port = TEST_SOURCE_TCP_PORT + 1;
        let first_request_prefix = b"GET /meta-data/host";
        let second_request_prefix = b"GET /meta-data/ami";
        let first_request_suffix = b"name HTTP/1.1\r\n\r\n";
        let second_request_suffix = b"-id HTTP/1.1\r\n\r\n";
        let first_prefix_packet =
            mmds_tcp_packet_from_source(first_source_port, 0, first_request_prefix);
        let second_prefix_packet =
            mmds_tcp_packet_from_source(second_source_port, 0, second_request_prefix);
        let first_suffix_packet = mmds_tcp_packet_from_source(
            first_source_port,
            u32::try_from(first_request_prefix.len()).expect("test payload length should fit u32"),
            first_request_suffix,
        );
        let second_suffix_packet = mmds_tcp_packet_from_source(
            second_source_port,
            u32::try_from(second_request_prefix.len()).expect("test payload length should fit u32"),
            second_request_suffix,
        );
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        for packet in [&first_prefix_packet, &second_prefix_packet] {
            let frame = tx_frame(&mut memory, &[(packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("MMDS split request prefix should detour");
        }

        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );

        for packet in [&first_suffix_packet, &second_suffix_packet] {
            let frame = tx_frame(&mut memory, &[(packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("MMDS split request suffix should complete request");
        }

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 2);
        for response in responses {
            assert_eq!(
                mmds_response_body(&response),
                b"The MMDS data store is not initialized."
            );
        }
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
    }

    #[test]
    fn tx_sink_mmds_request_buffer_overflow_drops_buffer_without_mutating_state() {
        let first_payload = b"PUT /latest/api/token HTTP/1.1\r\n";
        let oversized_payload = vec![b'a'; super::DEFAULT_MMDS_REQUEST_BUFFER_LEN_LIMIT];
        let first_packet = mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, 0, first_payload);
        let oversized_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            u32::try_from(first_payload.len()).expect("test payload length should fit u32"),
            &oversized_payload,
        );
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue.clone(),
        );

        let first_frame = tx_frame(&mut memory, &[(&first_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &first_frame)
            .expect("first MMDS oversized request fragment should detour");

        let oversized_frame = tx_frame(&mut memory, &[(&oversized_packet, PAYLOAD_ADDRESS)]);
        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &oversized_frame)
            .expect_err("oversized MMDS request buffer should fail TX");

        assert!(error.message().contains("MMDS request buffer length"));
        assert!(error.message().contains("exceeds limit"));
        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
        drop(state);

        let complete_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            0,
            b"PUT /latest/api/token HTTP/1.1\r\nX-metadata-token-ttl-seconds: 60\r\n\r\n",
        );
        let complete_frame = tx_frame(&mut memory, &[(&complete_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &complete_frame)
            .expect("same MMDS connection should accept a new complete request after overflow");

        let responses = response_queue
            .responses()
            .expect("MMDS response queue should read");
        assert_eq!(responses.len(), 1);
        let token =
            std::str::from_utf8(mmds_response_body(&responses[0])).expect("token should be UTF-8");
        assert!(
            mmds_state
                .with(|state| state.is_guest_token_valid(token))
                .expect("MMDS state should lock")
        );
    }

    #[test]
    fn tx_sink_mmds_request_buffer_rejects_too_many_partial_connections() {
        let partial_payload = b"GET /meta-data/host";
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        for index in 0..super::DEFAULT_MMDS_REQUEST_BUFFER_CAPACITY {
            let source_port = TEST_SOURCE_TCP_PORT
                + u16::try_from(index).expect("test source port offset should fit u16");
            let packet = mmds_tcp_packet_from_source(source_port, 0, partial_payload);
            let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
            packet_io
                .tx_sink()
                .transmit_frame(&memory, &frame)
                .expect("partial MMDS request should buffer");
        }

        let overflow_source_port = TEST_SOURCE_TCP_PORT
            + u16::try_from(super::DEFAULT_MMDS_REQUEST_BUFFER_CAPACITY)
                .expect("test source port offset should fit u16");
        let overflow_packet = mmds_tcp_packet_from_source(overflow_source_port, 0, partial_payload);
        let overflow_frame = tx_frame(&mut memory, &[(&overflow_packet, PAYLOAD_ADDRESS)]);
        let error = packet_io
            .tx_sink()
            .transmit_frame(&memory, &overflow_frame)
            .expect_err("too many buffered MMDS requests should fail TX");

        assert!(error.message().contains("MMDS request buffer is full"));
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
    }

    #[test]
    fn tx_sink_split_mmds_queue_overflow_does_not_mutate_token_state() {
        let first_payload = b"PUT /latest/api/token HTTP/1.1\r\n";
        let second_payload = b"X-metadata-token-ttl-seconds: 60\r\n\r\n";
        let first_packet = mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, 0, first_payload);
        let second_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            u32::try_from(first_payload.len()).expect("test payload length should fit u32"),
            second_payload,
        );
        let mut memory = tx_memory();
        let response_queue = MmdsResponseQueue::with_capacity(0);
        let mmds_state = v2_mmds_state_handle();
        let state_before = mmds_state
            .with(Clone::clone)
            .expect("MMDS state should lock");
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            mmds_state.clone(),
            response_queue,
        );

        let first_frame = tx_frame(&mut memory, &[(&first_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &first_frame)
            .expect("first MMDS split token PUT fragment should detour");
        let second_frame = tx_frame(&mut memory, &[(&second_packet, PAYLOAD_ADDRESS)]);
        packet_io
            .tx_sink()
            .transmit_frame(&memory, &second_frame)
            .expect_err("queue overflow should fail split token PUT TX");

        assert_eq!(
            mmds_state
                .with(Clone::clone)
                .expect("MMDS state should lock"),
            state_before
        );
        let state = packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.write_calls, 0);
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
    fn rx_source_prioritizes_mmds_response_before_vmnet_packet() {
        let response_queue = MmdsResponseQueue::with_capacity(2);
        push_mmds_response(&response_queue, &[0xaa, 0xbb]);
        let backend = FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0x33])));
        let mut packet_io =
            packet_io_with_mmds_detour(backend, MmdsStateHandle::default(), response_queue.clone());

        let response = packet_io
            .rx_source()
            .peek_packet()
            .expect("MMDS response peek should succeed")
            .expect("MMDS response should be present");
        assert_eq!(response.bytes(), &[0xaa, 0xbb]);
        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 0);
        drop(state);

        packet_io.rx_source().consume_packet();
        let vmnet_packet = packet_io
            .rx_source()
            .peek_packet()
            .expect("vmnet packet peek should succeed")
            .expect("vmnet packet should be present");
        assert_eq!(vmnet_packet.bytes(), &[0x33]);
        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("test state lock should succeed");
        assert_eq!(state.backend.read_calls, 1);
    }

    #[test]
    fn rx_source_caches_mmds_response_until_consumed() {
        let response_queue = MmdsResponseQueue::with_capacity(2);
        push_mmds_response(&response_queue, &[0x11]);
        push_mmds_response(&response_queue, &[0x22]);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );

        let first = packet_io
            .rx_source()
            .peek_packet()
            .expect("first MMDS peek should succeed")
            .expect("first MMDS response should be present");
        assert_eq!(first.bytes(), &[0x11]);
        let repeated = packet_io
            .rx_source()
            .peek_packet()
            .expect("repeated MMDS peek should succeed")
            .expect("cached MMDS response should be present");
        assert_eq!(repeated.bytes(), &[0x11]);
        assert_eq!(
            response_queue
                .responses()
                .expect("MMDS response queue should read"),
            [vec![0x11], vec![0x22]]
        );

        packet_io.rx_source().consume_packet();
        let second = packet_io
            .rx_source()
            .peek_packet()
            .expect("second MMDS peek should succeed")
            .expect("second MMDS response should be present");
        assert_eq!(second.bytes(), &[0x22]);
        assert_eq!(
            response_queue
                .responses()
                .expect("MMDS response queue should read"),
            [vec![0x22]]
        );
    }

    #[test]
    fn rx_source_rejects_oversized_mmds_response_without_dequeueing() {
        let response_queue = MmdsResponseQueue::with_capacity(1);
        push_mmds_response(&response_queue, &[0xaa, 0xbb, 0xcc]);
        let prepared = super::PreparedVmnetVirtioNetworkRxBuffer::with_len(2)
            .expect("small prepared RX buffer should allocate");
        let mut packet_io = VmnetVirtioNetworkPacketIo::with_prepared_rx_buffer_and_mmds_detour(
            FakeVmnetPacketIoBackend::default(),
            fake_interface(),
            prepared,
            2,
            Some(MmdsPacketDetour::new(
                MmdsStateHandle::default(),
                DEFAULT_MMDS_IPV4_ADDRESS,
                response_queue.clone(),
                SharedMmdsMetrics::default(),
            )),
        )
        .expect("packet I/O with MMDS detour should build");

        let error = packet_io
            .rx_source()
            .peek_packet()
            .expect_err("oversized MMDS response should fail");

        assert!(error.message().contains(
            "MMDS response queue read failed: MMDS response frame length 3 exceeds RX buffer length 2"
        ));
        assert_eq!(
            response_queue
                .responses()
                .expect("MMDS response queue should read"),
            [vec![0xaa, 0xbb, 0xcc]]
        );
    }

    #[test]
    fn rx_source_reports_poisoned_mmds_response_queue() {
        let response_queue = MmdsResponseQueue::with_capacity(1);
        let mut packet_io = packet_io_with_mmds_detour(
            FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0x10]))),
            MmdsStateHandle::default(),
            response_queue.clone(),
        );
        poison_mmds_response_queue(&response_queue);

        let error = packet_io
            .rx_source()
            .peek_packet()
            .expect_err("poisoned MMDS response queue should fail before vmnet read");

        assert!(
            error
                .message()
                .contains("MMDS response queue read failed: MMDS response queue lock is poisoned")
        );
        let state = packet_io
            .rx_source()
            .shared
            .lock()
            .expect("vmnet state lock should not be poisoned");
        assert_eq!(state.backend.read_calls, 0);
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
    fn mmds_only_provider_rejects_duplicate_interface_ids() {
        let response_queue = MmdsResponseQueue::with_capacity(1);
        let error = MmdsOnlyVirtioNetworkPacketIoProvider::new(vec![
            mmds_only_provider_entry("eth0", response_queue.clone()),
            mmds_only_provider_entry("eth0", response_queue),
        ])
        .expect_err("duplicate MMDS-only interface ids should fail");

        assert!(matches!(
            error,
            MmdsOnlyVirtioNetworkPacketIoProviderBuildError::DuplicateInterfaceId { ref iface_id }
                if iface_id == "eth0"
        ));
    }

    #[test]
    fn mmds_only_provider_reports_missing_interface_id() {
        let mut provider =
            MmdsOnlyVirtioNetworkPacketIoProvider::new(vec![mmds_only_provider_entry(
                "eth0",
                MmdsResponseQueue::with_capacity(1),
            )])
            .expect("MMDS-only provider should build");
        let device = network_device("eth1");

        let error = provider
            .packet_io(device.interface())
            .expect_err("missing MMDS-only provider entry should fail");

        assert_eq!(
            error.message(),
            "missing MMDS-only packet I/O for interface eth1"
        );
    }

    #[test]
    fn mmds_only_provider_keeps_split_requests_and_responses_on_their_interface() {
        let first_request_prefix = b"GET /meta-data/host";
        let request_suffix = b"name HTTP/1.1\r\n\r\n";
        let suffix_sequence_number = u32::try_from(first_request_prefix.len())
            .expect("test request prefix length should fit u32");
        let first_prefix_packet =
            mmds_tcp_packet_from_source(TEST_SOURCE_TCP_PORT, 0, first_request_prefix);
        let request_suffix_packet = mmds_tcp_packet_from_source(
            TEST_SOURCE_TCP_PORT,
            suffix_sequence_number,
            request_suffix,
        );
        let mut memory = tx_memory();
        let shared_state = MmdsStateHandle::default();
        let shared_metrics = SharedMmdsMetrics::default();
        let eth0_response_queue = MmdsResponseQueue::with_capacity(2);
        let eth1_response_queue = MmdsResponseQueue::with_capacity(2);
        let mut provider = MmdsOnlyVirtioNetworkPacketIoProvider::new(vec![
            mmds_only_provider_entry_with_state(
                "eth0",
                shared_state.clone(),
                eth0_response_queue.clone(),
                shared_metrics.clone(),
            ),
            mmds_only_provider_entry_with_state(
                "eth1",
                shared_state,
                eth1_response_queue.clone(),
                shared_metrics,
            ),
        ])
        .expect("two-interface MMDS-only provider should build");

        let first_prefix_frame = tx_frame(&mut memory, &[(&first_prefix_packet, PAYLOAD_ADDRESS)]);
        provider.entries[0]
            .packet_io
            .tx_sink
            .transmit_frame(&memory, &first_prefix_frame)
            .expect("eth0 split request prefix should detour");

        assert!(
            eth0_response_queue
                .responses()
                .expect("eth0 response queue should read")
                .is_empty()
        );
        assert!(
            eth1_response_queue
                .responses()
                .expect("eth1 response queue should read")
                .is_empty()
        );

        let peer_suffix_frame = tx_frame(
            &mut memory,
            &[(&request_suffix_packet, SECOND_PAYLOAD_ADDRESS)],
        );
        provider.entries[1]
            .packet_io
            .tx_sink
            .transmit_frame(&memory, &peer_suffix_frame)
            .expect("eth1 peer suffix should detour independently");

        assert!(
            eth0_response_queue
                .responses()
                .expect("eth0 response queue should read")
                .is_empty(),
            "eth1 suffix must not complete eth0 buffered request"
        );
        let eth1_responses = eth1_response_queue
            .responses()
            .expect("eth1 response queue should read");
        assert_eq!(eth1_responses.len(), 1);
        assert_eq!(
            mmds_response_body(&eth1_responses[0]),
            b"MMDS guest HTTP request is malformed."
        );

        let matching_suffix_frame = tx_frame(
            &mut memory,
            &[(&request_suffix_packet, THIRD_PAYLOAD_ADDRESS)],
        );
        provider.entries[0]
            .packet_io
            .tx_sink
            .transmit_frame(&memory, &matching_suffix_frame)
            .expect("eth0 matching suffix should complete buffered request");

        let eth0_responses = eth0_response_queue
            .responses()
            .expect("eth0 response queue should read");
        assert_eq!(eth0_responses.len(), 1);
        assert_eq!(
            mmds_response_body(&eth0_responses[0]),
            b"The MMDS data store is not initialized."
        );
        assert_eq!(
            eth1_response_queue
                .responses()
                .expect("eth1 response queue should read"),
            eth1_responses,
            "eth0 completion must not mutate eth1 response queue"
        );

        {
            let eth0_packet = provider.entries[0]
                .packet_io
                .rx_source
                .peek_packet()
                .expect("eth0 RX should succeed")
                .expect("eth0 MMDS response should exist");
            assert_eq!(
                mmds_response_body(eth0_packet.bytes()),
                b"The MMDS data store is not initialized."
            );
            provider.entries[0].packet_io.rx_source.consume_packet();
        }
        assert!(
            eth0_response_queue
                .responses()
                .expect("eth0 response queue should read")
                .is_empty()
        );
        assert_eq!(
            eth1_response_queue
                .responses()
                .expect("eth1 response queue should read"),
            eth1_responses,
            "consuming eth0 RX must not consume eth1 response"
        );

        {
            let eth1_packet = provider.entries[1]
                .packet_io
                .rx_source
                .peek_packet()
                .expect("eth1 RX should succeed")
                .expect("eth1 MMDS response should exist");
            assert_eq!(
                mmds_response_body(eth1_packet.bytes()),
                b"MMDS guest HTTP request is malformed."
            );
            provider.entries[1].packet_io.rx_source.consume_packet();
        }
        assert!(
            eth1_response_queue
                .responses()
                .expect("eth1 response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn provider_forwards_mmds_packet_on_interface_without_detour() {
        let packet = mmds_tcp_packet(b"GET /meta-data/hostname HTTP/1.1\r\n\r\n");
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&packet, PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let mut provider = VmnetVirtioNetworkPacketIoProvider::new(vec![
            provider_entry_with_mmds_detour(
                "eth0",
                FakeVmnetPacketIoBackend::default(),
                MmdsStateHandle::default(),
                response_queue.clone(),
            ),
            provider_entry("eth1", FakeVmnetPacketIoBackend::default()),
        ])
        .expect("provider should build");

        let eth1 = provider
            .entries
            .iter_mut()
            .find(|entry| entry.iface_id() == "eth1")
            .expect("eth1 entry should exist");
        eth1.packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("eth1 TX should forward");

        let eth1_state = eth1
            .packet_io
            .tx_sink()
            .shared
            .lock()
            .expect("eth1 state lock should succeed");
        assert_eq!(eth1_state.backend.write_calls, 1);
        assert_eq!(eth1_state.backend.written_packets, [packet]);
        assert!(
            response_queue
                .responses()
                .expect("MMDS response queue should read")
                .is_empty()
        );
    }

    #[test]
    fn provider_keeps_mmds_response_delivery_on_configured_interface() {
        let response_queue = MmdsResponseQueue::with_capacity(2);
        push_mmds_response(&response_queue, &[0xa0]);
        let mut provider = VmnetVirtioNetworkPacketIoProvider::new(vec![
            provider_entry_with_mmds_detour(
                "eth0",
                FakeVmnetPacketIoBackend::default(),
                MmdsStateHandle::default(),
                response_queue.clone(),
            ),
            provider_entry(
                "eth1",
                FakeVmnetPacketIoBackend::default().with_read_result(Ok(Some(vec![0xb0]))),
            ),
        ])
        .expect("provider should build");

        {
            let eth1 = provider
                .entries
                .iter_mut()
                .find(|entry| entry.iface_id() == "eth1")
                .expect("eth1 entry should exist");
            let packet = eth1
                .packet_io
                .rx_source()
                .peek_packet()
                .expect("eth1 RX should succeed")
                .expect("eth1 vmnet packet should exist");
            assert_eq!(packet.bytes(), &[0xb0]);
        }
        assert_eq!(
            response_queue
                .responses()
                .expect("MMDS response queue should read"),
            [vec![0xa0]]
        );

        {
            let eth0 = provider
                .entries
                .iter_mut()
                .find(|entry| entry.iface_id() == "eth0")
                .expect("eth0 entry should exist");
            let packet = eth0
                .packet_io
                .rx_source()
                .peek_packet()
                .expect("eth0 RX should succeed")
                .expect("eth0 MMDS response should exist");
            assert_eq!(packet.bytes(), &[0xa0]);
        }
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
