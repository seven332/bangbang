use super::*;

use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemoryRange};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::{PciBarAddressSpace, PciBarPrefetchable, PciSbdf};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceTransportKind, SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState,
    SnapshotV2PciBarProbeState, SnapshotV2PciDeviceState, SnapshotV2PciDeviceStateParts,
    SnapshotV2PciMsixState, SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry,
    SnapshotV2PciWritableByte, SnapshotV2VirtioQueueState, SnapshotV2VirtioStateParts,
};
use crate::virtio_pci::VirtioPciEndpointPhase;

const MAGIC: [u8; 8] = *b"BANGVS2\0";
const PROFILE: u16 = 1;
const FLAGS: u32 = 0;
const SECTION_COUNT: u16 = NATIVE_V2_VSOCK_STATE_SECTION_COUNT as u16;
const ALIGNMENT: usize = 8;
const DIRECTORY_OFFSET: usize = NATIVE_V2_VSOCK_STATE_HEADER_BYTES;
const PAYLOAD_OFFSET: usize = DIRECTORY_OFFSET
    + NATIVE_V2_VSOCK_STATE_SECTION_COUNT * NATIVE_V2_VSOCK_STATE_SECTION_ENTRY_BYTES;

const HEADER_MAGIC_OFFSET: usize = 0;
const HEADER_BYTES_OFFSET: usize = 8;
const HEADER_PROFILE_OFFSET: usize = 10;
const HEADER_TRANSPORT_OFFSET: usize = 12;
const HEADER_SECTION_COUNT_OFFSET: usize = 14;
const HEADER_FLAGS_OFFSET: usize = 16;
const HEADER_RESERVED_OFFSET: usize = 20;
const HEADER_TOTAL_LENGTH_OFFSET: usize = 24;
const HEADER_DIRECTORY_OFFSET_OFFSET: usize = 32;
const HEADER_PAYLOAD_OFFSET_OFFSET: usize = 40;
const HEADER_RESERVED_TAIL_OFFSET: usize = 48;
const HEADER_RESERVED_TAIL_BYTES: usize = 16;

const DIRECTORY_KIND_OFFSET: usize = 0;
const DIRECTORY_FLAGS_OFFSET: usize = 2;
const DIRECTORY_RESERVED_OFFSET: usize = 4;
const DIRECTORY_PAYLOAD_OFFSET: usize = 8;
const DIRECTORY_LENGTH_OFFSET: usize = 16;
const DIRECTORY_RESERVED_TAIL_OFFSET: usize = 24;

const SECTION_LOCAL: u16 = 1;
const SECTION_COMMON: u16 = 2;
const SECTION_TRANSPORT: u16 = 3;
const TRANSPORT_MMIO: u16 = 1;
const TRANSPORT_PCI: u16 = 2;

const LOCAL_ACTIVE: u16 = 1;
const LOCAL_KNOWN_FLAGS: u16 = LOCAL_ACTIVE;
const LOCAL_EVENT_IDX_MASK: u8 = 0b111;
const LOCAL_RESERVED_HEAD_BYTES: usize = 3;
const LOCAL_RESERVED_TAIL_BYTES: usize = 16;

const COMMON_FIXED_BYTES: usize = 32;
const COMMON_QUEUE_BYTES: usize = 32;
const INTERRUPT_QUEUE: u8 = 1;
const INTERRUPT_CONFIGURATION: u8 = 2;

const MMIO_SECTION_BYTES: usize = NATIVE_V2_VSOCK_MMIO_STATE_BYTES;
const PCI_FIXED_BYTES: usize = 72;
const PCI_PHASE_ACTIVE: u8 = 1;
const PCI_ORIGIN_STARTUP: u8 = 1;
const PCI_BAR_MEMORY64: u8 = 2;
const PCI_BAR_NOT_PREFETCHABLE: u8 = 0;
const PCI_WRITABLE_COUNT: usize = 4;
const PCI_PROBE_COUNT: usize = 2;
const PCI_MSIX_ENTRY_COUNT: usize = VIRTIO_VSOCK_QUEUE_COUNT + 1;
const PCI_PENDING_WORD_COUNT: usize = 1;
const PCI_QUEUE_VECTOR_COUNT: usize = VIRTIO_VSOCK_QUEUE_COUNT;
const PCI_WRITABLE_BYTES: usize = PCI_WRITABLE_COUNT * 4;
const PCI_PROBE_BYTES: usize = PCI_PROBE_COUNT * 4;
const PCI_MSIX_ENTRY_BYTES: usize = 16;
const PCI_PENDING_WORD_BYTES: usize = 8;
const PCI_QUEUE_VECTOR_BYTES: usize = 2;

const _: () = assert!(PAYLOAD_OFFSET == 160);
const _: () = assert!(
    PCI_FIXED_BYTES
        + PCI_WRITABLE_BYTES
        + PCI_PROBE_BYTES
        + PCI_MSIX_ENTRY_COUNT * PCI_MSIX_ENTRY_BYTES
        + PCI_PENDING_WORD_BYTES
        + PCI_QUEUE_VECTOR_COUNT * PCI_QUEUE_VECTOR_BYTES
        == 174
);

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

