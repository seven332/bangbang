//! Adapters between internal virtio-net packet traits and vmnet packet I/O.

use std::collections::{TryReserveError, VecDeque};
use std::fmt;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, MutexGuard};

use bangbang_runtime::memory::{GuestMemory, GuestMemoryAccessError};
use bangbang_runtime::mmds::{MmdsStateHandle, MmdsStateLockError, classify_mmds_guest_tcp_packet};
use bangbang_runtime::network::{
    VIRTIO_NET_MAX_BUFFER_SIZE, VirtioNetworkRxPacket, VirtioNetworkRxPacketSource,
    VirtioNetworkRxPacketSourceError, VirtioNetworkTxFrame, VirtioNetworkTxPacketSink,
    VirtioNetworkTxPacketSinkError,
};
use bangbang_runtime::startup::{
    Arm64BootNetworkDevice, Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError,
    Arm64BootNetworkPacketIoProvider,
};

use crate::host_network::vmnet::{
    VmnetPacketDescriptorError, VmnetPacketIoBackend, VmnetPacketIoError, VmnetReadPacket,
    VmnetWritePacket,
};

pub const DEFAULT_VMNET_VIRTIO_NETWORK_RX_BUFFER_LEN: usize = VIRTIO_NET_MAX_BUFFER_SIZE as usize;
pub(crate) const DEFAULT_MMDS_RESPONSE_QUEUE_CAPACITY: usize = 64;

#[derive(Debug)]
pub enum VmnetVirtioNetworkPacketIoBuildError {
    EmptyRxBuffer,
    RxBufferAllocation { len: usize, source: TryReserveError },
}

impl fmt::Display for VmnetVirtioNetworkPacketIoBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRxBuffer => f.write_str("vmnet virtio-net RX buffer must not be empty"),
            Self::RxBufferAllocation { len, source } => {
                write!(
                    f,
                    "failed to reserve vmnet virtio-net RX buffer of {len} bytes: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VmnetVirtioNetworkPacketIoBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RxBufferAllocation { source, .. } => Some(source),
            Self::EmptyRxBuffer => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MmdsPacketDetour {
    mmds_state: MmdsStateHandle,
    mmds_ipv4_address: Ipv4Addr,
    response_queue: MmdsResponseQueue,
}

impl MmdsPacketDetour {
    pub(crate) fn new(
        mmds_state: MmdsStateHandle,
        mmds_ipv4_address: Ipv4Addr,
        response_queue: MmdsResponseQueue,
    ) -> Self {
        Self {
            mmds_state,
            mmds_ipv4_address,
            response_queue,
        }
    }

    fn detour_packet(&self, packet: &[u8]) -> Result<bool, MmdsPacketDetourError> {
        let Some(classified) = classify_mmds_guest_tcp_packet(packet, self.mmds_ipv4_address)
        else {
            return Ok(false);
        };
        if classified.payload().is_empty() {
            return Ok(false);
        }

        self.response_queue.push_with(|| {
            self.mmds_state
                .with_mut(|state| state.guest_http_response_bytes(classified.payload()))
        })?;
        Ok(true)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MmdsResponseQueue {
    state: Arc<Mutex<MmdsResponseQueueState>>,
}

impl Default for MmdsResponseQueue {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_MMDS_RESPONSE_QUEUE_CAPACITY)
    }
}

impl MmdsResponseQueue {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(MmdsResponseQueueState {
                capacity,
                responses: VecDeque::new(),
            })),
        }
    }

    fn push_with(
        &self,
        response: impl FnOnce() -> Result<Vec<u8>, MmdsStateLockError>,
    ) -> Result<(), MmdsPacketDetourError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| MmdsPacketDetourError::ResponseQueue(MmdsResponseQueueError::Poisoned))?;
        if state.responses.len() >= state.capacity {
            return Err(MmdsPacketDetourError::ResponseQueue(
                MmdsResponseQueueError::Full {
                    capacity: state.capacity,
                },
            ));
        }

        let response = response().map_err(MmdsPacketDetourError::MmdsState)?;
        state.responses.push_back(response);
        Ok(())
    }

    #[cfg(test)]
    fn responses(&self) -> Result<Vec<Vec<u8>>, MmdsResponseQueueError> {
        let state = self
            .state
            .lock()
            .map_err(|_| MmdsResponseQueueError::Poisoned)?;
        Ok(state.responses.iter().cloned().collect())
    }
}

#[derive(Debug)]
struct MmdsResponseQueueState {
    capacity: usize,
    responses: VecDeque<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MmdsResponseQueueError {
    Full { capacity: usize },
    Poisoned,
}

impl fmt::Display for MmdsResponseQueueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full { capacity } => {
                write!(f, "MMDS response queue is full at capacity {capacity}")
            }
            Self::Poisoned => f.write_str("MMDS response queue lock is poisoned"),
        }
    }
}

