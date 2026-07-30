use super::*;
use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemoryRange};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::network::{
    VIRTIO_NET_F_CSUM, VIRTIO_NET_F_GUEST_CSUM, VIRTIO_NET_F_GUEST_TSO4, VIRTIO_NET_F_GUEST_TSO6,
    VIRTIO_NET_F_GUEST_UFO, VIRTIO_NET_F_HOST_TSO4, VIRTIO_NET_F_HOST_TSO6, VIRTIO_NET_F_HOST_UFO,
    VIRTIO_NET_F_MRG_RXBUF, VirtioNetworkPacketEnvelope,
};
use crate::snapshot_device_v2::{
    SnapshotV2MmioDeviceState, SnapshotV2PciBarProbeState, SnapshotV2PciDeviceStateParts,
    SnapshotV2PciMsixState, SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry,
    SnapshotV2PciWritableByte, SnapshotV2VirtioQueueState, SnapshotV2VirtioStateParts,
};

const MAGIC: [u8; 8] = *b"BANGNW2\0";
const RECORD_MAGIC: [u8; 8] = *b"BANGNI2\0";
const MMDS_MAGIC: [u8; 8] = *b"BANGMD2\0";
const PROFILE: u16 = 1;
const FLAGS: u32 = 0;
const ALIGNMENT: usize = 8;
const DEVICE_KIND_NETWORK: u32 = 4;

const SECTION_IDENTITY: u16 = 1;
const SECTION_LOCAL: u16 = 2;
const SECTION_COMMON: u16 = 3;
const SECTION_LIMITERS: u16 = 4;
const SECTION_TRANSPORT: u16 = 5;

const TRANSPORT_MMIO: u16 = 1;
const TRANSPORT_PCI: u16 = 2;
const BACKEND_MMDS_ONLY: u8 = 1;
const BACKEND_VMNET: u8 = 2;
const RETRY_NONE: u8 = 0;
const RETRY_IMMEDIATE: u8 = 1;
const RETRY_AFTER: u8 = 2;
const ENVELOPE_RAW_ETHERNET: u8 = 1;
const ENVELOPE_DIRECT_VIRTIO_HEADER: u8 = 2;
const MMDS_V1: u8 = 1;
const MMDS_V2: u8 = 2;
const INTERRUPT_QUEUE: u8 = 1;
const INTERRUPT_CONFIGURATION: u8 = 2;

const IDENTITY_PREFIX_BYTES: usize = 32;
const COMMON_FIXED_BYTES: usize = 32;
const COMMON_QUEUE_BYTES: usize = 32;
const BUCKET_BYTES: usize = 56;
const MMIO_BYTES: usize = 48;
const PCI_MSIX_ENTRY_BYTES: usize = 16;
const PCI_PENDING_WORD_BYTES: usize = 8;
const PCI_QUEUE_VECTOR_BYTES: usize = 2;
const PCI_WRITABLE_COUNT: usize = 4;
const PCI_PROBE_COUNT: usize = 2;
const PCI_MSIX_ENTRY_COUNT: usize = 3;
const PCI_PENDING_WORD_COUNT: usize = 1;
const PCI_QUEUE_VECTOR_COUNT: usize = 2;
const PCI_PHASE_ACTIVE: u8 = 1;
const PCI_ORIGIN_STARTUP: u8 = 1;
const PCI_ORIGIN_RUNTIME: u8 = 2;
const PCI_BAR_MEMORY64: u8 = 2;
const PCI_BAR_NOT_PREFETCHABLE: u8 = 0;
const MMDS_HEADER_BYTES: usize = 32;
const MMDS_INTERFACE_BYTES: usize = 16;

const IDENTITY_REQUESTED_MAC: u16 = 1 << 0;
const IDENTITY_REQUESTED_MTU: u16 = 1 << 1;
const IDENTITY_REALIZED_MAC: u16 = 1 << 2;
const IDENTITY_REALIZED_MTU: u16 = 1 << 3;
const IDENTITY_KNOWN_FLAGS: u16 =
    IDENTITY_REQUESTED_MAC | IDENTITY_REQUESTED_MTU | IDENTITY_REALIZED_MAC | IDENTITY_REALIZED_MTU;

const HEADER_DIRECTORY_OFFSET: usize = NATIVE_V2_NETWORK_STATE_HEADER_BYTES;
const RECORD_SECTION_DIRECTORY_OFFSET: usize = NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES;
const RECORD_PAYLOAD_OFFSET: usize = RECORD_SECTION_DIRECTORY_OFFSET
    + NATIVE_V2_NETWORK_INTERFACE_SECTION_COUNT * NATIVE_V2_NETWORK_INTERFACE_SECTION_ENTRY_BYTES;

pub(super) trait ReservePolicy {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()>;
    fn reserve_string(&mut self, value: &mut String, additional: usize) -> Result<(), ()>;
}

struct FallibleReserve;

impl ReservePolicy for FallibleReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        values.try_reserve_exact(additional).map_err(|_| ())
    }

    fn reserve_string(&mut self, value: &mut String, additional: usize) -> Result<(), ()> {
        value.try_reserve_exact(additional).map_err(|_| ())
    }
}

#[derive(Clone, Copy, Default)]
struct RecordLayout {
    offset: usize,
    length: usize,
    identity_length: usize,
    common_length: usize,
    transport_length: usize,
}

struct EncodeLayout {
    records_offset: usize,
    mmds_offset: usize,
    mmds_length: usize,
    total_length: usize,
    records: [RecordLayout; NATIVE_V2_NETWORK_MAX_INTERFACES],
}

#[derive(Clone, Copy, Default)]
struct SectionBounds {
    offset: usize,
    length: usize,
}

#[derive(Clone, Copy, Default)]
struct RecordBounds {
    identity: SectionBounds,
    local: SectionBounds,
    common: SectionBounds,
    limiters: SectionBounds,
    transport: SectionBounds,
}

struct Preflight {
    transport: SnapshotV2DeviceTransportKind,
    record_count: usize,
    records: [RecordBounds; NATIVE_V2_NETWORK_MAX_INTERFACES],
    mmds: Option<SectionBounds>,
}

pub(super) fn encode(
    version: SnapshotFormatVersion,
    state: &SnapshotV2NetworkState,
) -> Result<Vec<u8>, SnapshotV2NetworkStateEncodeError> {
    encode_with_policy(version, state, &mut FallibleReserve)
}

pub(super) fn decode(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<SnapshotV2NetworkState, SnapshotV2NetworkStateDecodeError> {
    decode_with_policy(version, bytes, &mut FallibleReserve)
}

pub(super) fn encode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    state: &SnapshotV2NetworkState,
    reserve: &mut R,
) -> Result<Vec<u8>, SnapshotV2NetworkStateEncodeError> {
    if version != NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2NetworkStateEncodeError::UnsupportedVersion);
    }
    validate_network_state(state).map_err(SnapshotV2NetworkStateEncodeError::InvalidState)?;
    let layout = encode_layout(state)?;
    let mut output = Vec::new();
    reserve
        .reserve_vec(&mut output, layout.total_length)
        .map_err(|()| SnapshotV2NetworkStateEncodeError::Allocation)?;
    encode_header(&mut output, state, &layout)?;
    encode_outer_directory(&mut output, state, &layout)?;
    for (index, interface) in state.interfaces().iter().enumerate() {
        encode_record(
            &mut output,
            index,
            interface,
            layout
                .records
                .get(index)
                .copied()
                .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        )?;
    }
    if let Some(mmds) = state.mmds() {
        encode_mmds(&mut output, mmds, layout.mmds_length)?;
    }
    if output.len() != layout.total_length {
        return Err(SnapshotV2NetworkStateEncodeError::LengthOverflow);
    }
    Ok(output)
}

