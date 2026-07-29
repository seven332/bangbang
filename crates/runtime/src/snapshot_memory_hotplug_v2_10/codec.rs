use super::*;
use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestAddress, GuestMemoryRange};
use crate::memory_hotplug::{MemoryHotplugConfigInput, VIRTIO_MEM_CONFIG_SPACE_SIZE};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::{PciBarAddressSpace, PciBarPrefetchable, PciSbdf};
use crate::snapshot_device_v2::{
    SnapshotV2DeviceTransportKind, SnapshotV2InterruptIntent, SnapshotV2MmioDeviceState,
    SnapshotV2PciBarProbeState, SnapshotV2PciDeviceState, SnapshotV2PciDeviceStateParts,
    SnapshotV2PciMsixState, SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry,
    SnapshotV2PciWritableByte, SnapshotV2VirtioQueueState, SnapshotV2VirtioStateParts,
};
use crate::virtio_pci::VirtioPciEndpointPhase;

const MAGIC: [u8; 8] = *b"BANGME2\0";
const PROFILE: u16 = 1;
const FLAGS: u32 = 0;
const SECTION_COUNT: u16 = 4;
const SECTION_COUNT_USIZE: usize = 4;
const ALIGNMENT: usize = 8;
const DIRECTORY_OFFSET: usize = NATIVE_V2_MEMORY_HOTPLUG_STATE_HEADER_BYTES;
const PAYLOAD_OFFSET: usize =
    DIRECTORY_OFFSET + NATIVE_V2_MEMORY_HOTPLUG_STATE_SECTION_ENTRY_BYTES * SECTION_COUNT_USIZE;

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
const SECTION_BITMAP: u16 = 3;
const SECTION_TRANSPORT: u16 = 4;
const TRANSPORT_MMIO: u16 = 1;
const TRANSPORT_PCI: u16 = 2;

const COMMON_FIXED_BYTES: usize = 32;
const COMMON_QUEUE_BYTES: usize = 32;
const COMMON_MAX_BYTES: usize = 80;
const INTERRUPT_QUEUE: u8 = 1;
const INTERRUPT_CONFIGURATION: u8 = 2;

const MMIO_SECTION_BYTES: usize = 48;
const PCI_SECTION_BYTES: usize = 144;
const PCI_PHASE_ACTIVE: u8 = 1;
const PCI_ORIGIN_STARTUP: u8 = 1;
const PCI_BAR_MEMORY64: u8 = 2;
const PCI_BAR_NOT_PREFETCHABLE: u8 = 0;
const PCI_WRITABLE_COUNT: usize = 4;
const PCI_PROBE_COUNT: usize = 2;
const PCI_MSIX_ENTRY_COUNT: usize = 2;
const PCI_PENDING_WORD_COUNT: usize = 1;
const PCI_QUEUE_VECTOR_COUNT: usize = 1;

pub(super) trait ReservePolicy {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()>;
}

struct FallibleReserve;

impl ReservePolicy for FallibleReserve {
    fn reserve_vec<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), ()> {
        values.try_reserve_exact(additional).map_err(|_| ())
    }
}

#[derive(Clone, Copy)]
struct SectionBounds {
    offset: usize,
    length: usize,
}

struct Layout {
    common_length: usize,
    bitmap_length: usize,
    transport_length: usize,
    total_length: usize,
}

#[derive(Clone, Copy)]
struct LocalState {
    config: MemoryHotplugConfig,
    config_space: VirtioMemConfigSpace,
    active_queue: Option<SnapshotV2MemoryHotplugQueueState>,
}

struct Preflight {
    transport: SnapshotV2DeviceTransportKind,
    local: LocalState,
    bitmap_raw_length: usize,
    common: SectionBounds,
    bitmap: SectionBounds,
    transport_section: SectionBounds,
}

pub(super) fn encode(
    version: SnapshotFormatVersion,
    state: &SnapshotV2MemoryHotplugState,
) -> Result<Vec<u8>, SnapshotV2MemoryHotplugStateEncodeError> {
    encode_with_policy(version, state, &mut FallibleReserve)
}