impl std::error::Error for MmdsResponseQueueError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MmdsPacketDetourError {
    MmdsState(MmdsStateLockError),
    ResponseQueue(MmdsResponseQueueError),
}

impl fmt::Display for MmdsPacketDetourError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MmdsState(source) => write!(f, "{source}"),
            Self::ResponseQueue(source) => write!(f, "{source}"),
        }
    }
}

impl std::error::Error for MmdsPacketDetourError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MmdsState(source) => Some(source),
            Self::ResponseQueue(source) => Some(source),
        }
    }
}

#[derive(Debug)]
pub struct VmnetVirtioNetworkPacketIo<B>
where
    B: VmnetPacketIoBackend,
{
    tx_sink: VmnetVirtioNetworkTxPacketSink<B>,
    rx_source: VmnetVirtioNetworkRxPacketSource<B>,
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
        Self::with_rx_buffer_len_and_mmds_detour(backend, interface, rx_buffer_len, None)
    }

    pub(crate) fn with_mmds_detour(
        backend: B,
        interface: B::Interface,
        mmds_detour: MmdsPacketDetour,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        Self::with_rx_buffer_len_and_mmds_detour(
            backend,
            interface,
            DEFAULT_VMNET_VIRTIO_NETWORK_RX_BUFFER_LEN,
            Some(mmds_detour),
        )
    }

    fn with_rx_buffer_len_and_mmds_detour(
        backend: B,
        interface: B::Interface,
        rx_buffer_len: usize,
        mmds_detour: Option<MmdsPacketDetour>,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        let shared = Arc::new(Mutex::new(VmnetVirtioNetworkPacketIoState {
            backend,
            interface,
        }));

        Ok(Self {
            tx_sink: VmnetVirtioNetworkTxPacketSink::new(Arc::clone(&shared), mmds_detour),
            rx_source: VmnetVirtioNetworkRxPacketSource::new(shared, rx_buffer_len)?,
        })
    }

    pub fn tx_sink(&mut self) -> &mut VmnetVirtioNetworkTxPacketSink<B> {
        &mut self.tx_sink
    }

    pub fn rx_source(&mut self) -> &mut VmnetVirtioNetworkRxPacketSource<B> {
        &mut self.rx_source
    }
}

#[derive(Debug)]
pub struct VmnetVirtioNetworkPacketIoProviderEntry<B>
where
    B: VmnetPacketIoBackend,
{
    iface_id: String,
    packet_io: VmnetVirtioNetworkPacketIo<B>,
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

#[derive(Debug)]
pub struct VmnetVirtioNetworkPacketIoProvider<B>
where
    B: VmnetPacketIoBackend,
{
    entries: Vec<VmnetVirtioNetworkPacketIoProviderEntry<B>>,
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
    fn packet_io(
        &mut self,
        device: &Arm64BootNetworkDevice,
    ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
        let iface_id = device.registration.iface_id();
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.iface_id == iface_id)
        else {
            return Err(Arm64BootNetworkPacketIoError::new(format!(
                "missing vmnet packet I/O for interface {iface_id}"
            )));
        };

        let VmnetVirtioNetworkPacketIo { tx_sink, rx_source } = &mut entry.packet_io;
        Ok(Arm64BootNetworkPacketIo::new(tx_sink, rx_source))
    }
}

#[derive(Debug)]
struct VmnetVirtioNetworkPacketIoState<B>
where
    B: VmnetPacketIoBackend,
{
    backend: B,
    interface: B::Interface,
}

#[derive(Debug)]
pub struct VmnetVirtioNetworkTxPacketSink<B>
where
    B: VmnetPacketIoBackend,
{
    shared: Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
    mmds_detour: Option<MmdsPacketDetour>,
}

impl<B> VmnetVirtioNetworkTxPacketSink<B>
where
    B: VmnetPacketIoBackend,
{
    fn new(
        shared: Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
        mmds_detour: Option<MmdsPacketDetour>,
    ) -> Self {
        Self {
            shared,
            mmds_detour,
        }
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
    ) -> Result<(), VirtioNetworkTxPacketSinkError> {
        let packet = copy_tx_frame_payload(memory, frame).map_err(tx_error)?;
        if let Some(mmds_detour) = &self.mmds_detour
            && mmds_detour
                .detour_packet(&packet)
                .map_err(tx_mmds_detour_error)?
        {
            return Ok(());
        }

        let mut packet = VmnetWritePacket::new(&packet).map_err(tx_descriptor_error)?;
        let mut state = lock_state_for_tx(&self.shared)?;
        let VmnetVirtioNetworkPacketIoState { backend, interface } = &mut *state;

        backend
            .write_packet(interface, &mut packet)
            .map_err(tx_vmnet_error)
    }
}