#[derive(Clone, Copy)]
struct SectionBounds {
    offset: usize,
    length: usize,
}

struct Layout {
    local_length: usize,
    common_length: usize,
    transport_length: usize,
    total_length: usize,
}

struct Preflight {
    transport: SnapshotV2DeviceTransportKind,
    local: SectionBounds,
    common: SectionBounds,
    transport_section: SectionBounds,
}

struct LocalState {
    guest_cid: u64,
    backend_selector: VsockBackendSelector,
    host_local_port_cursor: VsockHostLocalPortCursor,
    active_queues: Option<SnapshotV2VsockActiveQueuesState>,
}

pub(super) fn encode(
    version: SnapshotFormatVersion,
    state: &SnapshotV2VsockState,
) -> Result<Vec<u8>, SnapshotV2VsockStateEncodeError> {
    encode_with_policy(version, state, &mut FallibleReserve)
}

pub(super) fn decode(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<SnapshotV2VsockState, SnapshotV2VsockStateDecodeError> {
    decode_with_policy(version, bytes, &mut FallibleReserve)
}

pub(super) fn encode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    state: &SnapshotV2VsockState,
    reserve: &mut R,
) -> Result<Vec<u8>, SnapshotV2VsockStateEncodeError> {
    if version != NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2VsockStateEncodeError::UnsupportedVersion);
    }
    validate_vsock_state(state)
        .map_err(|source| SnapshotV2VsockStateEncodeError::InvalidState { source })?;
    let layout = calculate_layout(state)?;
    let mut output = Vec::new();
    reserve
        .reserve_vec(&mut output, layout.total_length)
        .map_err(|()| SnapshotV2VsockStateEncodeError::Allocation)?;

    write_header(&mut output, state, &layout)?;
    let local_offset = PAYLOAD_OFFSET;
    let common_offset = local_offset
        .checked_add(layout.local_length)
        .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    let transport_offset = common_offset
        .checked_add(layout.common_length)
        .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    write_directory_entry(
        &mut output,
        SECTION_LOCAL,
        local_offset,
        layout.local_length,
    )?;
    write_directory_entry(
        &mut output,
        SECTION_COMMON,
        common_offset,
        layout.common_length,
    )?;
    write_directory_entry(
        &mut output,
        SECTION_TRANSPORT,
        transport_offset,
        layout.transport_length,
    )?;
    encode_local(&mut output, state, layout.local_length)?;
    encode_common(&mut output, state.virtio(), layout.common_length)?;
    encode_transport(&mut output, state.transport(), layout.transport_length)?;
    if output.len() != layout.total_length {
        return Err(SnapshotV2VsockStateEncodeError::LengthOverflow);
    }
    Ok(output)
}

pub(super) fn decode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2VsockState, SnapshotV2VsockStateDecodeError> {
    let preflight = preflight(version, bytes)?;
    let local = decode_local(section_bytes(bytes, preflight.local)?, reserve)?;
    let virtio = decode_common(section_bytes(bytes, preflight.common)?, reserve)?;
    let transport_bytes = section_bytes(bytes, preflight.transport_section)?;
    let transport = match preflight.transport {
        SnapshotV2DeviceTransportKind::Mmio => {
            SnapshotV2DeviceTransport::Mmio(decode_mmio(transport_bytes)?)
        }
        SnapshotV2DeviceTransportKind::Pci => {
            SnapshotV2DeviceTransport::Pci(decode_pci(transport_bytes, reserve)?)
        }
    };
    SnapshotV2VsockState::try_from_parts(SnapshotV2VsockStateParts {
        guest_cid: local.guest_cid,
        backend_selector: local.backend_selector,
        host_local_port_cursor: local.host_local_port_cursor,
        active_queues: local.active_queues,
        virtio,
        transport,
    })
    .map_err(|source| SnapshotV2VsockStateDecodeError::InvalidState { source })
}

fn calculate_layout(
    state: &SnapshotV2VsockState,
) -> Result<Layout, SnapshotV2VsockStateEncodeError> {
    let selector = state.backend_selector().path().to_str().ok_or(
        SnapshotV2VsockStateEncodeError::InvalidState {
            source: SnapshotV2VsockStateBuildError::BackendSelector,
        },
    )?;
    let local_semantic = NATIVE_V2_VSOCK_LOCAL_PREFIX_BYTES
        .checked_add(selector.len())
        .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    let local_length =
        align_up(local_semantic).ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    let common_semantic = COMMON_FIXED_BYTES
        .checked_add(
            state
                .virtio()
                .queues()
                .len()
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?,
        )
        .and_then(|length| {
            length.checked_add(
                state
                    .virtio()
                    .pending_notifications()
                    .len()
                    .checked_mul(2)?,
            )
        })
        .and_then(|length| {
            length.checked_add(state.virtio().interrupt_intents().len().checked_mul(4)?)
        })
        .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    let common_length =
        align_up(common_semantic).ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    let transport_length = match state.transport() {
        SnapshotV2DeviceTransport::Mmio(_) => MMIO_SECTION_BYTES,
        SnapshotV2DeviceTransport::Pci(pci) => calculate_pci_length(pci)?,
    };
    let total_length = PAYLOAD_OFFSET
        .checked_add(local_length)
        .and_then(|length| length.checked_add(common_length))
        .and_then(|length| length.checked_add(transport_length))
        .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    if total_length > NATIVE_V2_VSOCK_STATE_MAX_BYTES
        || total_length > NATIVE_V2_VSOCK_STATE_WORST_CASE_BYTES
        || local_length > MAX_LOCAL_BYTES
        || common_length > NATIVE_V2_VSOCK_COMMON_STATE_MAX_BYTES
    {
        return Err(SnapshotV2VsockStateEncodeError::TooLarge);
    }
    Ok(Layout {
        local_length,
        common_length,
        transport_length,
        total_length,
    })
}