pub(super) fn decode(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<SnapshotV2MemoryHotplugState, SnapshotV2MemoryHotplugStateDecodeError> {
    decode_with_policy(version, bytes, &mut FallibleReserve)
}

pub(super) fn encode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    state: &SnapshotV2MemoryHotplugState,
    reserve: &mut R,
) -> Result<Vec<u8>, SnapshotV2MemoryHotplugStateEncodeError> {
    if version != NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2MemoryHotplugStateEncodeError::UnsupportedVersion);
    }
    validate_memory_hotplug_state(state)
        .map_err(SnapshotV2MemoryHotplugStateEncodeError::InvalidState)?;
    let layout = calculate_layout(state)?;
    let mut output = Vec::new();
    reserve
        .reserve_vec(&mut output, layout.total_length)
        .map_err(|()| SnapshotV2MemoryHotplugStateEncodeError::Allocation)?;

    write_header(&mut output, state, &layout)?;
    let local_offset = PAYLOAD_OFFSET;
    let common_offset = local_offset
        .checked_add(NATIVE_V2_MEMORY_HOTPLUG_STATE_LOCAL_BYTES)
        .ok_or(SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?;
    let bitmap_offset = common_offset
        .checked_add(layout.common_length)
        .ok_or(SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?;
    let transport_offset = bitmap_offset
        .checked_add(layout.bitmap_length)
        .ok_or(SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?;
    write_directory_entry(
        &mut output,
        SECTION_LOCAL,
        local_offset,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_LOCAL_BYTES,
    )?;
    write_directory_entry(
        &mut output,
        SECTION_COMMON,
        common_offset,
        layout.common_length,
    )?;
    write_directory_entry(
        &mut output,
        SECTION_BITMAP,
        bitmap_offset,
        layout.bitmap_length,
    )?;
    write_directory_entry(
        &mut output,
        SECTION_TRANSPORT,
        transport_offset,
        layout.transport_length,
    )?;
    encode_local(&mut output, state);
    encode_common(&mut output, state.virtio(), layout.common_length)?;
    encode_bitmap(&mut output, state.plugged_bitmap(), layout.bitmap_length)?;
    encode_transport(&mut output, state.transport(), layout.transport_length)?;
    if output.len() != layout.total_length {
        return Err(SnapshotV2MemoryHotplugStateEncodeError::InvalidState(
            SnapshotV2MemoryHotplugStateBuildError::Transport,
        ));
    }
    Ok(output)
}

pub(super) fn decode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2MemoryHotplugState, SnapshotV2MemoryHotplugStateDecodeError> {
    let preflight = preflight(version, bytes)?;
    let virtio = decode_common(section_bytes(bytes, preflight.common)?, reserve)?;
    let bitmap_section = section_bytes(bytes, preflight.bitmap)?;
    let mut plugged_bitmap = Vec::new();
    reserve
        .reserve_vec(&mut plugged_bitmap, preflight.bitmap_raw_length)
        .map_err(|()| SnapshotV2MemoryHotplugStateDecodeError::Allocation)?;
    plugged_bitmap.extend_from_slice(
        bitmap_section
            .get(..preflight.bitmap_raw_length)
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::Truncated)?,
    );
    let transport_bytes = section_bytes(bytes, preflight.transport_section)?;
    let transport = match preflight.transport {
        SnapshotV2DeviceTransportKind::Mmio => {
            SnapshotV2DeviceTransport::Mmio(decode_mmio(transport_bytes)?)
        }
        SnapshotV2DeviceTransportKind::Pci => {
            SnapshotV2DeviceTransport::Pci(decode_pci(transport_bytes, reserve)?)
        }
    };
    SnapshotV2MemoryHotplugState::try_new(
        preflight.local.config,
        preflight.local.config_space,
        preflight.local.active_queue,
        plugged_bitmap,
        virtio,
        transport,
    )
    .map_err(SnapshotV2MemoryHotplugStateDecodeError::InvalidState)
}

fn calculate_layout(
    state: &SnapshotV2MemoryHotplugState,
) -> Result<Layout, SnapshotV2MemoryHotplugStateEncodeError> {
    let common_semantic = COMMON_FIXED_BYTES
        .checked_add(
            state
                .virtio()
                .queues()
                .len()
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?,
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
        .ok_or(SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?;
    let common_length =
        align_up(common_semantic).ok_or(SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?;
    let bitmap_length = align_up(state.plugged_bitmap().len())
        .ok_or(SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?;
    let transport_length = match state.transport() {
        SnapshotV2DeviceTransport::Mmio(_) => MMIO_SECTION_BYTES,
        SnapshotV2DeviceTransport::Pci(_) => PCI_SECTION_BYTES,
    };
    let total_length = PAYLOAD_OFFSET
        .checked_add(NATIVE_V2_MEMORY_HOTPLUG_STATE_LOCAL_BYTES)
        .and_then(|length| length.checked_add(common_length))
        .and_then(|length| length.checked_add(bitmap_length))
        .and_then(|length| length.checked_add(transport_length))
        .ok_or(SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?;
    if total_length > NATIVE_V2_MEMORY_HOTPLUG_STATE_MAX_BYTES {
        return Err(SnapshotV2MemoryHotplugStateEncodeError::TooLarge);
    }
    Ok(Layout {
        common_length,
        bitmap_length,
        transport_length,
        total_length,
    })
}

fn write_header(
    output: &mut Vec<u8>,
    state: &SnapshotV2MemoryHotplugState,
    layout: &Layout,
) -> Result<(), SnapshotV2MemoryHotplugStateEncodeError> {
    write_bytes(output, &MAGIC);
    write_u16(output, NATIVE_V2_MEMORY_HOTPLUG_STATE_HEADER_BYTES as u16);
    write_u16(output, PROFILE);
    write_u16(output, transport_tag(state.transport().kind()));
    write_u16(output, SECTION_COUNT);
    write_u32(output, FLAGS);
    write_u32(output, 0);
    write_u64(
        output,
        u64::try_from(layout.total_length)
            .map_err(|_| SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(DIRECTORY_OFFSET)
            .map_err(|_| SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(PAYLOAD_OFFSET)
            .map_err(|_| SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?,
    );
    write_zeroes(output, HEADER_RESERVED_TAIL_BYTES);
    debug_assert_eq!(output.len(), NATIVE_V2_MEMORY_HOTPLUG_STATE_HEADER_BYTES);
    Ok(())
}

fn write_directory_entry(
    output: &mut Vec<u8>,
    kind: u16,
    offset: usize,
    length: usize,
) -> Result<(), SnapshotV2MemoryHotplugStateEncodeError> {
    write_u16(output, kind);
    write_u16(output, 0);
    write_u32(output, 0);
    write_u64(
        output,
        u64::try_from(offset)
            .map_err(|_| SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(length)
            .map_err(|_| SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?,
    );
    write_u64(output, 0);
    Ok(())
}

fn encode_local(output: &mut Vec<u8>, state: &SnapshotV2MemoryHotplugState) {
    let start = output.len();
    write_u64(output, state.config().total_size_mib());
    write_u64(output, state.config().block_size_mib());
    write_u64(output, state.config().slot_size_mib());
    write_bytes(output, &state.config_space().to_le_bytes());
    write_bool(output, state.active_queue().is_some());
    write_zeroes(output, 3);
    let (next_available, next_used) = state
        .active_queue()
        .map_or((0, 0), |queue| (queue.next_available(), queue.next_used()));
    write_u16(output, next_available);
    write_u16(output, next_used);
    write_zeroes(output, 8);
    debug_assert_eq!(
        output.len() - start,
        NATIVE_V2_MEMORY_HOTPLUG_STATE_LOCAL_BYTES
    );
}

fn encode_common(
    output: &mut Vec<u8>,
    common: &SnapshotV2VirtioState,
    section_length: usize,
) -> Result<(), SnapshotV2MemoryHotplugStateEncodeError> {
    let start = output.len();
    write_u64(output, common.available_features());
    write_u64(output, common.driver_features());
    write_u32(output, common.config_generation());
    write_u32(output, common.status());
    write_bool(output, common.is_activated());
    write_u8(output, 0);
    for count in [
        common.queues().len(),
        common.pending_notifications().len(),
        common.interrupt_intents().len(),
    ] {
        write_u16(
            output,
            u16::try_from(count)
                .map_err(|_| SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?,
        );
    }
    for queue in common.queues() {
        write_u16(output, queue.max_size());
        write_u16(output, queue.size());
        write_bool(output, queue.ready());
        write_zeroes(output, 3);
        write_u64(output, queue.descriptor_table().raw_value());
        write_u64(output, queue.driver_ring().raw_value());
        write_u64(output, queue.device_ring().raw_value());
    }
    for notification in common.pending_notifications() {
        write_u16(output, *notification);
    }
    for intent in common.interrupt_intents() {
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

fn encode_bitmap(
    output: &mut Vec<u8>,
    bitmap: &[u8],
    section_length: usize,
) -> Result<(), SnapshotV2MemoryHotplugStateEncodeError> {
    let start = output.len();
    write_bytes(output, bitmap);
    pad_section(output, start, section_length)
}

fn encode_transport(
    output: &mut Vec<u8>,
    transport: &SnapshotV2DeviceTransport,
    section_length: usize,
) -> Result<(), SnapshotV2MemoryHotplugStateEncodeError> {
    match transport {
        SnapshotV2DeviceTransport::Mmio(state) => encode_mmio(output, state, section_length),
        SnapshotV2DeviceTransport::Pci(state) => encode_pci(output, state, section_length),
    }
}

fn encode_mmio(
    output: &mut Vec<u8>,
    state: &SnapshotV2MmioDeviceState,
    section_length: usize,
) -> Result<(), SnapshotV2MemoryHotplugStateEncodeError> {
    let start = output.len();
    write_u32(output, state.device_feature_select());
    write_u32(output, state.driver_feature_select());
    write_u32(output, state.queue_select());
    write_u32(output, state.interrupt_line().raw_value());
    write_u64(output, state.region().id().raw_value());
    write_u64(output, state.region().range().start().raw_value());
    write_u64(output, state.region().range().size());
    write_zeroes(output, 8);
    pad_section(output, start, section_length)
}

fn encode_pci(
    output: &mut Vec<u8>,
    state: &SnapshotV2PciDeviceState,
    section_length: usize,
) -> Result<(), SnapshotV2MemoryHotplugStateEncodeError> {
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
    for count in [
        state.writable_bytes().len(),
        state.bar_probes().len(),
        state.msix().entries().len(),
        state.msix().pending_words().len(),
        state.msix().queue_vectors().len(),
    ] {
        write_u16(
            output,
            u16::try_from(count)
                .map_err(|_| SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?,
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
    pad_section(output, start, section_length)
}

fn pad_section(
    output: &mut Vec<u8>,
    start: usize,
    section_length: usize,
) -> Result<(), SnapshotV2MemoryHotplugStateEncodeError> {
    let target = start
        .checked_add(section_length)
        .ok_or(SnapshotV2MemoryHotplugStateEncodeError::LengthOverflow)?;
    if output.len() > target {
        return Err(SnapshotV2MemoryHotplugStateEncodeError::InvalidState(
            SnapshotV2MemoryHotplugStateBuildError::Transport,
        ));
    }
    output.resize(target, 0);
    Ok(())
}

fn preflight(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<Preflight, SnapshotV2MemoryHotplugStateDecodeError> {
    if version != NATIVE_V2_MEMORY_HOTPLUG_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::UnsupportedVersion);
    }
    if bytes.len() > NATIVE_V2_MEMORY_HOTPLUG_STATE_MAX_BYTES {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::TooLarge);
    }
    if bytes.len() < PAYLOAD_OFFSET {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::Truncated);
    }
    if read_array_at::<8>(bytes, HEADER_MAGIC_OFFSET)? != MAGIC {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidMagic);
    }
    if read_u16_at(bytes, HEADER_BYTES_OFFSET)?
        != u16::try_from(NATIVE_V2_MEMORY_HOTPLUG_STATE_HEADER_BYTES)
            .map_err(|_| SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?
        || read_u16_at(bytes, HEADER_SECTION_COUNT_OFFSET)? != SECTION_COUNT
        || read_u32_at(bytes, HEADER_FLAGS_OFFSET)? != FLAGS
        || read_u32_at(bytes, HEADER_RESERVED_OFFSET)? != 0
        || read_usize_u64_at(bytes, HEADER_DIRECTORY_OFFSET_OFFSET)? != DIRECTORY_OFFSET
        || read_usize_u64_at(bytes, HEADER_PAYLOAD_OFFSET_OFFSET)? != PAYLOAD_OFFSET
    {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
    }
    if read_u16_at(bytes, HEADER_PROFILE_OFFSET)? != PROFILE {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::UnsupportedProfile);
    }
    let transport = match read_u16_at(bytes, HEADER_TRANSPORT_OFFSET)? {
        TRANSPORT_MMIO => SnapshotV2DeviceTransportKind::Mmio,
        TRANSPORT_PCI => SnapshotV2DeviceTransportKind::Pci,
        _ => return Err(SnapshotV2MemoryHotplugStateDecodeError::UnsupportedProfile),
    };
    require_zeroes(slice_at(
        bytes,
        HEADER_RESERVED_TAIL_OFFSET,
        HEADER_RESERVED_TAIL_BYTES,
    )?)?;
    let total_length = read_usize_u64_at(bytes, HEADER_TOTAL_LENGTH_OFFSET)?;
    if total_length != bytes.len() {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
    }

    let mut expected_offset = PAYLOAD_OFFSET;
    let mut sections = [SectionBounds {
        offset: 0,
        length: 0,
    }; SECTION_COUNT_USIZE];
    for (index, expected_kind) in [
        SECTION_LOCAL,
        SECTION_COMMON,
        SECTION_BITMAP,
        SECTION_TRANSPORT,
    ]
    .into_iter()
    .enumerate()
    {
        let directory_offset = DIRECTORY_OFFSET
            .checked_add(
                index
                    .checked_mul(NATIVE_V2_MEMORY_HOTPLUG_STATE_SECTION_ENTRY_BYTES)
                    .ok_or(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?,
            )
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?;
        let entry = slice_at(
            bytes,
            directory_offset,
            NATIVE_V2_MEMORY_HOTPLUG_STATE_SECTION_ENTRY_BYTES,
        )?;
        if read_u16_at(entry, DIRECTORY_KIND_OFFSET)? != expected_kind
            || read_u16_at(entry, DIRECTORY_FLAGS_OFFSET)? != 0
            || read_u32_at(entry, DIRECTORY_RESERVED_OFFSET)? != 0
            || read_u64_at(entry, DIRECTORY_RESERVED_TAIL_OFFSET)? != 0
        {
            return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
        }
        let offset = read_usize_u64_at(entry, DIRECTORY_PAYLOAD_OFFSET)?;
        let length = read_usize_u64_at(entry, DIRECTORY_LENGTH_OFFSET)?;
        if offset != expected_offset
            || !offset.is_multiple_of(ALIGNMENT)
            || length == 0
            || !length.is_multiple_of(ALIGNMENT)
        {
            return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
        }
        expected_offset = expected_offset
            .checked_add(length)
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?;
        let section = sections
            .get_mut(index)
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?;
        *section = SectionBounds { offset, length };
    }
    let [local_bounds, common, bitmap, transport_section] = sections;
    if expected_offset != total_length
        || local_bounds.length != NATIVE_V2_MEMORY_HOTPLUG_STATE_LOCAL_BYTES
        || !(COMMON_FIXED_BYTES + COMMON_QUEUE_BYTES..=COMMON_MAX_BYTES).contains(&common.length)
        || bitmap.length > NATIVE_V2_MEMORY_HOTPLUG_MAX_BITMAP_BYTES
        || transport_section.length
            != match transport {
                SnapshotV2DeviceTransportKind::Mmio => MMIO_SECTION_BYTES,
                SnapshotV2DeviceTransportKind::Pci => PCI_SECTION_BYTES,
            }
    {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
    }

    let local = decode_local(section_bytes(bytes, local_bounds)?)?;
    let block_count = validate_local_relationship(local.config, local.config_space)
        .map_err(SnapshotV2MemoryHotplugStateDecodeError::InvalidState)?;
    let bitmap_raw_length = block_count.div_ceil(8);
    if align_up(bitmap_raw_length) != Some(bitmap.length) {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
    }
    let bitmap_bytes = section_bytes(bytes, bitmap)?;
    let raw_bitmap = bitmap_bytes
        .get(..bitmap_raw_length)
        .ok_or(SnapshotV2MemoryHotplugStateDecodeError::Truncated)?;
    require_zeroes(
        bitmap_bytes
            .get(bitmap_raw_length..)
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::Truncated)?,
    )?;
    validate_bitmap(
        raw_bitmap,
        block_count,
        local.config_space.usable_region_size(),
        local.config_space.block_size(),
        local.config_space.plugged_size(),
    )
    .map_err(SnapshotV2MemoryHotplugStateDecodeError::InvalidState)?;
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
        bitmap_raw_length,
        common,
        bitmap,
        transport_section,
    })
}

fn decode_local(bytes: &[u8]) -> Result<LocalState, SnapshotV2MemoryHotplugStateDecodeError> {
    if bytes.len() != NATIVE_V2_MEMORY_HOTPLUG_STATE_LOCAL_BYTES {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    let total_size_mib = reader.read_u64()?;
    let block_size_mib = reader.read_u64()?;
    let slot_size_mib = reader.read_u64()?;
    let config_bytes = reader.read_array::<VIRTIO_MEM_CONFIG_SPACE_SIZE>()?;
    require_zeroes(
        config_bytes
            .get(10..16)
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::Truncated)?,
    )?;
    let config_space = VirtioMemConfigSpace::from_le_bytes(config_bytes);
    let active = reader.read_bool()?;
    reader.read_zeroes(3)?;
    let next_available = reader.read_u16()?;
    let next_used = reader.read_u16()?;
    reader.read_zeroes(8)?;
    reader.finish_exact()?;

    let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(
        total_size_mib,
        block_size_mib,
        slot_size_mib,
    ))
    .map_err(|_| SnapshotV2MemoryHotplugStateDecodeError::InvalidValue)?;
    let active_queue = match (active, next_available, next_used) {
        (false, 0, 0) => None,
        (false, _, _) => return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue),
        (true, next_available, next_used) => Some(SnapshotV2MemoryHotplugQueueState::from_parts(
            next_available,
            next_used,
        )),
    };
    Ok(LocalState {
        config,
        config_space,
        active_queue,
    })
}

fn preflight_common(bytes: &[u8]) -> Result<(), SnapshotV2MemoryHotplugStateDecodeError> {
    if bytes.len() < COMMON_FIXED_BYTES + COMMON_QUEUE_BYTES
        || bytes.len() > COMMON_MAX_BYTES
        || !bytes.len().is_multiple_of(ALIGNMENT)
    {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
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
    if queue_count != 1 || notification_count > 1 || intent_count > 2 {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue);
    }
    let semantic_length = COMMON_FIXED_BYTES
        .checked_add(
            queue_count
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?,
        )
        .and_then(|length| length.checked_add(notification_count.checked_mul(2)?))
        .and_then(|length| length.checked_add(intent_count.checked_mul(4)?))
        .ok_or(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?;
    if align_up(semantic_length) != Some(bytes.len()) {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
    }
    reader.read_u16()?;
    reader.read_u16()?;
    reader.read_bool()?;
    reader.read_zeroes(3)?;
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_u64()?;
    for _ in 0..notification_count {
        reader.read_u16()?;
    }
    for _ in 0..intent_count {
        let tag = reader.read_u8()?;
        reader.read_zeroes(1)?;
        let queue_index = reader.read_u16()?;
        if !matches!(
            (tag, queue_index),
            (INTERRUPT_QUEUE, _) | (INTERRUPT_CONFIGURATION, 0)
        ) {
            return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue);
        }
    }
    reader.finish_padded()
}

fn decode_common<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2VirtioState, SnapshotV2MemoryHotplugStateDecodeError> {
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
        .map_err(|()| SnapshotV2MemoryHotplugStateDecodeError::Allocation)?;
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
        .map_err(|()| SnapshotV2MemoryHotplugStateDecodeError::Allocation)?;
    for _ in 0..notification_count {
        pending_notifications.push(reader.read_u16()?);
    }
    let mut interrupt_intents = Vec::new();
    reserve
        .reserve_vec(&mut interrupt_intents, intent_count)
        .map_err(|()| SnapshotV2MemoryHotplugStateDecodeError::Allocation)?;
    for _ in 0..intent_count {
        let tag = reader.read_u8()?;
        reader.read_zeroes(1)?;
        let queue_index = reader.read_u16()?;
        interrupt_intents.push(match tag {
            INTERRUPT_QUEUE => SnapshotV2InterruptIntent::Queue { queue_index },
            INTERRUPT_CONFIGURATION if queue_index == 0 => SnapshotV2InterruptIntent::Configuration,
            _ => return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue),
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

fn preflight_mmio(bytes: &[u8]) -> Result<(), SnapshotV2MemoryHotplugStateDecodeError> {
    if bytes.len() != MMIO_SECTION_BYTES {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
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

fn decode_mmio(
    bytes: &[u8],
) -> Result<SnapshotV2MmioDeviceState, SnapshotV2MemoryHotplugStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let device_feature_select = reader.read_u32()?;
    let driver_feature_select = reader.read_u32()?;
    let queue_select = reader.read_u32()?;
    let interrupt_line = GuestInterruptLine::new(reader.read_u32()?)
        .map_err(|_| SnapshotV2MemoryHotplugStateDecodeError::InvalidValue)?;
    let region_id = MmioRegionId::new(reader.read_u64()?);
    let region_start = GuestAddress::new(reader.read_u64()?);
    let region_size = reader.read_u64()?;
    reader.read_zeroes(8)?;
    reader.finish_exact()?;
    let region = MmioRegion::new(region_id, region_start, region_size)
        .map_err(|_| SnapshotV2MemoryHotplugStateDecodeError::InvalidValue)?;
    Ok(SnapshotV2MmioDeviceState::from_parts(
        device_feature_select,
        driver_feature_select,
        queue_select,
        region,
        interrupt_line,
    ))
}

fn preflight_pci(bytes: &[u8]) -> Result<(), SnapshotV2MemoryHotplugStateDecodeError> {
    if bytes.len() != PCI_SECTION_BYTES {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    if reader.read_u8()? != PCI_PHASE_ACTIVE || reader.read_u8()? != PCI_ORIGIN_STARTUP {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue);
    }
    reader.read_u8()?;
    if reader.read_u8()? != PCI_BAR_MEMORY64 || reader.read_u8()? != PCI_BAR_NOT_PREFETCHABLE {
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue);
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
        return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue);
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
        reader.read_bytes(16)?;
    }
    for _ in 0..PCI_PENDING_WORD_COUNT {
        reader.read_u64()?;
    }
    for _ in 0..PCI_QUEUE_VECTOR_COUNT {
        reader.read_u16()?;
    }
    reader.finish_padded()
}

fn decode_pci<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2PciDeviceState, SnapshotV2MemoryHotplugStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let phase = match reader.read_u8()? {
        PCI_PHASE_ACTIVE => VirtioPciEndpointPhase::Active,
        _ => return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue),
    };
    let origin = match reader.read_u8()? {
        PCI_ORIGIN_STARTUP => StorageDeviceOrigin::Startup,
        _ => return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue),
    };
    let bar_index = reader.read_u8()?;
    let bar_address_space = match reader.read_u8()? {
        PCI_BAR_MEMORY64 => PciBarAddressSpace::Memory64,
        _ => return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue),
    };
    let bar_prefetchable = match reader.read_u8()? {
        PCI_BAR_NOT_PREFETCHABLE => PciBarPrefetchable::No,
        _ => return Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue),
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
        .map_err(|()| SnapshotV2MemoryHotplugStateDecodeError::Allocation)?;
    for _ in 0..writable_count {
        let offset = reader.read_u16()?;
        let value = reader.read_u8()?;
        reader.read_zeroes(1)?;
        writable_bytes.push(SnapshotV2PciWritableByte::from_parts(offset, value));
    }
    let mut bar_probes = Vec::new();
    reserve
        .reserve_vec(&mut bar_probes, probe_count)
        .map_err(|()| SnapshotV2MemoryHotplugStateDecodeError::Allocation)?;
    for _ in 0..probe_count {
        let index = reader.read_u8()?;
        let pending = reader.read_bool()?;
        reader.read_zeroes(2)?;
        bar_probes.push(SnapshotV2PciBarProbeState::from_parts(index, pending));
    }
    let mut entries = Vec::new();
    reserve
        .reserve_vec(&mut entries, entry_count)
        .map_err(|()| SnapshotV2MemoryHotplugStateDecodeError::Allocation)?;
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
        .map_err(|()| SnapshotV2MemoryHotplugStateDecodeError::Allocation)?;
    for _ in 0..pending_count {
        pending_words.push(reader.read_u64()?);
    }
    let mut queue_vectors = Vec::new();
    reserve
        .reserve_vec(&mut queue_vectors, vector_count)
        .map_err(|()| SnapshotV2MemoryHotplugStateDecodeError::Allocation)?;
    for _ in 0..vector_count {
        queue_vectors.push(reader.read_u16()?);
    }
    reader.finish_padded()?;

    let sbdf = PciSbdf::new(segment, bus, device, function)
        .map_err(|_| SnapshotV2MemoryHotplugStateDecodeError::InvalidValue)?;
    let bar_range = GuestMemoryRange::new(bar_start, bar_size)
        .map_err(|_| SnapshotV2MemoryHotplugStateDecodeError::InvalidValue)?;
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

    fn read_u8(&mut self) -> Result<u8, SnapshotV2MemoryHotplugStateDecodeError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::Truncated)?;
        self.position = self
            .position
            .checked_add(1)
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?;
        Ok(value)
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotV2MemoryHotplugStateDecodeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidValue),
        }
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotV2MemoryHotplugStateDecodeError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotV2MemoryHotplugStateDecodeError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotV2MemoryHotplugStateDecodeError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_array<const LENGTH: usize>(
        &mut self,
    ) -> Result<[u8; LENGTH], SnapshotV2MemoryHotplugStateDecodeError> {
        self.read_bytes(LENGTH)?
            .try_into()
            .map_err(|_| SnapshotV2MemoryHotplugStateDecodeError::Truncated)
    }

    fn read_bytes(
        &mut self,
        length: usize,
    ) -> Result<&'a [u8], SnapshotV2MemoryHotplugStateDecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(SnapshotV2MemoryHotplugStateDecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn read_zeroes(
        &mut self,
        length: usize,
    ) -> Result<(), SnapshotV2MemoryHotplugStateDecodeError> {
        require_zeroes(self.read_bytes(length)?)
    }

    fn finish_exact(self) -> Result<(), SnapshotV2MemoryHotplugStateDecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)
        }
    }

    fn finish_padded(self) -> Result<(), SnapshotV2MemoryHotplugStateDecodeError> {
        require_zeroes(
            self.bytes
                .get(self.position..)
                .ok_or(SnapshotV2MemoryHotplugStateDecodeError::Truncated)?,
        )
    }
}