#[derive(Debug)]
pub struct VmnetVirtioNetworkRxPacketSource<B>
where
    B: VmnetPacketIoBackend,
{
    shared: Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
    read_buffer: Vec<u8>,
    cached_packet_len: Option<usize>,
}

impl<B> VmnetVirtioNetworkRxPacketSource<B>
where
    B: VmnetPacketIoBackend,
{
    fn new(
        shared: Arc<Mutex<VmnetVirtioNetworkPacketIoState<B>>>,
        rx_buffer_len: usize,
    ) -> Result<Self, VmnetVirtioNetworkPacketIoBuildError> {
        if rx_buffer_len == 0 {
            return Err(VmnetVirtioNetworkPacketIoBuildError::EmptyRxBuffer);
        }

        let mut read_buffer = Vec::new();
        read_buffer
            .try_reserve_exact(rx_buffer_len)
            .map_err(
                |source| VmnetVirtioNetworkPacketIoBuildError::RxBufferAllocation {
                    len: rx_buffer_len,
                    source,
                },
            )?;
        read_buffer.resize(rx_buffer_len, 0);

        Ok(Self {
            shared,
            read_buffer,
            cached_packet_len: None,
        })
    }

    fn cached_packet(&self) -> Option<VirtioNetworkRxPacket<'_>> {
        let len = self.cached_packet_len?;
        self.read_buffer.get(..len).map(VirtioNetworkRxPacket::new)
    }
}