fn calculate_pci_length(
    pci: &SnapshotV2PciDeviceState,
) -> Result<usize, SnapshotV2VsockStateEncodeError> {
    let semantic = PCI_FIXED_BYTES
        .checked_add(
            pci.writable_bytes()
                .len()
                .checked_mul(4)
                .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?,
        )
        .and_then(|length| length.checked_add(pci.bar_probes().len().checked_mul(4)?))
        .and_then(|length| {
            length.checked_add(
                pci.msix()
                    .entries()
                    .len()
                    .checked_mul(PCI_MSIX_ENTRY_BYTES)?,
            )
        })
        .and_then(|length| {
            length.checked_add(
                pci.msix()
                    .pending_words()
                    .len()
                    .checked_mul(PCI_PENDING_WORD_BYTES)?,
            )
        })
        .and_then(|length| {
            length.checked_add(
                pci.msix()
                    .queue_vectors()
                    .len()
                    .checked_mul(PCI_QUEUE_VECTOR_BYTES)?,
            )
        })
        .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    align_up(semantic).ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)
}

fn write_header(
    output: &mut Vec<u8>,
    state: &SnapshotV2VsockState,
    layout: &Layout,
) -> Result<(), SnapshotV2VsockStateEncodeError> {
    write_bytes(output, &MAGIC);
    write_u16(output, NATIVE_V2_VSOCK_STATE_HEADER_BYTES as u16);
    write_u16(output, PROFILE);
    write_u16(output, transport_tag(state.transport().kind()));
    write_u16(output, SECTION_COUNT);
    write_u32(output, FLAGS);
    write_u32(output, 0);
    write_u64(
        output,
        u64::try_from(layout.total_length)
            .map_err(|_| SnapshotV2VsockStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(DIRECTORY_OFFSET)
            .map_err(|_| SnapshotV2VsockStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(PAYLOAD_OFFSET)
            .map_err(|_| SnapshotV2VsockStateEncodeError::LengthOverflow)?,
    );
    write_zeroes(output, HEADER_RESERVED_TAIL_BYTES);
    Ok(())
}

fn write_directory_entry(
    output: &mut Vec<u8>,
    kind: u16,
    offset: usize,
    length: usize,
) -> Result<(), SnapshotV2VsockStateEncodeError> {
    write_u16(output, kind);
    write_u16(output, 0);
    write_u32(output, 0);
    write_u64(
        output,
        u64::try_from(offset).map_err(|_| SnapshotV2VsockStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(length).map_err(|_| SnapshotV2VsockStateEncodeError::LengthOverflow)?,
    );
    write_u64(output, 0);
    Ok(())
}

fn encode_local(
    output: &mut Vec<u8>,
    state: &SnapshotV2VsockState,
    section_length: usize,
) -> Result<(), SnapshotV2VsockStateEncodeError> {
    let start = output.len();
    let selector = state.backend_selector().path().to_str().ok_or(
        SnapshotV2VsockStateEncodeError::InvalidState {
            source: SnapshotV2VsockStateBuildError::BackendSelector,
        },
    )?;
    let active = state.active_queues();
    let mut event_idx_mask = 0_u8;
    if let Some(active) = active {
        for (index, queue) in active.as_array().into_iter().enumerate() {
            if queue.event_idx_enabled() {
                event_idx_mask |= 1 << index;
            }
        }
    }

    write_u64(output, state.guest_cid());
    write_u32(output, state.host_local_port_cursor().last_used());
    write_u16(
        output,
        u16::try_from(selector.len())
            .map_err(|_| SnapshotV2VsockStateEncodeError::LengthOverflow)?,
    );
    write_u16(output, u16::from(active.is_some()) * LOCAL_ACTIVE);
    write_u8(output, event_idx_mask);
    write_zeroes(output, LOCAL_RESERVED_HEAD_BYTES);
    for queue in active
        .map(SnapshotV2VsockActiveQueuesState::as_array)
        .unwrap_or([SnapshotV2VsockQueueState::new(0, 0, false); VIRTIO_VSOCK_QUEUE_COUNT])
    {
        write_u16(output, queue.next_available());
        write_u16(output, queue.next_used());
    }
    write_zeroes(output, LOCAL_RESERVED_TAIL_BYTES);
    write_bytes(output, selector.as_bytes());
    pad_section(output, start, section_length)
}

fn encode_common(
    output: &mut Vec<u8>,
    state: &SnapshotV2VirtioState,
    section_length: usize,
) -> Result<(), SnapshotV2VsockStateEncodeError> {
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
            .map_err(|_| SnapshotV2VsockStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.pending_notifications().len())
            .map_err(|_| SnapshotV2VsockStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.interrupt_intents().len())
            .map_err(|_| SnapshotV2VsockStateEncodeError::LengthOverflow)?,
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
    for index in state.pending_notifications() {
        write_u16(output, *index);
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
    pad_section(output, start, section_length)
}

fn encode_transport(
    output: &mut Vec<u8>,
    state: &SnapshotV2DeviceTransport,
    section_length: usize,
) -> Result<(), SnapshotV2VsockStateEncodeError> {
    match state {
        SnapshotV2DeviceTransport::Mmio(mmio) => encode_mmio(output, mmio, section_length),
        SnapshotV2DeviceTransport::Pci(pci) => encode_pci(output, pci, section_length),
    }
}

fn encode_mmio(
    output: &mut Vec<u8>,
    state: &SnapshotV2MmioDeviceState,
    section_length: usize,
) -> Result<(), SnapshotV2VsockStateEncodeError> {
    let start = output.len();
    write_u32(output, state.device_feature_select());
    write_u32(output, state.driver_feature_select());
    write_u32(output, state.queue_select());
    write_u32(output, state.interrupt_line().raw_value());
    write_u64(output, state.region().id().raw_value());
    write_u64(output, state.region().range().start().raw_value());
    write_u64(output, state.region().range().size());
    write_zeroes(output, 8);
    finish_exact_section(output, start, section_length)
}

fn encode_pci(
    output: &mut Vec<u8>,
    state: &SnapshotV2PciDeviceState,
    section_length: usize,
) -> Result<(), SnapshotV2VsockStateEncodeError> {
    let start = output.len();
    write_u8(output, PCI_PHASE_ACTIVE);
    write_u8(output, PCI_ORIGIN_STARTUP);
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
    write_u16(output, PCI_WRITABLE_COUNT as u16);
    write_u16(output, PCI_PROBE_COUNT as u16);
    write_u16(output, PCI_MSIX_ENTRY_COUNT as u16);
    write_u16(output, PCI_PENDING_WORD_COUNT as u16);
    write_u16(output, PCI_QUEUE_VECTOR_COUNT as u16);
    write_zeroes(output, 4);
    write_u32(output, state.pci_cfg_offset());
    write_u32(output, state.pci_cfg_length());
    write_bool(output, state.msix().enabled());
    write_bool(output, state.msix().function_masked());
    write_bool(output, state.msix().pending_transition_observed());
    write_u8(output, 0);
    write_u16(output, state.msix().config_vector());
    write_u16(output, 0);
    for byte in state.writable_bytes() {
        write_u16(output, byte.offset());
        write_u8(output, byte.value());
        write_u8(output, 0);
    }
    for probe in state.bar_probes() {
        write_u8(output, probe.index());
        write_bool(output, probe.pending());
        write_u16(output, 0);
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
    pad_section(output, start, section_length)
}

fn preflight(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<Preflight, SnapshotV2VsockStateDecodeError> {
    if version != NATIVE_V2_VSOCK_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2VsockStateDecodeError::UnsupportedVersion);
    }
    if bytes.len() > NATIVE_V2_VSOCK_STATE_MAX_BYTES
        || bytes.len() > NATIVE_V2_VSOCK_STATE_WORST_CASE_BYTES
    {
        return Err(SnapshotV2VsockStateDecodeError::TooLarge);
    }
    if bytes.len() < PAYLOAD_OFFSET {
        return Err(SnapshotV2VsockStateDecodeError::Truncated);
    }
    if read_array_at::<8>(bytes, HEADER_MAGIC_OFFSET)? != MAGIC {
        return Err(SnapshotV2VsockStateDecodeError::InvalidMagic);
    }
    if usize::from(read_u16_at(bytes, HEADER_BYTES_OFFSET)?) != NATIVE_V2_VSOCK_STATE_HEADER_BYTES
        || read_u16_at(bytes, HEADER_PROFILE_OFFSET)? != PROFILE
    {
        return Err(SnapshotV2VsockStateDecodeError::InvalidProfile);
    }
    let transport = match read_u16_at(bytes, HEADER_TRANSPORT_OFFSET)? {
        TRANSPORT_MMIO => SnapshotV2DeviceTransportKind::Mmio,
        TRANSPORT_PCI => SnapshotV2DeviceTransportKind::Pci,
        _ => return Err(SnapshotV2VsockStateDecodeError::InvalidTransport),
    };
    if read_u16_at(bytes, HEADER_SECTION_COUNT_OFFSET)? != SECTION_COUNT
        || read_u32_at(bytes, HEADER_FLAGS_OFFSET)? != FLAGS
        || read_u32_at(bytes, HEADER_RESERVED_OFFSET)? != 0
        || read_usize_u64_at(bytes, HEADER_TOTAL_LENGTH_OFFSET)? != bytes.len()
        || read_usize_u64_at(bytes, HEADER_DIRECTORY_OFFSET_OFFSET)? != DIRECTORY_OFFSET
        || read_usize_u64_at(bytes, HEADER_PAYLOAD_OFFSET_OFFSET)? != PAYLOAD_OFFSET
    {
        return Err(SnapshotV2VsockStateDecodeError::InvalidStructure);
    }
    require_zeroes(slice_at(
        bytes,
        HEADER_RESERVED_TAIL_OFFSET,
        HEADER_RESERVED_TAIL_BYTES,
    )?)?;

    let expected_kinds = [SECTION_LOCAL, SECTION_COMMON, SECTION_TRANSPORT];
    let mut sections = [SectionBounds {
        offset: 0,
        length: 0,
    }; NATIVE_V2_VSOCK_STATE_SECTION_COUNT];
    let mut expected_offset = PAYLOAD_OFFSET;
    for (index, kind) in expected_kinds.into_iter().enumerate() {
        let entry_offset = DIRECTORY_OFFSET
            .checked_add(
                index
                    .checked_mul(NATIVE_V2_VSOCK_STATE_SECTION_ENTRY_BYTES)
                    .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)?,
            )
            .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)?;
        if read_u16_at(bytes, entry_offset + DIRECTORY_KIND_OFFSET)? != kind
            || read_u16_at(bytes, entry_offset + DIRECTORY_FLAGS_OFFSET)? != 0
            || read_u32_at(bytes, entry_offset + DIRECTORY_RESERVED_OFFSET)? != 0
            || read_u64_at(bytes, entry_offset + DIRECTORY_RESERVED_TAIL_OFFSET)? != 0
        {
            return Err(SnapshotV2VsockStateDecodeError::InvalidStructure);
        }
        let offset = read_usize_u64_at(bytes, entry_offset + DIRECTORY_PAYLOAD_OFFSET)?;
        let length = read_usize_u64_at(bytes, entry_offset + DIRECTORY_LENGTH_OFFSET)?;
        if length == 0
            || offset != expected_offset
            || !offset.is_multiple_of(ALIGNMENT)
            || !length.is_multiple_of(ALIGNMENT)
        {
            return Err(SnapshotV2VsockStateDecodeError::InvalidStructure);
        }
        expected_offset = offset
            .checked_add(length)
            .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)?;
        if expected_offset > bytes.len() {
            return Err(SnapshotV2VsockStateDecodeError::Truncated);
        }
        *sections
            .get_mut(index)
            .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)? =
            SectionBounds { offset, length };
    }
    if expected_offset != bytes.len() {
        return Err(SnapshotV2VsockStateDecodeError::InvalidStructure);
    }

    let [local, common, transport_section] = sections;
    preflight_local(section_bytes(bytes, local)?)?;
    preflight_common(section_bytes(bytes, common)?)?;
    match transport {
        SnapshotV2DeviceTransportKind::Mmio => {
            preflight_mmio(section_bytes(bytes, transport_section)?)?
        }
        SnapshotV2DeviceTransportKind::Pci => {
            preflight_pci(section_bytes(bytes, transport_section)?)?
        }
    }
    Ok(Preflight {
        transport,
        local,
        common,
        transport_section,
    })
}