pub(super) fn decode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2NetworkState, SnapshotV2NetworkStateDecodeError> {
    let preflight = preflight(version, bytes)?;
    let mut interfaces = Vec::new();
    reserve
        .reserve_vec(&mut interfaces, preflight.record_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for index in 0..preflight.record_count {
        let bounds = preflight
            .records
            .get(index)
            .copied()
            .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?;
        interfaces.push(decode_record(bytes, bounds, preflight.transport, reserve)?);
    }
    let mmds = preflight
        .mmds
        .map(|bounds| decode_mmds(section(bytes, bounds)?, reserve))
        .transpose()?;
    SnapshotV2NetworkState::try_new(interfaces, mmds)
        .map_err(SnapshotV2NetworkStateDecodeError::InvalidState)
}

fn encode_layout(
    state: &SnapshotV2NetworkState,
) -> Result<EncodeLayout, SnapshotV2NetworkStateEncodeError> {
    let records_offset = HEADER_DIRECTORY_OFFSET
        .checked_add(
            state
                .interfaces()
                .len()
                .checked_mul(NATIVE_V2_NETWORK_INTERFACE_DIRECTORY_ENTRY_BYTES)
                .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
    let mut records = [RecordLayout::default(); NATIVE_V2_NETWORK_MAX_INTERFACES];
    let mut cursor = records_offset;
    for (index, interface) in state.interfaces().iter().enumerate() {
        let identity_semantic = IDENTITY_PREFIX_BYTES
            .checked_add(interface.iface_id().len())
            .and_then(|length| length.checked_add(interface.captured_selector().len()))
            .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
        let identity_length = aligned_length(identity_semantic)
            .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
        let common_length = common_length(interface.virtio())?;
        let transport_length = match interface.transport() {
            SnapshotV2DeviceTransport::Mmio(_) => MMIO_BYTES,
            SnapshotV2DeviceTransport::Pci(_) => NATIVE_V2_NETWORK_PCI_STATE_BYTES,
        };
        let length = RECORD_PAYLOAD_OFFSET
            .checked_add(identity_length)
            .and_then(|length| length.checked_add(NATIVE_V2_NETWORK_LOCAL_STATE_BYTES))
            .and_then(|length| length.checked_add(common_length))
            .and_then(|length| length.checked_add(NATIVE_V2_NETWORK_LIMITER_STATE_BYTES))
            .and_then(|length| length.checked_add(transport_length))
            .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
        if length > NATIVE_V2_NETWORK_INTERFACE_RECORD_MAX_BYTES {
            return Err(SnapshotV2NetworkStateEncodeError::TooLarge);
        }
        let slot = records
            .get_mut(index)
            .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
        *slot = RecordLayout {
            offset: cursor,
            length,
            identity_length,
            common_length,
            transport_length,
        };
        cursor = cursor
            .checked_add(length)
            .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
    }
    let (mmds_offset, mmds_length) = match state.mmds() {
        Some(mmds) => {
            let length = MMDS_HEADER_BYTES
                .checked_add(
                    mmds.interfaces()
                        .len()
                        .checked_mul(MMDS_INTERFACE_BYTES)
                        .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
                )
                .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
            (cursor, length)
        }
        None => (0, 0),
    };
    let total_length = cursor
        .checked_add(mmds_length)
        .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
    if total_length > NATIVE_V2_NETWORK_STATE_WORST_CASE_BYTES
        || total_length > NATIVE_V2_NETWORK_STATE_MAX_BYTES
    {
        return Err(SnapshotV2NetworkStateEncodeError::TooLarge);
    }
    Ok(EncodeLayout {
        records_offset,
        mmds_offset,
        mmds_length,
        total_length,
        records,
    })
}

fn common_length(
    state: &SnapshotV2VirtioState,
) -> Result<usize, SnapshotV2NetworkStateEncodeError> {
    let semantic = COMMON_FIXED_BYTES
        .checked_add(
            state
                .queues()
                .len()
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        )
        .and_then(|length| {
            length.checked_add(
                state
                    .pending_notifications()
                    .len()
                    .checked_mul(size_of::<u16>())?,
            )
        })
        .and_then(|length| {
            length.checked_add(
                state
                    .interrupt_intents()
                    .len()
                    .checked_mul(size_of::<u32>())?,
            )
        })
        .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
    let aligned =
        aligned_length(semantic).ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
    if aligned > NATIVE_V2_NETWORK_COMMON_STATE_MAX_BYTES {
        Err(SnapshotV2NetworkStateEncodeError::TooLarge)
    } else {
        Ok(aligned)
    }
}

fn encode_header(
    output: &mut Vec<u8>,
    state: &SnapshotV2NetworkState,
    layout: &EncodeLayout,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    write_bytes(output, &MAGIC);
    write_u16(
        output,
        u16::try_from(NATIVE_V2_NETWORK_STATE_HEADER_BYTES)
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u16(output, PROFILE);
    write_u16(
        output,
        transport_tag(
            state
                .interfaces()
                .first()
                .ok_or(SnapshotV2NetworkStateEncodeError::InvalidState(
                    SnapshotV2NetworkStateBuildError::InterfaceCount,
                ))?
                .transport()
                .kind(),
        ),
    );
    write_u16(
        output,
        u16::try_from(state.interfaces().len())
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u32(output, FLAGS);
    write_u32(output, 0);
    for value in [
        layout.total_length,
        HEADER_DIRECTORY_OFFSET,
        layout.records_offset,
        layout.mmds_offset,
        layout.mmds_length,
    ] {
        write_u64(
            output,
            u64::try_from(value).map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        );
    }
    Ok(())
}

fn encode_outer_directory(
    output: &mut Vec<u8>,
    state: &SnapshotV2NetworkState,
    layout: &EncodeLayout,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    for index in 0..state.interfaces().len() {
        let record = layout
            .records
            .get(index)
            .copied()
            .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
        write_u32(output, DEVICE_KIND_NETWORK);
        write_u32(
            output,
            u32::try_from(index).map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        );
        write_u64(
            output,
            u64::try_from(record.offset)
                .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        );
        write_u64(
            output,
            u64::try_from(record.length)
                .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        );
        write_u64(output, 0);
    }
    Ok(())
}

fn encode_record(
    output: &mut Vec<u8>,
    index: usize,
    interface: &SnapshotV2NetworkInterfaceState,
    layout: RecordLayout,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    if output.len() != layout.offset {
        return Err(SnapshotV2NetworkStateEncodeError::LengthOverflow);
    }
    let start = output.len();
    write_bytes(output, &RECORD_MAGIC);
    write_u16(
        output,
        u16::try_from(NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES)
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u16(output, PROFILE);
    write_u16(output, transport_tag(interface.transport().kind()));
    write_u16(
        output,
        u16::try_from(NATIVE_V2_NETWORK_INTERFACE_SECTION_COUNT)
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u32(output, FLAGS);
    write_u32(output, 0);
    write_u64(
        output,
        u64::try_from(layout.length)
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(RECORD_SECTION_DIRECTORY_OFFSET)
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(RECORD_PAYLOAD_OFFSET)
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u32(output, DEVICE_KIND_NETWORK);
    write_u32(
        output,
        u32::try_from(index).map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u64(output, 0);

    let mut cursor = RECORD_PAYLOAD_OFFSET;
    for (kind, length) in [
        (SECTION_IDENTITY, layout.identity_length),
        (SECTION_LOCAL, NATIVE_V2_NETWORK_LOCAL_STATE_BYTES),
        (SECTION_COMMON, layout.common_length),
        (SECTION_LIMITERS, NATIVE_V2_NETWORK_LIMITER_STATE_BYTES),
        (SECTION_TRANSPORT, layout.transport_length),
    ] {
        write_u16(output, kind);
        write_u16(output, 0);
        write_u32(output, 0);
        write_u64(
            output,
            u64::try_from(cursor).map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        );
        write_u64(
            output,
            u64::try_from(length).map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        );
        write_u64(output, 0);
        cursor = cursor
            .checked_add(length)
            .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
    }
    encode_identity(output, interface, layout.identity_length)?;
    encode_local(output, interface.local(), interface.backend())?;
    encode_common(output, interface.virtio(), layout.common_length)?;
    encode_limiters(output, interface.rx_limiter(), interface.tx_limiter());
    encode_transport(output, interface.transport(), layout.transport_length)?;
    if output.len() != start + layout.length {
        return Err(SnapshotV2NetworkStateEncodeError::LengthOverflow);
    }
    Ok(())
}

fn encode_identity(
    output: &mut Vec<u8>,
    interface: &SnapshotV2NetworkInterfaceState,
    section_length: usize,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    let start = output.len();
    write_u16(
        output,
        u16::try_from(interface.iface_id().len())
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(interface.captured_selector().len())
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    let mut flags = 0;
    if interface.requested_guest_mac().is_some() {
        flags |= IDENTITY_REQUESTED_MAC;
    }
    if interface.requested_mtu().is_some() {
        flags |= IDENTITY_REQUESTED_MTU;
    }
    if interface.profile().guest_mac().is_some() {
        flags |= IDENTITY_REALIZED_MAC;
    }
    if interface.profile().mtu().is_some() {
        flags |= IDENTITY_REALIZED_MTU;
    }
    write_u16(output, flags);
    write_u8(
        output,
        match interface.profile().packet_envelope() {
            VirtioNetworkPacketEnvelope::RawEthernet => ENVELOPE_RAW_ETHERNET,
            VirtioNetworkPacketEnvelope::DirectVirtioHeader => ENVELOPE_DIRECT_VIRTIO_HEADER,
        },
    );
    write_u8(output, 0);
    write_u64(
        output,
        interface.profile().feature_capabilities().feature_bits(),
    );
    write_bytes(
        output,
        &interface
            .requested_guest_mac()
            .map(GuestMacAddress::octets)
            .unwrap_or([0; 6]),
    );
    write_bytes(
        output,
        &interface
            .profile()
            .guest_mac()
            .map(GuestMacAddress::octets)
            .unwrap_or([0; 6]),
    );
    write_u16(output, interface.requested_mtu().unwrap_or(0));
    write_u16(output, interface.profile().mtu().unwrap_or(0));
    write_bytes(output, interface.iface_id().as_bytes());
    write_bytes(output, interface.captured_selector().as_bytes());
    pad_to(output, start, section_length)
}

fn encode_local(
    output: &mut Vec<u8>,
    local: &SnapshotV2NetworkLocalState,
    backend: SnapshotV2NetworkBackendClass,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    let start = output.len();
    write_u8(
        output,
        match backend {
            SnapshotV2NetworkBackendClass::MmdsOnly => BACKEND_MMDS_ONLY,
            SnapshotV2NetworkBackendClass::Vmnet => BACKEND_VMNET,
        },
    );
    write_bool(output, local.active_rx_queue().is_some());
    write_bool(output, local.active_tx_queue().is_some());
    let (retry_tag, retry_nanos) = match local.tx_retry() {
        SnapshotV2NetworkRetryState::None => (RETRY_NONE, 0),
        SnapshotV2NetworkRetryState::Immediate => (RETRY_IMMEDIATE, 0),
        SnapshotV2NetworkRetryState::After { remaining_nanos } => (RETRY_AFTER, remaining_nanos),
    };
    write_u8(output, retry_tag);
    let rx = local
        .active_rx_queue()
        .unwrap_or_else(|| SnapshotV2NetworkQueueState::new(0, 0));
    let tx = local
        .active_tx_queue()
        .unwrap_or_else(|| SnapshotV2NetworkQueueState::new(0, 0));
    write_u16(output, rx.next_available());
    write_u16(output, rx.next_used());
    write_u16(output, tx.next_available());
    write_u16(output, tx.next_used());
    write_u64(output, retry_nanos);
    write_zeroes(
        output,
        NATIVE_V2_NETWORK_LOCAL_STATE_BYTES
            .checked_sub(output.len() - start)
            .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    Ok(())
}

fn encode_common(
    output: &mut Vec<u8>,
    state: &SnapshotV2VirtioState,
    section_length: usize,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    let start = output.len();
    write_u64(output, state.available_features());
    write_u64(output, state.driver_features());
    write_u32(output, state.config_generation());
    write_u32(output, state.status());
    write_bool(output, state.is_activated());
    write_u8(output, 0);
    write_u16(
        output,
        u16::try_from(state.queues().len())
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.pending_notifications().len())
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.interrupt_intents().len())
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    for queue in state.queues() {
        write_u16(output, queue.max_size());
        write_u16(output, queue.size());
        write_bool(output, queue.ready());
        write_zeroes(output, 3);
        write_u64(output, queue.descriptor_table().raw_value());
        write_u64(output, queue.driver_ring().raw_value());
        write_u64(output, queue.device_ring().raw_value());
    }
    for notification in state.pending_notifications() {
        write_u16(output, *notification);
    }
    for intent in state.interrupt_intents() {
        match intent {
            SnapshotV2InterruptIntent::Queue { queue_index } => {
                write_u8(output, INTERRUPT_QUEUE);
                write_u8(output, 0);
                write_u16(output, *queue_index);
            }
            SnapshotV2InterruptIntent::Configuration => {
                write_u8(output, INTERRUPT_CONFIGURATION);
                write_u8(output, 0);
                write_u16(output, 0);
            }
        }
    }
    pad_to(output, start, section_length)
}

fn encode_limiters(
    output: &mut Vec<u8>,
    rx: SnapshotV2NetworkLimiterState,
    tx: SnapshotV2NetworkLimiterState,
) {
    for bucket in [rx.bandwidth(), rx.ops(), tx.bandwidth(), tx.ops()] {
        encode_bucket(output, bucket);
    }
}

fn encode_bucket(output: &mut Vec<u8>, bucket: Option<SnapshotV2NetworkTokenBucketState>) {
    match bucket {
        Some(bucket) => {
            write_bool(output, true);
            write_bool(output, bucket.configured_burst().is_some());
            write_zeroes(output, 6);
            write_u64(output, bucket.size());
            write_u64(output, bucket.configured_burst().unwrap_or(0));
            write_u64(output, bucket.refill_time_millis());
            write_u64(output, bucket.budget());
            write_u64(output, bucket.remaining_burst());
            write_u64(output, bucket.age_nanos());
        }
        None => write_zeroes(output, BUCKET_BYTES),
    }
}

fn encode_transport(
    output: &mut Vec<u8>,
    transport: &SnapshotV2DeviceTransport,
    section_length: usize,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    match transport {
        SnapshotV2DeviceTransport::Mmio(state) => encode_mmio(output, state, section_length),
        SnapshotV2DeviceTransport::Pci(state) => encode_pci(output, state, section_length),
    }
}

fn encode_mmio(
    output: &mut Vec<u8>,
    state: &SnapshotV2MmioDeviceState,
    section_length: usize,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    let start = output.len();
    write_u32(output, state.device_feature_select());
    write_u32(output, state.driver_feature_select());
    write_u32(output, state.queue_select());
    write_u32(output, state.interrupt_line().raw_value());
    write_u64(output, state.region().id().raw_value());
    write_u64(output, state.region().range().start().raw_value());
    write_u64(output, state.region().range().size());
    write_u64(output, 0);
    pad_to(output, start, section_length)
}

fn encode_pci(
    output: &mut Vec<u8>,
    state: &SnapshotV2PciDeviceState,
    section_length: usize,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    let start = output.len();
    write_u8(output, PCI_PHASE_ACTIVE);
    write_u8(
        output,
        match state.origin() {
            StorageDeviceOrigin::Startup => PCI_ORIGIN_STARTUP,
            StorageDeviceOrigin::Runtime => PCI_ORIGIN_RUNTIME,
        },
    );
    write_u8(output, state.bar_index());
    write_u8(output, PCI_BAR_MEMORY64);
    write_u8(output, PCI_BAR_NOT_PREFETCHABLE);
    write_u8(output, state.pci_cfg_bar());
    write_u8(output, state.sbdf().function());
    write_u8(output, 0);
    write_u16(output, state.sbdf().segment());
    write_u8(output, state.sbdf().bus());
    write_u8(output, state.sbdf().device());
    write_zeroes(output, 4);
    write_u64(output, state.bar_range().start().raw_value());
    write_u64(output, state.bar_range().size());
    write_u32(output, state.device_feature_select());
    write_u32(output, state.driver_feature_select());
    write_u16(output, state.queue_select());
    for count in [
        state.writable_bytes().len(),
        state.bar_probes().len(),
        state.msix().entries().len(),
        state.msix().pending_words().len(),
        state.msix().queue_vectors().len(),
    ] {
        write_u16(
            output,
            u16::try_from(count).map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
        );
    }
    write_zeroes(output, 4);
    write_u32(output, state.pci_cfg_offset());
    write_u32(output, state.pci_cfg_length());
    write_bool(output, state.msix().enabled());
    write_bool(output, state.msix().function_masked());
    write_bool(output, state.msix().pending_transition_observed());
    write_u8(output, 0);
    write_u16(output, state.msix().config_vector());
    write_u16(output, 0);
    for writable in state.writable_bytes() {
        write_u16(output, writable.offset());
        write_u8(output, writable.value());
        write_u8(output, 0);
    }
    for probe in state.bar_probes() {
        write_u8(output, probe.index());
        write_bool(output, probe.pending());
        write_zeroes(output, 2);
    }
    for entry in state.msix().entries() {
        write_u32(output, entry.message_address_low());
        write_u32(output, entry.message_address_high());
        write_u32(output, entry.message_data());
        write_u32(output, entry.vector_control());
    }
    for pending in state.msix().pending_words() {
        write_u64(output, *pending);
    }
    for vector in state.msix().queue_vectors() {
        write_u16(output, *vector);
    }
    pad_to(output, start, section_length)
}

fn encode_mmds(
    output: &mut Vec<u8>,
    mmds: &SnapshotV2MmdsState,
    section_length: usize,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    let start = output.len();
    write_bytes(output, &MMDS_MAGIC);
    write_u8(
        output,
        match mmds.version() {
            MmdsVersion::V1 => MMDS_V1,
            MmdsVersion::V2 => MMDS_V2,
        },
    );
    write_bool(output, mmds.imds_compat());
    write_bool(output, mmds.ipv4_address().is_some());
    write_u8(
        output,
        u8::try_from(mmds.interfaces().len())
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_bytes(
        output,
        &mmds
            .ipv4_address()
            .map(|address| address.octets())
            .unwrap_or([0; 4]),
    );
    write_u64(
        output,
        u64::try_from(section_length)
            .map_err(|_| SnapshotV2NetworkStateEncodeError::LengthOverflow)?,
    );
    write_u64(output, 0);
    for interface in mmds.interfaces() {
        write_u16(output, interface.interface_index());
        write_bytes(output, &interface.local_mac_address().octets());
        write_bytes(output, &interface.ipv4_address().octets());
        write_u16(output, interface.tcp_port());
        write_u16(output, 0);
    }
    if output.len() != start + section_length {
        return Err(SnapshotV2NetworkStateEncodeError::LengthOverflow);
    }
    Ok(())
}

fn preflight(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<Preflight, SnapshotV2NetworkStateDecodeError> {
    if version != NATIVE_V2_NETWORK_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2NetworkStateDecodeError::UnsupportedVersion);
    }
    if bytes.len() > NATIVE_V2_NETWORK_STATE_MAX_BYTES
        || bytes.len() > NATIVE_V2_NETWORK_STATE_WORST_CASE_BYTES
    {
        return Err(SnapshotV2NetworkStateDecodeError::TooLarge);
    }
    let mut reader = Reader::new(bytes);
    if reader.read_array::<8>()? != MAGIC
        || usize::from(reader.read_u16()?) != NATIVE_V2_NETWORK_STATE_HEADER_BYTES
        || reader.read_u16()? != PROFILE
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidHeader);
    }
    let transport = decode_transport_kind(reader.read_u16()?)?;
    let record_count = usize::from(reader.read_u16()?);
    if !(1..=NATIVE_V2_NETWORK_MAX_INTERFACES).contains(&record_count)
        || reader.read_u32()? != FLAGS
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidHeader);
    }
    reader.read_zeroes(4)?;
    let total_length = read_usize(&mut reader)?;
    let directory_offset = read_usize(&mut reader)?;
    let records_offset = read_usize(&mut reader)?;
    let mmds_offset = read_usize(&mut reader)?;
    let mmds_length = read_usize(&mut reader)?;
    let expected_records_offset = HEADER_DIRECTORY_OFFSET
        .checked_add(
            record_count
                .checked_mul(NATIVE_V2_NETWORK_INTERFACE_DIRECTORY_ENTRY_BYTES)
                .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?,
        )
        .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?;
    if total_length != bytes.len()
        || directory_offset != HEADER_DIRECTORY_OFFSET
        || records_offset != expected_records_offset
        || reader.position() != NATIVE_V2_NETWORK_STATE_HEADER_BYTES
        || (mmds_offset == 0) != (mmds_length == 0)
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }

    let mut records = [RecordBounds::default(); NATIVE_V2_NETWORK_MAX_INTERFACES];
    let mut cursor = records_offset;
    for index in 0..record_count {
        if reader.read_u32()? != DEVICE_KIND_NETWORK
            || usize::try_from(reader.read_u32()?).ok() != Some(index)
        {
            return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
        }
        let offset = read_usize(&mut reader)?;
        let length = read_usize(&mut reader)?;
        reader.read_zeroes(8)?;
        if offset != cursor
            || !(RECORD_PAYLOAD_OFFSET..=NATIVE_V2_NETWORK_INTERFACE_RECORD_MAX_BYTES)
                .contains(&length)
        {
            return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
        }
        let end = offset
            .checked_add(length)
            .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?;
        let record_bytes = bytes
            .get(offset..end)
            .ok_or(SnapshotV2NetworkStateDecodeError::Truncated)?;
        let bounds = preflight_record(record_bytes, offset, index, transport)?;
        *records
            .get_mut(index)
            .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)? = bounds;
        cursor = end;
    }
    if reader.position() != records_offset {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let mmds = if mmds_length == 0 {
        if cursor != bytes.len() {
            return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
        }
        None
    } else {
        if mmds_offset != cursor
            || mmds_length > NATIVE_V2_NETWORK_MMDS_STATE_MAX_BYTES
            || mmds_offset
                .checked_add(mmds_length)
                .is_none_or(|end| end != bytes.len())
        {
            return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
        }
        let bounds = SectionBounds {
            offset: mmds_offset,
            length: mmds_length,
        };
        preflight_mmds(section(bytes, bounds)?, record_count)?;
        Some(bounds)
    };
    Ok(Preflight {
        transport,
        record_count,
        records,
        mmds,
    })
}

fn preflight_record(
    bytes: &[u8],
    absolute_offset: usize,
    index: usize,
    outer_transport: SnapshotV2DeviceTransportKind,
) -> Result<RecordBounds, SnapshotV2NetworkStateDecodeError> {
    let mut reader = Reader::new(bytes);
    if reader.read_array::<8>()? != RECORD_MAGIC
        || usize::from(reader.read_u16()?) != NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES
        || reader.read_u16()? != PROFILE
        || decode_transport_kind(reader.read_u16()?)? != outer_transport
        || usize::from(reader.read_u16()?) != NATIVE_V2_NETWORK_INTERFACE_SECTION_COUNT
        || reader.read_u32()? != FLAGS
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidHeader);
    }
    reader.read_zeroes(4)?;
    let total_length = read_usize(&mut reader)?;
    let directory_offset = read_usize(&mut reader)?;
    let payload_offset = read_usize(&mut reader)?;
    if reader.read_u32()? != DEVICE_KIND_NETWORK
        || usize::try_from(reader.read_u32()?).ok() != Some(index)
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    reader.read_zeroes(8)?;
    if total_length != bytes.len()
        || directory_offset != RECORD_SECTION_DIRECTORY_OFFSET
        || payload_offset != RECORD_PAYLOAD_OFFSET
        || reader.position() != NATIVE_V2_NETWORK_INTERFACE_RECORD_HEADER_BYTES
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }

    let mut section_bounds = [SectionBounds::default(); NATIVE_V2_NETWORK_INTERFACE_SECTION_COUNT];
    let mut cursor = RECORD_PAYLOAD_OFFSET;
    for (section_index, expected_kind) in [
        SECTION_IDENTITY,
        SECTION_LOCAL,
        SECTION_COMMON,
        SECTION_LIMITERS,
        SECTION_TRANSPORT,
    ]
    .into_iter()
    .enumerate()
    {
        if reader.read_u16()? != expected_kind || reader.read_u16()? != 0 {
            return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
        }
        reader.read_zeroes(4)?;
        let offset = read_usize(&mut reader)?;
        let length = read_usize(&mut reader)?;
        reader.read_zeroes(8)?;
        if offset != cursor
            || !offset.is_multiple_of(ALIGNMENT)
            || !length.is_multiple_of(ALIGNMENT)
        {
            return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
        }
        cursor = cursor
            .checked_add(length)
            .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?;
        *section_bounds
            .get_mut(section_index)
            .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)? =
            SectionBounds { offset, length };
    }
    if reader.position() != RECORD_PAYLOAD_OFFSET || cursor != bytes.len() {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let identity = section_bounds[0];
    let local = section_bounds[1];
    let common = section_bounds[2];
    let limiters = section_bounds[3];
    let transport = section_bounds[4];
    if local.length != NATIVE_V2_NETWORK_LOCAL_STATE_BYTES
        || limiters.length != NATIVE_V2_NETWORK_LIMITER_STATE_BYTES
        || transport.length
            != match outer_transport {
                SnapshotV2DeviceTransportKind::Mmio => MMIO_BYTES,
                SnapshotV2DeviceTransportKind::Pci => NATIVE_V2_NETWORK_PCI_STATE_BYTES,
            }
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    preflight_identity(section(bytes, identity)?)?;
    preflight_local(section(bytes, local)?)?;
    preflight_common(section(bytes, common)?)?;
    preflight_limiters(section(bytes, limiters)?)?;
    preflight_transport(section(bytes, transport)?, outer_transport)?;
    Ok(RecordBounds {
        identity: absolute_bounds(absolute_offset, identity)?,
        local: absolute_bounds(absolute_offset, local)?,
        common: absolute_bounds(absolute_offset, common)?,
        limiters: absolute_bounds(absolute_offset, limiters)?,
        transport: absolute_bounds(absolute_offset, transport)?,
    })
}

fn preflight_identity(bytes: &[u8]) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if bytes.len() < IDENTITY_PREFIX_BYTES
        || bytes.len()
            > align_up_const(
                IDENTITY_PREFIX_BYTES
                    + NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES
                    + NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES,
                ALIGNMENT,
            )
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let mut reader = Reader::new(bytes);
    let id_length = usize::from(reader.read_u16()?);
    let selector_length = usize::from(reader.read_u16()?);
    let flags = reader.read_u16()?;
    if flags & !IDENTITY_KNOWN_FLAGS != 0 {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    match reader.read_u8()? {
        ENVELOPE_RAW_ETHERNET | ENVELOPE_DIRECT_VIRTIO_HEADER => {}
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    }
    reader.read_zeroes(1)?;
    let capability_bits = reader.read_u64()?;
    if decode_feature_capabilities(capability_bits).is_err() {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    let requested_mac = reader.read_array::<6>()?;
    let realized_mac = reader.read_array::<6>()?;
    let requested_mtu = reader.read_u16()?;
    let realized_mtu = reader.read_u16()?;
    validate_option_payload(flags, IDENTITY_REQUESTED_MAC, &requested_mac)?;
    validate_option_payload(flags, IDENTITY_REALIZED_MAC, &realized_mac)?;
    validate_option_scalar(flags, IDENTITY_REQUESTED_MTU, requested_mtu)?;
    validate_option_scalar(flags, IDENTITY_REALIZED_MTU, realized_mtu)?;
    if id_length == 0
        || id_length > NATIVE_V2_NETWORK_MAX_INTERFACE_ID_BYTES
        || selector_length == 0
        || selector_length > NATIVE_V2_NETWORK_MAX_CAPTURED_SELECTOR_BYTES
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    let semantic_length = IDENTITY_PREFIX_BYTES
        .checked_add(id_length)
        .and_then(|length| length.checked_add(selector_length))
        .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?;
    if aligned_length(semantic_length) != Some(bytes.len()) {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let id = std::str::from_utf8(reader.read_bytes(id_length)?)
        .map_err(|_| SnapshotV2NetworkStateDecodeError::InvalidUtf8)?;
    let selector = std::str::from_utf8(reader.read_bytes(selector_length)?)
        .map_err(|_| SnapshotV2NetworkStateDecodeError::InvalidUtf8)?;
    if !id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
        || selector.chars().any(char::is_control)
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    reader.finish_padded()
}

fn preflight_local(bytes: &[u8]) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if bytes.len() != NATIVE_V2_NETWORK_LOCAL_STATE_BYTES {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let mut reader = Reader::new(bytes);
    match reader.read_u8()? {
        BACKEND_MMDS_ONLY | BACKEND_VMNET => {}
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    }
    let rx_present = reader.read_bool()?;
    let tx_present = reader.read_bool()?;
    let retry = reader.read_u8()?;
    let rx_available = reader.read_u16()?;
    let rx_used = reader.read_u16()?;
    let tx_available = reader.read_u16()?;
    let tx_used = reader.read_u16()?;
    let retry_nanos = reader.read_u64()?;
    if (!rx_present && (rx_available != 0 || rx_used != 0))
        || (!tx_present && (tx_available != 0 || tx_used != 0))
        || !matches!(
            (retry, retry_nanos),
            (RETRY_NONE | RETRY_IMMEDIATE, 0) | (RETRY_AFTER, 1..)
        )
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    reader.finish_padded()
}

fn preflight_common(bytes: &[u8]) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if bytes.len() < COMMON_FIXED_BYTES + VIRTIO_NET_QUEUE_COUNT * COMMON_QUEUE_BYTES
        || bytes.len() > NATIVE_V2_NETWORK_COMMON_STATE_MAX_BYTES
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let mut reader = Reader::new(bytes);
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_u32()?;
    reader.read_u32()?;
    reader.read_bool()?;
    reader.read_zeroes(1)?;
    let queue_count = usize::from(reader.read_u16()?);
    let notification_count = usize::from(reader.read_u16()?);
    let intent_count = usize::from(reader.read_u16()?);
    if queue_count != VIRTIO_NET_QUEUE_COUNT
        || notification_count > VIRTIO_NET_QUEUE_COUNT
        || intent_count > VIRTIO_NET_QUEUE_COUNT + 1
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    let semantic_length = COMMON_FIXED_BYTES
        .checked_add(
            queue_count
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?,
        )
        .and_then(|length| length.checked_add(notification_count.checked_mul(2)?))
        .and_then(|length| length.checked_add(intent_count.checked_mul(4)?))
        .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?;
    if aligned_length(semantic_length) != Some(bytes.len()) {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    for _ in 0..queue_count {
        reader.read_u16()?;
        reader.read_u16()?;
        reader.read_bool()?;
        reader.read_zeroes(3)?;
        reader.read_u64()?;
        reader.read_u64()?;
        reader.read_u64()?;
    }
    for _ in 0..notification_count {
        reader.read_u16()?;
    }
    for _ in 0..intent_count {
        match reader.read_u8()? {
            INTERRUPT_QUEUE | INTERRUPT_CONFIGURATION => {}
            _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
        }
        reader.read_zeroes(1)?;
        reader.read_u16()?;
    }
    reader.finish_padded()
}

fn preflight_limiters(bytes: &[u8]) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if bytes.len() != NATIVE_V2_NETWORK_LIMITER_STATE_BYTES {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let mut reader = Reader::new(bytes);
    for _ in 0..4 {
        let present = reader.read_bool()?;
        let burst_present = reader.read_bool()?;
        reader.read_zeroes(6)?;
        let values = [
            reader.read_u64()?,
            reader.read_u64()?,
            reader.read_u64()?,
            reader.read_u64()?,
            reader.read_u64()?,
            reader.read_u64()?,
        ];
        if !present && (burst_present || values.iter().any(|value| *value != 0))
            || present && !burst_present && values[1] != 0
        {
            return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
        }
    }
    reader.finish_exact()
}

fn preflight_transport(
    bytes: &[u8],
    transport: SnapshotV2DeviceTransportKind,
) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    match transport {
        SnapshotV2DeviceTransportKind::Mmio => preflight_mmio(bytes),
        SnapshotV2DeviceTransportKind::Pci => preflight_pci(bytes),
    }
}

fn preflight_mmio(bytes: &[u8]) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if bytes.len() != MMIO_BYTES {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let mut reader = Reader::new(bytes);
    reader.read_u32()?;
    reader.read_u32()?;
    reader.read_u32()?;
    reader.read_u32()?;
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_zeroes(8)?;
    reader.finish_exact()
}

fn preflight_pci(bytes: &[u8]) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if bytes.len() != NATIVE_V2_NETWORK_PCI_STATE_BYTES {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let mut reader = Reader::new(bytes);
    if reader.read_u8()? != PCI_PHASE_ACTIVE
        || !matches!(reader.read_u8()?, PCI_ORIGIN_STARTUP | PCI_ORIGIN_RUNTIME)
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    reader.read_u8()?;
    if reader.read_u8()? != PCI_BAR_MEMORY64 || reader.read_u8()? != PCI_BAR_NOT_PREFETCHABLE {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    reader.read_u8()?;
    reader.read_u8()?;
    reader.read_zeroes(1)?;
    reader.read_u16()?;
    reader.read_u8()?;
    reader.read_u8()?;
    reader.read_zeroes(4)?;
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_u32()?;
    reader.read_u32()?;
    reader.read_u16()?;
    let counts = [
        usize::from(reader.read_u16()?),
        usize::from(reader.read_u16()?),
        usize::from(reader.read_u16()?),
        usize::from(reader.read_u16()?),
        usize::from(reader.read_u16()?),
    ];
    if counts
        != [
            PCI_WRITABLE_COUNT,
            PCI_PROBE_COUNT,
            PCI_MSIX_ENTRY_COUNT,
            PCI_PENDING_WORD_COUNT,
            PCI_QUEUE_VECTOR_COUNT,
        ]
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    reader.read_zeroes(4)?;
    reader.read_u32()?;
    reader.read_u32()?;
    reader.read_bool()?;
    reader.read_bool()?;
    reader.read_bool()?;
    reader.read_zeroes(1)?;
    reader.read_u16()?;
    reader.read_zeroes(2)?;
    for _ in 0..PCI_WRITABLE_COUNT {
        reader.read_u16()?;
        reader.read_u8()?;
        reader.read_zeroes(1)?;
    }
    for _ in 0..PCI_PROBE_COUNT {
        reader.read_u8()?;
        reader.read_bool()?;
        reader.read_zeroes(2)?;
    }
    reader.read_bytes(PCI_MSIX_ENTRY_COUNT * PCI_MSIX_ENTRY_BYTES)?;
    reader.read_bytes(PCI_PENDING_WORD_COUNT * PCI_PENDING_WORD_BYTES)?;
    reader.read_bytes(PCI_QUEUE_VECTOR_COUNT * PCI_QUEUE_VECTOR_BYTES)?;
    reader.finish_padded()
}

fn preflight_mmds(
    bytes: &[u8],
    record_count: usize,
) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if bytes.len() < MMDS_HEADER_BYTES || bytes.len() > NATIVE_V2_NETWORK_MMDS_STATE_MAX_BYTES {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidLayout);
    }
    let mut reader = Reader::new(bytes);
    if reader.read_array::<8>()? != MMDS_MAGIC || !matches!(reader.read_u8()?, MMDS_V1 | MMDS_V2) {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    reader.read_bool()?;
    let address_present = reader.read_bool()?;
    let interface_count = usize::from(reader.read_u8()?);
    let address = reader.read_array::<4>()?;
    if !address_present && address != [0; 4]
        || interface_count == 0
        || interface_count > record_count
        || bytes.len()
            != MMDS_HEADER_BYTES
                + interface_count
                    .checked_mul(MMDS_INTERFACE_BYTES)
                    .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?
        || read_usize(&mut reader)? != bytes.len()
    {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    reader.read_zeroes(8)?;
    let mut previous = None;
    for _ in 0..interface_count {
        let index = reader.read_u16()?;
        if previous.is_some_and(|previous| previous >= index) || usize::from(index) >= record_count
        {
            return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
        }
        previous = Some(index);
        reader.read_bytes(6)?;
        reader.read_bytes(4)?;
        reader.read_u16()?;
        reader.read_zeroes(2)?;
    }
    reader.finish_exact()
}

fn decode_record<R: ReservePolicy>(
    bytes: &[u8],
    bounds: RecordBounds,
    transport_kind: SnapshotV2DeviceTransportKind,
    reserve: &mut R,
) -> Result<SnapshotV2NetworkInterfaceState, SnapshotV2NetworkStateDecodeError> {
    let (iface_id, captured_selector, requested_guest_mac, requested_mtu, profile) =
        decode_identity(section(bytes, bounds.identity)?, reserve)?;
    let (backend, local) = decode_local(section(bytes, bounds.local)?)?;
    let virtio = decode_common(section(bytes, bounds.common)?, reserve)?;
    let (rx_limiter, tx_limiter) = decode_limiters(section(bytes, bounds.limiters)?)?;
    let transport = decode_transport(section(bytes, bounds.transport)?, transport_kind, reserve)?;
    Ok(SnapshotV2NetworkInterfaceState::from_parts_unchecked(
        SnapshotV2NetworkInterfaceStateParts {
            iface_id,
            captured_selector,
            requested_guest_mac,
            requested_mtu,
            profile,
            backend,
            local,
            virtio,
            rx_limiter,
            tx_limiter,
            transport,
        },
    ))
}

type DecodedIdentity = (
    String,
    String,
    Option<GuestMacAddress>,
    Option<u16>,
    NetworkDeviceProfile,
);

fn decode_identity<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<DecodedIdentity, SnapshotV2NetworkStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let id_length = usize::from(reader.read_u16()?);
    let selector_length = usize::from(reader.read_u16()?);
    let flags = reader.read_u16()?;
    let envelope = match reader.read_u8()? {
        ENVELOPE_RAW_ETHERNET => VirtioNetworkPacketEnvelope::RawEthernet,
        ENVELOPE_DIRECT_VIRTIO_HEADER => VirtioNetworkPacketEnvelope::DirectVirtioHeader,
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    };
    reader.read_zeroes(1)?;
    let capabilities = decode_feature_capabilities(reader.read_u64()?)?;
    let requested_mac_bytes = reader.read_array::<6>()?;
    let realized_mac_bytes = reader.read_array::<6>()?;
    let requested_mtu_value = reader.read_u16()?;
    let realized_mtu_value = reader.read_u16()?;
    let requested_guest_mac = (flags & IDENTITY_REQUESTED_MAC != 0)
        .then(|| GuestMacAddress::from_bytes(requested_mac_bytes));
    let realized_guest_mac = (flags & IDENTITY_REALIZED_MAC != 0)
        .then(|| GuestMacAddress::from_bytes(realized_mac_bytes));
    let requested_mtu = (flags & IDENTITY_REQUESTED_MTU != 0).then_some(requested_mtu_value);
    let realized_mtu = (flags & IDENTITY_REALIZED_MTU != 0).then_some(realized_mtu_value);
    let id = std::str::from_utf8(reader.read_bytes(id_length)?)
        .map_err(|_| SnapshotV2NetworkStateDecodeError::InvalidUtf8)?;
    let selector = std::str::from_utf8(reader.read_bytes(selector_length)?)
        .map_err(|_| SnapshotV2NetworkStateDecodeError::InvalidUtf8)?;
    let mut iface_id = String::new();
    reserve
        .reserve_string(&mut iface_id, id_length)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    iface_id.push_str(id);
    let mut captured_selector = String::new();
    reserve
        .reserve_string(&mut captured_selector, selector_length)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    captured_selector.push_str(selector);
    reader.finish_padded()?;
    let profile = NetworkDeviceProfile::new(realized_guest_mac, realized_mtu)
        .with_packet_envelope(envelope)
        .with_feature_capabilities(capabilities);
    Ok((
        iface_id,
        captured_selector,
        requested_guest_mac,
        requested_mtu,
        profile,
    ))
}

fn decode_local(
    bytes: &[u8],
) -> Result<
    (SnapshotV2NetworkBackendClass, SnapshotV2NetworkLocalState),
    SnapshotV2NetworkStateDecodeError,
> {
    let mut reader = Reader::new(bytes);
    let backend = match reader.read_u8()? {
        BACKEND_MMDS_ONLY => SnapshotV2NetworkBackendClass::MmdsOnly,
        BACKEND_VMNET => SnapshotV2NetworkBackendClass::Vmnet,
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    };
    let rx_present = reader.read_bool()?;
    let tx_present = reader.read_bool()?;
    let retry_tag = reader.read_u8()?;
    let rx = SnapshotV2NetworkQueueState::new(reader.read_u16()?, reader.read_u16()?);
    let tx = SnapshotV2NetworkQueueState::new(reader.read_u16()?, reader.read_u16()?);
    let retry_nanos = reader.read_u64()?;
    reader.finish_padded()?;
    let retry = match (retry_tag, retry_nanos) {
        (RETRY_NONE, 0) => SnapshotV2NetworkRetryState::None,
        (RETRY_IMMEDIATE, 0) => SnapshotV2NetworkRetryState::Immediate,
        (RETRY_AFTER, 1..) => SnapshotV2NetworkRetryState::After {
            remaining_nanos: retry_nanos,
        },
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    };
    Ok((
        backend,
        SnapshotV2NetworkLocalState::new(rx_present.then_some(rx), tx_present.then_some(tx), retry),
    ))
}

fn decode_common<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2VirtioState, SnapshotV2NetworkStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let available_features = reader.read_u64()?;
    let driver_features = reader.read_u64()?;
    let config_generation = reader.read_u32()?;
    let status = reader.read_u32()?;
    let activated = reader.read_bool()?;
    reader.read_zeroes(1)?;
    let queue_count = usize::from(reader.read_u16()?);
    let notification_count = usize::from(reader.read_u16()?);
    let intent_count = usize::from(reader.read_u16()?);
    let mut queues = Vec::new();
    reserve
        .reserve_vec(&mut queues, queue_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for _ in 0..queue_count {
        let max_size = reader.read_u16()?;
        let size = reader.read_u16()?;
        let ready = reader.read_bool()?;
        reader.read_zeroes(3)?;
        queues.push(SnapshotV2VirtioQueueState::from_parts(
            max_size,
            size,
            ready,
            GuestAddress::new(reader.read_u64()?),
            GuestAddress::new(reader.read_u64()?),
            GuestAddress::new(reader.read_u64()?),
        ));
    }
    let mut pending_notifications = Vec::new();
    reserve
        .reserve_vec(&mut pending_notifications, notification_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for _ in 0..notification_count {
        pending_notifications.push(reader.read_u16()?);
    }
    let mut interrupt_intents = Vec::new();
    reserve
        .reserve_vec(&mut interrupt_intents, intent_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for _ in 0..intent_count {
        let tag = reader.read_u8()?;
        reader.read_zeroes(1)?;
        let queue_index = reader.read_u16()?;
        interrupt_intents.push(match (tag, queue_index) {
            (INTERRUPT_QUEUE, queue_index) => SnapshotV2InterruptIntent::Queue { queue_index },
            (INTERRUPT_CONFIGURATION, 0) => SnapshotV2InterruptIntent::Configuration,
            _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
        });
    }
    reader.finish_padded()?;
    Ok(SnapshotV2VirtioState::from_parts(
        SnapshotV2VirtioStateParts {
            available_features,
            driver_features,
            config_generation,
            status,
            activated,
            queues,
            pending_notifications,
            interrupt_intents,
        },
    ))
}

fn decode_limiters(
    bytes: &[u8],
) -> Result<
    (SnapshotV2NetworkLimiterState, SnapshotV2NetworkLimiterState),
    SnapshotV2NetworkStateDecodeError,
> {
    let mut reader = Reader::new(bytes);
    let rx_bandwidth = decode_bucket(&mut reader)?;
    let rx_ops = decode_bucket(&mut reader)?;
    let tx_bandwidth = decode_bucket(&mut reader)?;
    let tx_ops = decode_bucket(&mut reader)?;
    reader.finish_exact()?;
    Ok((
        SnapshotV2NetworkLimiterState::new(rx_bandwidth, rx_ops),
        SnapshotV2NetworkLimiterState::new(tx_bandwidth, tx_ops),
    ))
}

fn decode_bucket(
    reader: &mut Reader<'_>,
) -> Result<Option<SnapshotV2NetworkTokenBucketState>, SnapshotV2NetworkStateDecodeError> {
    let present = reader.read_bool()?;
    let burst_present = reader.read_bool()?;
    reader.read_zeroes(6)?;
    let size = reader.read_u64()?;
    let burst = reader.read_u64()?;
    let refill = reader.read_u64()?;
    let budget = reader.read_u64()?;
    let remaining = reader.read_u64()?;
    let age = reader.read_u64()?;
    if present {
        Ok(Some(SnapshotV2NetworkTokenBucketState::new(
            size,
            burst_present.then_some(burst),
            refill,
            budget,
            remaining,
            age,
        )))
    } else {
        Ok(None)
    }
}

fn decode_transport<R: ReservePolicy>(
    bytes: &[u8],
    kind: SnapshotV2DeviceTransportKind,
    reserve: &mut R,
) -> Result<SnapshotV2DeviceTransport, SnapshotV2NetworkStateDecodeError> {
    match kind {
        SnapshotV2DeviceTransportKind::Mmio => {
            decode_mmio(bytes).map(SnapshotV2DeviceTransport::Mmio)
        }
        SnapshotV2DeviceTransportKind::Pci => {
            decode_pci(bytes, reserve).map(SnapshotV2DeviceTransport::Pci)
        }
    }
}

fn decode_mmio(
    bytes: &[u8],
) -> Result<SnapshotV2MmioDeviceState, SnapshotV2NetworkStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let device_feature_select = reader.read_u32()?;
    let driver_feature_select = reader.read_u32()?;
    let queue_select = reader.read_u32()?;
    let interrupt_line = GuestInterruptLine::new(reader.read_u32()?)
        .map_err(|_| SnapshotV2NetworkStateDecodeError::InvalidField)?;
    let region_id = MmioRegionId::new(reader.read_u64()?);
    let start = GuestAddress::new(reader.read_u64()?);
    let size = reader.read_u64()?;
    reader.read_zeroes(8)?;
    reader.finish_exact()?;
    let region = MmioRegion::new(region_id, start, size)
        .map_err(|_| SnapshotV2NetworkStateDecodeError::InvalidField)?;
    Ok(SnapshotV2MmioDeviceState::from_parts(
        device_feature_select,
        driver_feature_select,
        queue_select,
        region,
        interrupt_line,
    ))
}

fn decode_pci<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2PciDeviceState, SnapshotV2NetworkStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let phase = match reader.read_u8()? {
        PCI_PHASE_ACTIVE => VirtioPciEndpointPhase::Active,
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    };
    let origin = match reader.read_u8()? {
        PCI_ORIGIN_STARTUP => StorageDeviceOrigin::Startup,
        PCI_ORIGIN_RUNTIME => StorageDeviceOrigin::Runtime,
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    };
    let bar_index = reader.read_u8()?;
    let bar_address_space = match reader.read_u8()? {
        PCI_BAR_MEMORY64 => PciBarAddressSpace::Memory64,
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    };
    let bar_prefetchable = match reader.read_u8()? {
        PCI_BAR_NOT_PREFETCHABLE => PciBarPrefetchable::No,
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    };
    let pci_cfg_bar = reader.read_u8()?;
    let function = reader.read_u8()?;
    reader.read_zeroes(1)?;
    let segment = reader.read_u16()?;
    let bus = reader.read_u8()?;
    let device = reader.read_u8()?;
    reader.read_zeroes(4)?;
    let bar_start = GuestAddress::new(reader.read_u64()?);
    let bar_size = reader.read_u64()?;
    let device_feature_select = reader.read_u32()?;
    let driver_feature_select = reader.read_u32()?;
    let queue_select = reader.read_u16()?;
    let writable_count = usize::from(reader.read_u16()?);
    let probe_count = usize::from(reader.read_u16()?);
    let entry_count = usize::from(reader.read_u16()?);
    let pending_count = usize::from(reader.read_u16()?);
    let vector_count = usize::from(reader.read_u16()?);
    reader.read_zeroes(4)?;
    let pci_cfg_offset = reader.read_u32()?;
    let pci_cfg_length = reader.read_u32()?;
    let enabled = reader.read_bool()?;
    let function_masked = reader.read_bool()?;
    let pending_transition_observed = reader.read_bool()?;
    reader.read_zeroes(1)?;
    let config_vector = reader.read_u16()?;
    reader.read_zeroes(2)?;
    let mut writable_bytes = Vec::new();
    reserve
        .reserve_vec(&mut writable_bytes, writable_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for _ in 0..writable_count {
        let offset = reader.read_u16()?;
        let value = reader.read_u8()?;
        reader.read_zeroes(1)?;
        writable_bytes.push(SnapshotV2PciWritableByte::from_parts(offset, value));
    }
    let mut bar_probes = Vec::new();
    reserve
        .reserve_vec(&mut bar_probes, probe_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for _ in 0..probe_count {
        let index = reader.read_u8()?;
        let pending = reader.read_bool()?;
        reader.read_zeroes(2)?;
        bar_probes.push(SnapshotV2PciBarProbeState::from_parts(index, pending));
    }
    let mut entries = Vec::new();
    reserve
        .reserve_vec(&mut entries, entry_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for _ in 0..entry_count {
        entries.push(SnapshotV2PciMsixTableEntry::from_parts(
            reader.read_u32()?,
            reader.read_u32()?,
            reader.read_u32()?,
            reader.read_u32()?,
        ));
    }
    let mut pending_words = Vec::new();
    reserve
        .reserve_vec(&mut pending_words, pending_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for _ in 0..pending_count {
        pending_words.push(reader.read_u64()?);
    }
    let mut queue_vectors = Vec::new();
    reserve
        .reserve_vec(&mut queue_vectors, vector_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for _ in 0..vector_count {
        queue_vectors.push(reader.read_u16()?);
    }
    reader.finish_padded()?;
    let sbdf = crate::pci::PciSbdf::new(segment, bus, device, function)
        .map_err(|_| SnapshotV2NetworkStateDecodeError::InvalidField)?;
    let bar_range = GuestMemoryRange::new(bar_start, bar_size)
        .map_err(|_| SnapshotV2NetworkStateDecodeError::InvalidField)?;
    let msix = SnapshotV2PciMsixState::from_parts(SnapshotV2PciMsixStateParts {
        entries,
        pending_words,
        enabled,
        function_masked,
        config_vector,
        queue_vectors,
        pending_transition_observed,
    });
    Ok(SnapshotV2PciDeviceState::from_parts(
        SnapshotV2PciDeviceStateParts {
            phase,
            origin,
            sbdf,
            bar_index,
            bar_address_space,
            bar_prefetchable,
            bar_range,
            device_feature_select,
            driver_feature_select,
            queue_select,
            pci_cfg_bar,
            pci_cfg_offset,
            pci_cfg_length,
            writable_bytes,
            bar_probes,
            msix,
        },
    ))
}

fn decode_mmds<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2MmdsState, SnapshotV2NetworkStateDecodeError> {
    let mut reader = Reader::new(bytes);
    if reader.read_array::<8>()? != MMDS_MAGIC {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    let version = match reader.read_u8()? {
        MMDS_V1 => MmdsVersion::V1,
        MMDS_V2 => MmdsVersion::V2,
        _ => return Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    };
    let imds_compat = reader.read_bool()?;
    let address_present = reader.read_bool()?;
    let interface_count = usize::from(reader.read_u8()?);
    let address = Ipv4Addr::from(reader.read_array::<4>()?);
    if read_usize(&mut reader)? != bytes.len() {
        return Err(SnapshotV2NetworkStateDecodeError::InvalidField);
    }
    reader.read_zeroes(8)?;
    let mut interfaces = Vec::new();
    reserve
        .reserve_vec(&mut interfaces, interface_count)
        .map_err(|()| SnapshotV2NetworkStateDecodeError::Allocation)?;
    for _ in 0..interface_count {
        let index = reader.read_u16()?;
        let mac = EthernetMacAddress::from_octets(reader.read_array::<6>()?);
        let ipv4 = Ipv4Addr::from(reader.read_array::<4>()?);
        let port = reader.read_u16()?;
        reader.read_zeroes(2)?;
        interfaces.push(SnapshotV2MmdsInterfaceState::new(index, mac, ipv4, port));
    }
    reader.finish_exact()?;
    Ok(SnapshotV2MmdsState::new(
        version,
        address_present.then_some(address),
        imds_compat,
        interfaces,
    ))
}

fn decode_feature_capabilities(
    bits: u64,
) -> Result<VirtioNetworkFeatureCapabilities, SnapshotV2NetworkStateDecodeError> {
    let enabled = |feature| bits & (1_u64 << feature) != 0;
    let capabilities = VirtioNetworkFeatureCapabilities::none()
        .with_checksum(enabled(VIRTIO_NET_F_CSUM))
        .with_guest_checksum(enabled(VIRTIO_NET_F_GUEST_CSUM))
        .with_guest_tso4(enabled(VIRTIO_NET_F_GUEST_TSO4))
        .with_guest_tso6(enabled(VIRTIO_NET_F_GUEST_TSO6))
        .with_guest_ufo(enabled(VIRTIO_NET_F_GUEST_UFO))
        .with_host_tso4(enabled(VIRTIO_NET_F_HOST_TSO4))
        .with_host_tso6(enabled(VIRTIO_NET_F_HOST_TSO6))
        .with_host_ufo(enabled(VIRTIO_NET_F_HOST_UFO))
        .with_merged_rx_buffers(enabled(VIRTIO_NET_F_MRG_RXBUF));
    if capabilities.feature_bits() == bits && capabilities.is_dependency_complete() {
        Ok(capabilities)
    } else {
        Err(SnapshotV2NetworkStateDecodeError::InvalidField)
    }
}

fn validate_option_payload(
    flags: u16,
    bit: u16,
    value: &[u8],
) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if flags & bit == 0 && value.iter().any(|byte| *byte != 0) {
        Err(SnapshotV2NetworkStateDecodeError::InvalidField)
    } else {
        Ok(())
    }
}

fn validate_option_scalar(
    flags: u16,
    bit: u16,
    value: u16,
) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if flags & bit == 0 && value != 0 {
        Err(SnapshotV2NetworkStateDecodeError::InvalidField)
    } else {
        Ok(())
    }
}

fn absolute_bounds(
    record_offset: usize,
    relative: SectionBounds,
) -> Result<SectionBounds, SnapshotV2NetworkStateDecodeError> {
    Ok(SectionBounds {
        offset: record_offset
            .checked_add(relative.offset)
            .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?,
        length: relative.length,
    })
}

fn section(
    bytes: &[u8],
    bounds: SectionBounds,
) -> Result<&[u8], SnapshotV2NetworkStateDecodeError> {
    let end = bounds
        .offset
        .checked_add(bounds.length)
        .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?;
    bytes
        .get(bounds.offset..end)
        .ok_or(SnapshotV2NetworkStateDecodeError::Truncated)
}

fn decode_transport_kind(
    tag: u16,
) -> Result<SnapshotV2DeviceTransportKind, SnapshotV2NetworkStateDecodeError> {
    match tag {
        TRANSPORT_MMIO => Ok(SnapshotV2DeviceTransportKind::Mmio),
        TRANSPORT_PCI => Ok(SnapshotV2DeviceTransportKind::Pci),
        _ => Err(SnapshotV2NetworkStateDecodeError::InvalidField),
    }
}

fn transport_tag(kind: SnapshotV2DeviceTransportKind) -> u16 {
    match kind {
        SnapshotV2DeviceTransportKind::Mmio => TRANSPORT_MMIO,
        SnapshotV2DeviceTransportKind::Pci => TRANSPORT_PCI,
    }
}

fn read_usize(reader: &mut Reader<'_>) -> Result<usize, SnapshotV2NetworkStateDecodeError> {
    usize::try_from(reader.read_u64()?)
        .map_err(|_| SnapshotV2NetworkStateDecodeError::InvalidLayout)
}

fn aligned_length(length: usize) -> Option<usize> {
    length
        .checked_add(ALIGNMENT - 1)
        .map(|rounded| rounded & !(ALIGNMENT - 1))
}

fn pad_to(
    output: &mut Vec<u8>,
    start: usize,
    length: usize,
) -> Result<(), SnapshotV2NetworkStateEncodeError> {
    let target = start
        .checked_add(length)
        .ok_or(SnapshotV2NetworkStateEncodeError::LengthOverflow)?;
    if output.len() > target {
        return Err(SnapshotV2NetworkStateEncodeError::LengthOverflow);
    }
    output.resize(target, 0);
    Ok(())
}

fn write_u8(output: &mut Vec<u8>, value: u8) {
    output.push(value);
}

fn write_bool(output: &mut Vec<u8>, value: bool) {
    write_u8(output, u8::from(value));
}

fn write_u16(output: &mut Vec<u8>, value: u16) {
    write_bytes(output, &value.to_le_bytes());
}

fn write_u32(output: &mut Vec<u8>, value: u32) {
    write_bytes(output, &value.to_le_bytes());
}

fn write_u64(output: &mut Vec<u8>, value: u64) {
    write_bytes(output, &value.to_le_bytes());
}

fn write_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(bytes);
}

fn write_zeroes(output: &mut Vec<u8>, count: usize) {
    output.resize(output.len().saturating_add(count), 0);
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    const fn position(&self) -> usize {
        self.position
    }

    fn read_u8(&mut self) -> Result<u8, SnapshotV2NetworkStateDecodeError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(SnapshotV2NetworkStateDecodeError::Truncated)?;
        self.position = self
            .position
            .checked_add(1)
            .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?;
        Ok(value)
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotV2NetworkStateDecodeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotV2NetworkStateDecodeError::InvalidField),
        }
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotV2NetworkStateDecodeError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotV2NetworkStateDecodeError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotV2NetworkStateDecodeError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_array<const LENGTH: usize>(
        &mut self,
    ) -> Result<[u8; LENGTH], SnapshotV2NetworkStateDecodeError> {
        self.read_bytes(LENGTH)?
            .try_into()
            .map_err(|_| SnapshotV2NetworkStateDecodeError::Truncated)
    }

    fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], SnapshotV2NetworkStateDecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(SnapshotV2NetworkStateDecodeError::InvalidLayout)?;
        let bytes = self
            .bytes
            .get(self.position..end)
            .ok_or(SnapshotV2NetworkStateDecodeError::Truncated)?;
        self.position = end;
        Ok(bytes)
    }

    fn read_zeroes(&mut self, length: usize) -> Result<(), SnapshotV2NetworkStateDecodeError> {
        require_zeroes(self.read_bytes(length)?)
    }

    fn finish_exact(self) -> Result<(), SnapshotV2NetworkStateDecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(SnapshotV2NetworkStateDecodeError::InvalidLayout)
        }
    }

    fn finish_padded(self) -> Result<(), SnapshotV2NetworkStateDecodeError> {
        require_zeroes(
            self.bytes
                .get(self.position..)
                .ok_or(SnapshotV2NetworkStateDecodeError::Truncated)?,
        )
    }
}

fn require_zeroes(bytes: &[u8]) -> Result<(), SnapshotV2NetworkStateDecodeError> {
    if bytes.iter().all(|byte| *byte == 0) {
        Ok(())
    } else {
        Err(SnapshotV2NetworkStateDecodeError::InvalidField)
    }
}