impl<B> VirtioNetworkRxPacketSource for VmnetVirtioNetworkRxPacketSource<B>
where
    B: VmnetPacketIoBackend,
{
    fn peek_packet(
        &mut self,
    ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
        if self.cached_packet_len.is_some() {
            return Ok(self.cached_packet());
        }

        let packet_len = {
            let mut packet = VmnetReadPacket::new(&mut self.read_buffer).map_err(rx_error)?;
            let mut state = lock_state_for_rx(&self.shared)?;
            let VmnetVirtioNetworkPacketIoState { backend, interface } = &mut *state;

            backend
                .read_packet(interface, &mut packet)
                .map_err(rx_vmnet_error)?
        };
        if let Some(len) = packet_len {
            validate_rx_packet_len(len, self.read_buffer.len())?;
        }

        self.cached_packet_len = packet_len;
        Ok(self.cached_packet())
    }

    fn consume_packet(&mut self) {
        self.cached_packet_len = None;
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

    Ok(packet)
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

fn rx_error(source: VmnetPacketDescriptorError) -> VirtioNetworkRxPacketSourceError {
    VirtioNetworkRxPacketSourceError::new(format!(
        "failed to build vmnet RX packet descriptor: {source}"
    ))
}

fn rx_vmnet_error(source: VmnetPacketIoError) -> VirtioNetworkRxPacketSourceError {
    VirtioNetworkRxPacketSourceError::new(format!("vmnet RX packet read failed: {source}"))
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

    use bangbang_runtime::fdt::{Arm64FdtRegion, Arm64FdtVirtioMmioDevice};
    use bangbang_runtime::interrupt::GuestInterruptLine;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::mmds::{
        DEFAULT_MMDS_IPV4_ADDRESS, MMDS_GUEST_TCP_PORT, MmdsConfigInput, MmdsStateHandle,
        MmdsVersion,
    };
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::{
        NetworkInterfaceConfigInput, NetworkInterfaceConfigs, NetworkMmioLayout,
        PreparedNetworkDevices, VIRTIO_NET_TX_HEADER_SIZE, VirtioNetworkRxPacketSource,
        VirtioNetworkTxFrame, VirtioNetworkTxPacketSink,
    };
    use bangbang_runtime::startup::{Arm64BootNetworkDevice, Arm64BootNetworkPacketIoProvider};
    use bangbang_runtime::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESCRIPTOR_SIZE, read_descriptor_chain,
    };

    use super::{
        MmdsPacketDetour, MmdsResponseQueue, VmnetPacketIoBackend, VmnetPacketIoError,
        VmnetReadPacket, VmnetVirtioNetworkPacketIo, VmnetVirtioNetworkPacketIoBuildError,
        VmnetVirtioNetworkPacketIoProvider, VmnetVirtioNetworkPacketIoProviderBuildError,
        VmnetVirtioNetworkPacketIoProviderEntry, VmnetWritePacket,
    };
    use crate::host_network::vmnet::{VmnetOperation, VmnetPacketCountExpectation};

    const DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x1000);
    const HEADER_ADDRESS: GuestAddress = GuestAddress::new(0x2000);
    const PAYLOAD_ADDRESS: GuestAddress = GuestAddress::new(0x3000);
    const SECOND_PAYLOAD_ADDRESS: GuestAddress = GuestAddress::new(0x4000);
    const ETHERNET_HEADER_LEN: usize = 14;
    const IPV4_HEADER_LEN: usize = 20;
    const TCP_HEADER_LEN: usize = 20;
    const TEST_SOURCE_IPV4_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);
    const TEST_SOURCE_TCP_PORT: u16 = 49_152;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct FakeInterface;

    #[derive(Debug, Default)]
    struct FakeVmnetPacketIoBackend {
        write_error: Option<VmnetPacketIoError>,
        read_results: VecDeque<Result<Option<Vec<u8>>, VmnetPacketIoError>>,
        written_packets: Vec<Vec<u8>>,
        read_calls: usize,
        write_calls: usize,
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
        packet[tcp..tcp + 2].copy_from_slice(&TEST_SOURCE_TCP_PORT.to_be_bytes());
        packet[tcp + 2..tcp + 4].copy_from_slice(&destination_port.to_be_bytes());
        packet[tcp + 12] = 5 << 4;

        let payload_start = tcp + TCP_HEADER_LEN;
        packet[payload_start..].copy_from_slice(payload);
        packet
    }

    fn mmds_tcp_packet(payload: &[u8]) -> Vec<u8> {
        tcp_ipv4_packet(DEFAULT_MMDS_IPV4_ADDRESS, MMDS_GUEST_TCP_PORT, payload)
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

    fn mmds_response_body(response: &[u8]) -> &[u8] {
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

    fn packet_io_with_mmds_detour(
        backend: FakeVmnetPacketIoBackend,
        mmds_state: MmdsStateHandle,
        response_queue: MmdsResponseQueue,
    ) -> VmnetVirtioNetworkPacketIo<FakeVmnetPacketIoBackend> {
        let detour = MmdsPacketDetour::new(mmds_state, DEFAULT_MMDS_IPV4_ADDRESS, response_queue);
        VmnetVirtioNetworkPacketIo::with_mmds_detour(backend, fake_interface(), detour)
            .expect("packet I/O with MMDS detour should build")
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
    fn tx_sink_writes_single_segment_payload() {
        let mut memory = tx_memory();
        let frame = tx_frame(&mut memory, &[(&[0xaa, 0xbb, 0xcc], PAYLOAD_ADDRESS)]);
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
        assert_eq!(state.backend.write_calls, 1);
        assert_eq!(state.backend.written_packets, [vec![0xaa, 0xbb, 0xcc]]);
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
    fn tx_sink_detours_mmds_packet_and_retains_response() {
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

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("MMDS TX should detour");

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
            std::str::from_utf8(&responses[0])
                .expect("response should be UTF-8")
                .starts_with("HTTP/1.1 400 Bad Request")
        );
        assert_eq!(
            mmds_response_body(&responses[0]),
            b"The MMDS data store is not initialized."
        );
    }

    #[test]
    fn tx_sink_detours_only_configured_mmds_ipv4_address() {
        let configured_mmds_address = Ipv4Addr::new(169, 254, 169, 253);
        let payload = b"GET /meta-data/hostname HTTP/1.1\r\n\r\n";
        let configured_packet =
            tcp_ipv4_packet(configured_mmds_address, MMDS_GUEST_TCP_PORT, payload);
        let default_packet = mmds_tcp_packet(payload);
        let mut memory = tx_memory();
        let configured_frame = tx_frame(&mut memory, &[(&configured_packet, PAYLOAD_ADDRESS)]);
        let default_frame = tx_frame(&mut memory, &[(&default_packet, SECOND_PAYLOAD_ADDRESS)]);
        let response_queue = MmdsResponseQueue::with_capacity(2);
        let detour = MmdsPacketDetour::new(
            MmdsStateHandle::default(),
            configured_mmds_address,
            response_queue.clone(),
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

        packet_io
            .tx_sink()
            .transmit_frame(&memory, &frame)
            .expect("non-MMDS TX should forward");

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
    fn tx_sink_forwards_empty_mmds_payload_when_detour_configured() {
        let packet = mmds_tcp_packet(b"");
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
            .expect("empty-payload MMDS TX should forward");

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

        assert!(
            error
                .message()
                .contains("vmnet TX packet write failed: vmnet_write returned packet count 0")
        );
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

        assert!(
            error
                .message()
                .contains("vmnet RX packet read failed: vmnet_read returned packet count 0")
        );
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
                "vmnet RX packet read failed: vmnet_read returned packet size 2049, larger than read buffer 2048"
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
            .packet_io(&eth0_device)
            .expect("eth0 provider entry should return packet I/O");
        provider
            .packet_io(&eth1_device)
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
            .packet_io(&device)
            .expect_err("missing provider entry should fail");

        assert_eq!(
            error.message(),
            "missing vmnet packet I/O for interface eth1"
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