fn preflight_local(bytes: &[u8]) -> Result<(), SnapshotV2VsockStateDecodeError> {
    if bytes.len() < NATIVE_V2_VSOCK_LOCAL_PREFIX_BYTES + ALIGNMENT
        || bytes.len() > MAX_LOCAL_BYTES
        || !bytes.len().is_multiple_of(ALIGNMENT)
    {
        return Err(SnapshotV2VsockStateDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    reader.read_u64()?;
    reader.read_u32()?;
    let selector_length = usize::from(reader.read_u16()?);
    let flags = reader.read_u16()?;
    let event_idx_mask = reader.read_u8()?;
    reader.read_zeroes(LOCAL_RESERVED_HEAD_BYTES)?;
    let mut cursors = [(0_u16, 0_u16); VIRTIO_VSOCK_QUEUE_COUNT];
    for cursor in &mut cursors {
        *cursor = (reader.read_u16()?, reader.read_u16()?);
    }
    reader.read_zeroes(LOCAL_RESERVED_TAIL_BYTES)?;
    if !(1..=NATIVE_V2_VSOCK_MAX_SELECTOR_BYTES).contains(&selector_length)
        || flags & !LOCAL_KNOWN_FLAGS != 0
        || event_idx_mask & !LOCAL_EVENT_IDX_MASK != 0
        || align_up(
            NATIVE_V2_VSOCK_LOCAL_PREFIX_BYTES
                .checked_add(selector_length)
                .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)?,
        ) != Some(bytes.len())
        || flags & LOCAL_ACTIVE == 0
            && (event_idx_mask != 0
                || cursors
                    .iter()
                    .any(|(next_available, next_used)| *next_available != 0 || *next_used != 0))
    {
        return Err(SnapshotV2VsockStateDecodeError::InvalidValue);
    }
    std::str::from_utf8(reader.read_bytes(selector_length)?)
        .map_err(|_| SnapshotV2VsockStateDecodeError::InvalidUtf8)?;
    reader.finish_padded()
}

fn decode_local<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<LocalState, SnapshotV2VsockStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let guest_cid = reader.read_u64()?;
    let cursor_value = reader.read_u32()?;
    let selector_length = usize::from(reader.read_u16()?);
    let flags = reader.read_u16()?;
    let event_idx_mask = reader.read_u8()?;
    reader.read_zeroes(LOCAL_RESERVED_HEAD_BYTES)?;
    let mut cursors = [(0_u16, 0_u16); VIRTIO_VSOCK_QUEUE_COUNT];
    for cursor in &mut cursors {
        *cursor = (reader.read_u16()?, reader.read_u16()?);
    }
    reader.read_zeroes(LOCAL_RESERVED_TAIL_BYTES)?;
    let selector_bytes = reader.read_bytes(selector_length)?;
    let selector_text = std::str::from_utf8(selector_bytes)
        .map_err(|_| SnapshotV2VsockStateDecodeError::InvalidUtf8)?;
    reader.finish_padded()?;

    let mut selector = String::new();
    reserve
        .reserve_string(&mut selector, selector_length)
        .map_err(|()| SnapshotV2VsockStateDecodeError::Allocation)?;
    selector.push_str(selector_text);
    let backend_selector = VsockBackendSelector::try_from_string(selector)
        .map_err(|_| SnapshotV2VsockStateDecodeError::InvalidValue)?;
    let host_local_port_cursor = VsockHostLocalPortCursor::try_from_last_used(cursor_value)
        .map_err(|_| SnapshotV2VsockStateDecodeError::InvalidValue)?;
    let active_queues = if flags & LOCAL_ACTIVE == 0 {
        None
    } else {
        let [
            (rx_available, rx_used),
            (tx_available, tx_used),
            (event_available, event_used),
        ] = cursors;
        Some(SnapshotV2VsockActiveQueuesState::new(
            SnapshotV2VsockQueueState::new(rx_available, rx_used, event_idx_mask & 0b001 != 0),
            SnapshotV2VsockQueueState::new(tx_available, tx_used, event_idx_mask & 0b010 != 0),
            SnapshotV2VsockQueueState::new(
                event_available,
                event_used,
                event_idx_mask & 0b100 != 0,
            ),
        ))
    };
    Ok(LocalState {
        guest_cid,
        backend_selector,
        host_local_port_cursor,
        active_queues,
    })
}