fn section_bytes(
    bytes: &[u8],
    bounds: SectionBounds,
) -> Result<&[u8], SnapshotV2MemoryHotplugStateDecodeError> {
    slice_at(bytes, bounds.offset, bounds.length)
}

fn slice_at(
    bytes: &[u8],
    offset: usize,
    length: usize,
) -> Result<&[u8], SnapshotV2MemoryHotplugStateDecodeError> {
    let end = offset
        .checked_add(length)
        .ok_or(SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)?;
    bytes
        .get(offset..end)
        .ok_or(SnapshotV2MemoryHotplugStateDecodeError::Truncated)
}

fn read_u16_at(
    bytes: &[u8],
    offset: usize,
) -> Result<u16, SnapshotV2MemoryHotplugStateDecodeError> {
    Ok(u16::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u32_at(
    bytes: &[u8],
    offset: usize,
) -> Result<u32, SnapshotV2MemoryHotplugStateDecodeError> {
    Ok(u32::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u64_at(
    bytes: &[u8],
    offset: usize,
) -> Result<u64, SnapshotV2MemoryHotplugStateDecodeError> {
    Ok(u64::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_usize_u64_at(
    bytes: &[u8],
    offset: usize,
) -> Result<usize, SnapshotV2MemoryHotplugStateDecodeError> {
    usize::try_from(read_u64_at(bytes, offset)?)
        .map_err(|_| SnapshotV2MemoryHotplugStateDecodeError::InvalidStructure)
}

fn read_array_at<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotV2MemoryHotplugStateDecodeError> {
    slice_at(bytes, offset, LENGTH)?
        .try_into()
        .map_err(|_| SnapshotV2MemoryHotplugStateDecodeError::Truncated)
}

fn require_zeroes(bytes: &[u8]) -> Result<(), SnapshotV2MemoryHotplugStateDecodeError> {
    if bytes.iter().any(|byte| *byte != 0) {
        Err(SnapshotV2MemoryHotplugStateDecodeError::NonzeroReserved)
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