fn preflight_common(bytes: &[u8]) -> Result<(), SnapshotV2VsockStateDecodeError> {
    let minimum = COMMON_FIXED_BYTES + VIRTIO_VSOCK_QUEUE_COUNT * COMMON_QUEUE_BYTES;
    if bytes.len() < minimum
        || bytes.len() > NATIVE_V2_VSOCK_COMMON_STATE_MAX_BYTES
        || !bytes.len().is_multiple_of(ALIGNMENT)
    {
        return Err(SnapshotV2VsockStateDecodeError::InvalidStructure);
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
    if queue_count != VIRTIO_VSOCK_QUEUE_COUNT
        || notification_count > VIRTIO_VSOCK_QUEUE_COUNT
        || intent_count > VIRTIO_VSOCK_QUEUE_COUNT + 1
    {
        return Err(SnapshotV2VsockStateDecodeError::InvalidValue);
    }
    let semantic_length = COMMON_FIXED_BYTES
        .checked_add(
            queue_count
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)?,
        )
        .and_then(|length| length.checked_add(notification_count.checked_mul(2)?))
        .and_then(|length| length.checked_add(intent_count.checked_mul(4)?))
        .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)?;
    if align_up(semantic_length) != Some(bytes.len()) {
        return Err(SnapshotV2VsockStateDecodeError::InvalidStructure);
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
        let tag = reader.read_u8()?;
        reader.read_zeroes(1)?;
        let queue_index = reader.read_u16()?;
        if !matches!(
            (tag, queue_index),
            (INTERRUPT_QUEUE, 0..=2) | (INTERRUPT_CONFIGURATION, 0)
        ) {
            return Err(SnapshotV2VsockStateDecodeError::InvalidValue);
        }
    }
    reader.finish_padded()
}

fn decode_common<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2VirtioState, SnapshotV2VsockStateDecodeError> {
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
        .map_err(|()| SnapshotV2VsockStateDecodeError::Allocation)?;
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
        .map_err(|()| SnapshotV2VsockStateDecodeError::Allocation)?;
    for _ in 0..notification_count {
        pending_notifications.push(reader.read_u16()?);
    }
    let mut interrupt_intents = Vec::new();
    reserve
        .reserve_vec(&mut interrupt_intents, intent_count)
        .map_err(|()| SnapshotV2VsockStateDecodeError::Allocation)?;
    for _ in 0..intent_count {
        let tag = reader.read_u8()?;
        reader.read_zeroes(1)?;
        let queue_index = reader.read_u16()?;
        interrupt_intents.push(match tag {
            INTERRUPT_QUEUE => SnapshotV2InterruptIntent::Queue { queue_index },
            INTERRUPT_CONFIGURATION if queue_index == 0 => SnapshotV2InterruptIntent::Configuration,
            _ => return Err(SnapshotV2VsockStateDecodeError::InvalidValue),
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

fn preflight_mmio(bytes: &[u8]) -> Result<(), SnapshotV2VsockStateDecodeError> {
    if bytes.len() != MMIO_SECTION_BYTES {
        return Err(SnapshotV2VsockStateDecodeError::InvalidStructure);
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

fn decode_mmio(bytes: &[u8]) -> Result<SnapshotV2MmioDeviceState, SnapshotV2VsockStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let device_feature_select = reader.read_u32()?;
    let driver_feature_select = reader.read_u32()?;
    let queue_select = reader.read_u32()?;
    let interrupt_line = GuestInterruptLine::new(reader.read_u32()?)
        .map_err(|_| SnapshotV2VsockStateDecodeError::InvalidValue)?;
    let region_id = MmioRegionId::new(reader.read_u64()?);
    let region_start = GuestAddress::new(reader.read_u64()?);
    let region_size = reader.read_u64()?;
    reader.read_zeroes(8)?;
    reader.finish_exact()?;
    let region = MmioRegion::new(region_id, region_start, region_size)
        .map_err(|_| SnapshotV2VsockStateDecodeError::InvalidValue)?;
    Ok(SnapshotV2MmioDeviceState::from_parts(
        device_feature_select,
        driver_feature_select,
        queue_select,
        region,
        interrupt_line,
    ))
}

fn preflight_pci(bytes: &[u8]) -> Result<(), SnapshotV2VsockStateDecodeError> {
    if bytes.len() != NATIVE_V2_VSOCK_PCI_STATE_BYTES {
        return Err(SnapshotV2VsockStateDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    if reader.read_u8()? != PCI_PHASE_ACTIVE || reader.read_u8()? != PCI_ORIGIN_STARTUP {
        return Err(SnapshotV2VsockStateDecodeError::InvalidValue);
    }
    reader.read_u8()?;
    if reader.read_u8()? != PCI_BAR_MEMORY64 || reader.read_u8()? != PCI_BAR_NOT_PREFETCHABLE {
        return Err(SnapshotV2VsockStateDecodeError::InvalidValue);
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
        return Err(SnapshotV2VsockStateDecodeError::InvalidValue);
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
    for _ in 0..PCI_MSIX_ENTRY_COUNT {
        reader.read_bytes(PCI_MSIX_ENTRY_BYTES)?;
    }
    reader.read_u64()?;
    for _ in 0..PCI_QUEUE_VECTOR_COUNT {
        reader.read_u16()?;
    }
    reader.finish_padded()
}

fn decode_pci<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2PciDeviceState, SnapshotV2VsockStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let phase = match reader.read_u8()? {
        PCI_PHASE_ACTIVE => VirtioPciEndpointPhase::Active,
        _ => return Err(SnapshotV2VsockStateDecodeError::InvalidValue),
    };
    let origin = match reader.read_u8()? {
        PCI_ORIGIN_STARTUP => StorageDeviceOrigin::Startup,
        _ => return Err(SnapshotV2VsockStateDecodeError::InvalidValue),
    };
    let bar_index = reader.read_u8()?;
    let bar_address_space = match reader.read_u8()? {
        PCI_BAR_MEMORY64 => PciBarAddressSpace::Memory64,
        _ => return Err(SnapshotV2VsockStateDecodeError::InvalidValue),
    };
    let bar_prefetchable = match reader.read_u8()? {
        PCI_BAR_NOT_PREFETCHABLE => PciBarPrefetchable::No,
        _ => return Err(SnapshotV2VsockStateDecodeError::InvalidValue),
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
        .map_err(|()| SnapshotV2VsockStateDecodeError::Allocation)?;
    for _ in 0..writable_count {
        let offset = reader.read_u16()?;
        let value = reader.read_u8()?;
        reader.read_zeroes(1)?;
        writable_bytes.push(SnapshotV2PciWritableByte::from_parts(offset, value));
    }
    let mut bar_probes = Vec::new();
    reserve
        .reserve_vec(&mut bar_probes, probe_count)
        .map_err(|()| SnapshotV2VsockStateDecodeError::Allocation)?;
    for _ in 0..probe_count {
        let index = reader.read_u8()?;
        let pending = reader.read_bool()?;
        reader.read_zeroes(2)?;
        bar_probes.push(SnapshotV2PciBarProbeState::from_parts(index, pending));
    }
    let mut entries = Vec::new();
    reserve
        .reserve_vec(&mut entries, entry_count)
        .map_err(|()| SnapshotV2VsockStateDecodeError::Allocation)?;
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
        .map_err(|()| SnapshotV2VsockStateDecodeError::Allocation)?;
    for _ in 0..pending_count {
        pending_words.push(reader.read_u64()?);
    }
    let mut queue_vectors = Vec::new();
    reserve
        .reserve_vec(&mut queue_vectors, vector_count)
        .map_err(|()| SnapshotV2VsockStateDecodeError::Allocation)?;
    for _ in 0..vector_count {
        queue_vectors.push(reader.read_u16()?);
    }
    reader.finish_padded()?;

    let sbdf = PciSbdf::new(segment, bus, device, function)
        .map_err(|_| SnapshotV2VsockStateDecodeError::InvalidValue)?;
    let bar_range = GuestMemoryRange::new(bar_start, bar_size)
        .map_err(|_| SnapshotV2VsockStateDecodeError::InvalidValue)?;
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

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, SnapshotV2VsockStateDecodeError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(SnapshotV2VsockStateDecodeError::Truncated)?;
        self.position = self
            .position
            .checked_add(1)
            .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)?;
        Ok(value)
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotV2VsockStateDecodeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotV2VsockStateDecodeError::InvalidValue),
        }
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotV2VsockStateDecodeError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotV2VsockStateDecodeError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotV2VsockStateDecodeError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_array<const LENGTH: usize>(
        &mut self,
    ) -> Result<[u8; LENGTH], SnapshotV2VsockStateDecodeError> {
        self.read_bytes(LENGTH)?
            .try_into()
            .map_err(|_| SnapshotV2VsockStateDecodeError::Truncated)
    }

    fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], SnapshotV2VsockStateDecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(SnapshotV2VsockStateDecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn read_zeroes(&mut self, length: usize) -> Result<(), SnapshotV2VsockStateDecodeError> {
        require_zeroes(self.read_bytes(length)?)
    }

    fn finish_exact(self) -> Result<(), SnapshotV2VsockStateDecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(SnapshotV2VsockStateDecodeError::InvalidStructure)
        }
    }

    fn finish_padded(self) -> Result<(), SnapshotV2VsockStateDecodeError> {
        require_zeroes(
            self.bytes
                .get(self.position..)
                .ok_or(SnapshotV2VsockStateDecodeError::Truncated)?,
        )
    }
}

fn section_bytes(
    bytes: &[u8],
    bounds: SectionBounds,
) -> Result<&[u8], SnapshotV2VsockStateDecodeError> {
    slice_at(bytes, bounds.offset, bounds.length)
}

fn slice_at(
    bytes: &[u8],
    offset: usize,
    length: usize,
) -> Result<&[u8], SnapshotV2VsockStateDecodeError> {
    let end = offset
        .checked_add(length)
        .ok_or(SnapshotV2VsockStateDecodeError::InvalidStructure)?;
    bytes
        .get(offset..end)
        .ok_or(SnapshotV2VsockStateDecodeError::Truncated)
}

fn read_u16_at(bytes: &[u8], offset: usize) -> Result<u16, SnapshotV2VsockStateDecodeError> {
    Ok(u16::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u32_at(bytes: &[u8], offset: usize) -> Result<u32, SnapshotV2VsockStateDecodeError> {
    Ok(u32::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u64_at(bytes: &[u8], offset: usize) -> Result<u64, SnapshotV2VsockStateDecodeError> {
    Ok(u64::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_usize_u64_at(
    bytes: &[u8],
    offset: usize,
) -> Result<usize, SnapshotV2VsockStateDecodeError> {
    usize::try_from(read_u64_at(bytes, offset)?)
        .map_err(|_| SnapshotV2VsockStateDecodeError::InvalidStructure)
}

fn read_array_at<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotV2VsockStateDecodeError> {
    slice_at(bytes, offset, LENGTH)?
        .try_into()
        .map_err(|_| SnapshotV2VsockStateDecodeError::Truncated)
}

fn require_zeroes(bytes: &[u8]) -> Result<(), SnapshotV2VsockStateDecodeError> {
    if bytes.iter().any(|byte| *byte != 0) {
        Err(SnapshotV2VsockStateDecodeError::NonzeroReserved)
    } else {
        Ok(())
    }
}

fn align_up(length: usize) -> Option<usize> {
    length
        .checked_add(ALIGNMENT - 1)
        .map(|rounded| rounded & !(ALIGNMENT - 1))
}

fn transport_tag(kind: SnapshotV2DeviceTransportKind) -> u16 {
    match kind {
        SnapshotV2DeviceTransportKind::Mmio => TRANSPORT_MMIO,
        SnapshotV2DeviceTransportKind::Pci => TRANSPORT_PCI,
    }
}

fn pad_section(
    output: &mut Vec<u8>,
    start: usize,
    section_length: usize,
) -> Result<(), SnapshotV2VsockStateEncodeError> {
    let written = output
        .len()
        .checked_sub(start)
        .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    let padding = section_length
        .checked_sub(written)
        .ok_or(SnapshotV2VsockStateEncodeError::LengthOverflow)?;
    write_zeroes(output, padding);
    Ok(())
}

fn finish_exact_section(
    output: &[u8],
    start: usize,
    section_length: usize,
) -> Result<(), SnapshotV2VsockStateEncodeError> {
    if output
        .len()
        .checked_sub(start)
        .is_some_and(|written| written == section_length)
    {
        Ok(())
    } else {
        Err(SnapshotV2VsockStateEncodeError::LengthOverflow)
    }
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
    output.resize(output.len() + count, 0);
}
