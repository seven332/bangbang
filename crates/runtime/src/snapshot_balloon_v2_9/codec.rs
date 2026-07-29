use super::*;
use crate::balloon::BalloonConfigInput;
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

const MAGIC: [u8; 8] = *b"BANGBL2\0";
const PROFILE: u16 = 1;
const FLAGS: u32 = 0;
const SECTION_COUNT: u16 = 4;
const SECTION_COUNT_USIZE: usize = 4;
const ALIGNMENT: usize = 8;
const DIRECTORY_OFFSET: usize = NATIVE_V2_BALLOON_STATE_HEADER_BYTES;
const PAYLOAD_OFFSET: usize =
    DIRECTORY_OFFSET + NATIVE_V2_BALLOON_STATE_SECTION_ENTRY_BYTES * SECTION_COUNT_USIZE;

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
const SECTION_ACCOUNTING: u16 = 3;
const SECTION_TRANSPORT: u16 = 4;
const TRANSPORT_MMIO: u16 = 1;
const TRANSPORT_PCI: u16 = 2;

const CONFIG_DEFLATE_ON_OOM: u16 = 1 << 0;
const CONFIG_FREE_PAGE_HINTING: u16 = 1 << 1;
const CONFIG_FREE_PAGE_REPORTING: u16 = 1 << 2;
const CONFIG_KNOWN_FLAGS: u16 =
    CONFIG_DEFLATE_ON_OOM | CONFIG_FREE_PAGE_HINTING | CONFIG_FREE_PAGE_REPORTING;
const LOCAL_PENDING_DESCRIPTOR: u16 = 1 << 0;
const LOCAL_KNOWN_FLAGS: u16 = LOCAL_PENDING_DESCRIPTOR;
const HINT_GUEST_PRESENT: u16 = 1 << 0;
const HINT_ACKNOWLEDGE_ON_STOP: u16 = 1 << 1;
const HINT_KNOWN_FLAGS: u16 = HINT_GUEST_PRESENT | HINT_ACKNOWLEDGE_ON_STOP;
const ACTIVE_QUEUE_KNOWN_MASK: u8 = 0b1_1111;
const LOCAL_CURSOR_COUNT: usize = 5;
const LOCAL_PREFIX_BYTES: usize = 48;
const LOCAL_CURSOR_BYTES: usize = 4;
const LOCAL_CURSOR_AREA_BYTES: usize = LOCAL_CURSOR_COUNT * LOCAL_CURSOR_BYTES;
const LOCAL_CURSOR_PADDING_BYTES: usize = 4;
const LOCAL_STAT_VALUE_BYTES: usize = 8;
const LOCAL_STAT_AREA_BYTES: usize = NATIVE_V2_BALLOON_STATISTIC_COUNT * LOCAL_STAT_VALUE_BYTES;
const LOCAL_RESERVED_TAIL_BYTES: usize = NATIVE_V2_BALLOON_STATE_LOCAL_BYTES
    - LOCAL_PREFIX_BYTES
    - LOCAL_CURSOR_AREA_BYTES
    - LOCAL_CURSOR_PADDING_BYTES
    - LOCAL_STAT_AREA_BYTES;
const _: () = assert!(LOCAL_RESERVED_TAIL_BYTES == 56);

const COMMON_FIXED_BYTES: usize = 32;
const COMMON_QUEUE_BYTES: usize = 32;
const COMMON_MAX_BYTES: usize = 232;
const INTERRUPT_QUEUE: u8 = 1;
const INTERRUPT_CONFIGURATION: u8 = 2;

const ACCOUNTING_PREFIX_BYTES: usize = 16;
const ACCOUNTING_RANGE_BYTES: usize = 8;
const ACCOUNTING_MAX_BYTES: usize = ACCOUNTING_PREFIX_BYTES
    + NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES * ACCOUNTING_RANGE_BYTES;

const MMIO_SECTION_BYTES: usize = 48;
const PCI_FIXED_BYTES: usize = 72;
const PCI_PHASE_ACTIVE: u8 = 1;
const PCI_ORIGIN_STARTUP: u8 = 1;
const PCI_BAR_MEMORY64: u8 = 2;
const PCI_BAR_NOT_PREFETCHABLE: u8 = 0;
const PCI_WRITABLE_COUNT: usize = 4;
const PCI_PROBE_COUNT: usize = 2;
const PCI_PENDING_WORD_COUNT: usize = 1;
const PCI_WRITABLE_BYTES: usize = PCI_WRITABLE_COUNT * 4;
const PCI_PROBE_BYTES: usize = PCI_PROBE_COUNT * 4;
const PCI_MSIX_ENTRY_BYTES: usize = 16;
const PCI_PENDING_WORD_BYTES: usize = 8;
const PCI_QUEUE_VECTOR_BYTES: usize = 2;
const PCI_MAX_QUEUE_COUNT: usize = 5;
const PCI_MAX_BYTES: usize = 216;
const PROFILE_MAX_BYTES: usize = PAYLOAD_OFFSET
    + NATIVE_V2_BALLOON_STATE_LOCAL_BYTES
    + COMMON_MAX_BYTES
    + ACCOUNTING_MAX_BYTES
    + PCI_MAX_BYTES;
const _: () = assert!(PROFILE_MAX_BYTES == 2_098_064);
const _: () = assert!(PROFILE_MAX_BYTES < NATIVE_V2_BALLOON_STATE_MAX_BYTES);

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
    accounting_length: usize,
    transport_length: usize,
    total_length: usize,
}

struct Preflight {
    transport: SnapshotV2DeviceTransportKind,
    local: SectionBounds,
    common: SectionBounds,
    accounting: SectionBounds,
    transport_section: SectionBounds,
}

struct LocalState {
    config: BalloonConfig,
    config_space: VirtioBalloonConfigSpace,
    continuation: SnapshotV2BalloonContinuationState,
}

pub(super) fn encode(
    version: SnapshotFormatVersion,
    state: &SnapshotV2BalloonState,
) -> Result<Vec<u8>, SnapshotV2BalloonStateEncodeError> {
    encode_with_policy(version, state, &mut FallibleReserve)
}

pub(super) fn decode(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<SnapshotV2BalloonState, SnapshotV2BalloonStateDecodeError> {
    decode_with_policy(version, bytes, &mut FallibleReserve)
}

pub(super) fn encode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    state: &SnapshotV2BalloonState,
    reserve: &mut R,
) -> Result<Vec<u8>, SnapshotV2BalloonStateEncodeError> {
    if version != NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2BalloonStateEncodeError::UnsupportedVersion);
    }
    validate_balloon_state(state).map_err(SnapshotV2BalloonStateEncodeError::InvalidState)?;
    let layout = calculate_layout(state)?;
    let mut output = Vec::new();
    reserve
        .reserve_vec(&mut output, layout.total_length)
        .map_err(|()| SnapshotV2BalloonStateEncodeError::Allocation)?;

    write_header(&mut output, state, &layout)?;
    let local_offset = PAYLOAD_OFFSET;
    let common_offset = local_offset
        .checked_add(NATIVE_V2_BALLOON_STATE_LOCAL_BYTES)
        .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    let accounting_offset = common_offset
        .checked_add(layout.common_length)
        .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    let transport_offset = accounting_offset
        .checked_add(layout.accounting_length)
        .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    write_directory_entry(
        &mut output,
        SECTION_LOCAL,
        local_offset,
        NATIVE_V2_BALLOON_STATE_LOCAL_BYTES,
    )?;
    write_directory_entry(
        &mut output,
        SECTION_COMMON,
        common_offset,
        layout.common_length,
    )?;
    write_directory_entry(
        &mut output,
        SECTION_ACCOUNTING,
        accounting_offset,
        layout.accounting_length,
    )?;
    write_directory_entry(
        &mut output,
        SECTION_TRANSPORT,
        transport_offset,
        layout.transport_length,
    )?;
    encode_local(&mut output, state);
    encode_common(&mut output, state.virtio(), layout.common_length)?;
    encode_accounting(&mut output, state.accounting())?;
    encode_transport(&mut output, state.transport(), layout.transport_length)?;
    if output.len() != layout.total_length {
        return Err(SnapshotV2BalloonStateEncodeError::InvalidState(
            SnapshotV2BalloonStateBuildError::Transport,
        ));
    }
    Ok(output)
}

pub(super) fn decode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2BalloonState, SnapshotV2BalloonStateDecodeError> {
    let preflight = preflight(version, bytes)?;
    let local = decode_local(section_bytes(bytes, preflight.local)?)?;
    let virtio = decode_common(section_bytes(bytes, preflight.common)?, reserve)?;
    let accounting = decode_accounting(section_bytes(bytes, preflight.accounting)?, reserve)?;
    let transport_bytes = section_bytes(bytes, preflight.transport_section)?;
    let transport = match preflight.transport {
        SnapshotV2DeviceTransportKind::Mmio => {
            SnapshotV2DeviceTransport::Mmio(decode_mmio(transport_bytes)?)
        }
        SnapshotV2DeviceTransportKind::Pci => {
            SnapshotV2DeviceTransport::Pci(decode_pci(transport_bytes, reserve)?)
        }
    };
    SnapshotV2BalloonState::try_new(
        local.config,
        local.config_space,
        local.continuation,
        accounting,
        virtio,
        transport,
    )
    .map_err(SnapshotV2BalloonStateDecodeError::InvalidState)
}

fn calculate_layout(
    state: &SnapshotV2BalloonState,
) -> Result<Layout, SnapshotV2BalloonStateEncodeError> {
    let common_semantic = COMMON_FIXED_BYTES
        .checked_add(
            state
                .virtio()
                .queues()
                .len()
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
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
        .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    let common_length =
        align_up(common_semantic).ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    let accounting_length = ACCOUNTING_PREFIX_BYTES
        .checked_add(
            state
                .accounting()
                .ranges()
                .len()
                .checked_mul(ACCOUNTING_RANGE_BYTES)
                .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    let transport_length = match state.transport() {
        SnapshotV2DeviceTransport::Mmio(_) => MMIO_SECTION_BYTES,
        SnapshotV2DeviceTransport::Pci(pci) => calculate_pci_length(pci)?,
    };
    let total_length = PAYLOAD_OFFSET
        .checked_add(NATIVE_V2_BALLOON_STATE_LOCAL_BYTES)
        .and_then(|length| length.checked_add(common_length))
        .and_then(|length| length.checked_add(accounting_length))
        .and_then(|length| length.checked_add(transport_length))
        .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    if total_length > NATIVE_V2_BALLOON_STATE_MAX_BYTES {
        return Err(SnapshotV2BalloonStateEncodeError::TooLarge);
    }
    Ok(Layout {
        common_length,
        accounting_length,
        transport_length,
        total_length,
    })
}

fn calculate_pci_length(
    pci: &SnapshotV2PciDeviceState,
) -> Result<usize, SnapshotV2BalloonStateEncodeError> {
    let semantic = PCI_FIXED_BYTES
        .checked_add(PCI_WRITABLE_BYTES)
        .and_then(|length| length.checked_add(PCI_PROBE_BYTES))
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
        .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    align_up(semantic).ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)
}

fn write_header(
    output: &mut Vec<u8>,
    state: &SnapshotV2BalloonState,
    layout: &Layout,
) -> Result<(), SnapshotV2BalloonStateEncodeError> {
    write_bytes(output, &MAGIC);
    write_u16(output, NATIVE_V2_BALLOON_STATE_HEADER_BYTES as u16);
    write_u16(output, PROFILE);
    write_u16(output, transport_tag(state.transport().kind()));
    write_u16(output, SECTION_COUNT);
    write_u32(output, FLAGS);
    write_u32(output, 0);
    write_u64(
        output,
        u64::try_from(layout.total_length)
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(DIRECTORY_OFFSET)
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(PAYLOAD_OFFSET)
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_zeroes(output, HEADER_RESERVED_TAIL_BYTES);
    Ok(())
}

fn write_directory_entry(
    output: &mut Vec<u8>,
    kind: u16,
    offset: usize,
    length: usize,
) -> Result<(), SnapshotV2BalloonStateEncodeError> {
    write_u16(output, kind);
    write_u16(output, 0);
    write_u32(output, 0);
    write_u64(
        output,
        u64::try_from(offset).map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u64(
        output,
        u64::try_from(length).map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u64(output, 0);
    Ok(())
}

fn encode_local(output: &mut Vec<u8>, state: &SnapshotV2BalloonState) {
    let config = state.config();
    let continuation = state.continuation();
    let mut config_flags = 0;
    set_flag(
        &mut config_flags,
        CONFIG_DEFLATE_ON_OOM,
        config.deflate_on_oom(),
    );
    set_flag(
        &mut config_flags,
        CONFIG_FREE_PAGE_HINTING,
        config.free_page_hinting(),
    );
    set_flag(
        &mut config_flags,
        CONFIG_FREE_PAGE_REPORTING,
        config.free_page_reporting(),
    );
    let mut local_flags = 0;
    set_flag(
        &mut local_flags,
        LOCAL_PENDING_DESCRIPTOR,
        continuation.statistics_pending_descriptor_head().is_some(),
    );
    let mut statistic_flags = 0_u16;
    for (index, value) in continuation.statistics().values().iter().enumerate() {
        if value.is_some() {
            statistic_flags |= 1 << index;
        }
    }
    let layout = VirtioBalloonQueueLayout::from_config(config);
    let queue_count = layout.queue_count();
    let active_mask = if continuation.active_queues().is_some() {
        ((1_u16 << queue_count) - 1) as u8
    } else {
        0
    };
    let hinting = continuation.hinting();
    let mut hint_flags = 0;
    set_flag(
        &mut hint_flags,
        HINT_GUEST_PRESENT,
        hinting.guest_cmd().is_some(),
    );
    set_flag(
        &mut hint_flags,
        HINT_ACKNOWLEDGE_ON_STOP,
        hinting.acknowledge_on_stop(),
    );

    write_u32(output, config.amount_mib());
    write_u16(output, config_flags);
    write_u16(output, config.stats_polling_interval_s());
    write_u32(output, state.config_space().num_pages());
    write_u32(output, state.config_space().actual_pages());
    write_u32(output, state.config_space().free_page_hint_cmd_id());
    write_u16(output, continuation.stats_polling_interval_s());
    write_u16(output, local_flags);
    write_u16(output, statistic_flags);
    write_u8(output, active_mask);
    write_u8(output, 0);
    write_u16(
        output,
        continuation
            .statistics_pending_descriptor_head()
            .unwrap_or(0),
    );
    write_u16(output, 0);
    write_u32(output, hinting.host_cmd());
    write_u32(output, hinting.guest_cmd().unwrap_or(0));
    write_u32(output, hinting.last_cmd());
    write_u16(output, hint_flags);
    write_u16(output, 0);
    for index in 0..LOCAL_CURSOR_COUNT {
        let cursor = continuation
            .active_queues()
            .and_then(|active| active.cursor_for_layout(layout, index));
        write_u16(
            output,
            cursor.map_or(0, SnapshotV2BalloonQueueState::next_available),
        );
        write_u16(
            output,
            cursor.map_or(0, SnapshotV2BalloonQueueState::next_used),
        );
    }
    write_zeroes(output, LOCAL_CURSOR_PADDING_BYTES);
    for value in continuation.statistics().values() {
        write_u64(output, value.unwrap_or(0));
    }
    write_zeroes(output, LOCAL_RESERVED_TAIL_BYTES);
}

fn encode_common(
    output: &mut Vec<u8>,
    state: &SnapshotV2VirtioState,
    section_length: usize,
) -> Result<(), SnapshotV2BalloonStateEncodeError> {
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
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.pending_notifications().len())
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.interrupt_intents().len())
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
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

fn encode_accounting(
    output: &mut Vec<u8>,
    state: &SnapshotV2BalloonAccountingState,
) -> Result<(), SnapshotV2BalloonStateEncodeError> {
    write_u32(
        output,
        u32::try_from(state.ranges().len())
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u32(output, 0);
    write_u64(output, state.inflated_page_count());
    for range in state.ranges() {
        write_u32(output, range.start_pfn());
        write_u32(output, range.page_count());
    }
    Ok(())
}

fn encode_transport(
    output: &mut Vec<u8>,
    state: &SnapshotV2DeviceTransport,
    section_length: usize,
) -> Result<(), SnapshotV2BalloonStateEncodeError> {
    match state {
        SnapshotV2DeviceTransport::Mmio(mmio) => encode_mmio(output, mmio, section_length),
        SnapshotV2DeviceTransport::Pci(pci) => encode_pci(output, pci, section_length),
    }
}

fn encode_mmio(
    output: &mut Vec<u8>,
    state: &SnapshotV2MmioDeviceState,
    section_length: usize,
) -> Result<(), SnapshotV2BalloonStateEncodeError> {
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
) -> Result<(), SnapshotV2BalloonStateEncodeError> {
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
    write_u16(
        output,
        u16::try_from(state.writable_bytes().len())
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.bar_probes().len())
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.msix().entries().len())
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.msix().pending_words().len())
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
    write_u16(
        output,
        u16::try_from(state.msix().queue_vectors().len())
            .map_err(|_| SnapshotV2BalloonStateEncodeError::LengthOverflow)?,
    );
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
) -> Result<Preflight, SnapshotV2BalloonStateDecodeError> {
    if version != NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2BalloonStateDecodeError::UnsupportedVersion);
    }
    if bytes.len() > NATIVE_V2_BALLOON_STATE_MAX_BYTES {
        return Err(SnapshotV2BalloonStateDecodeError::TooLarge);
    }
    if bytes.len() < PAYLOAD_OFFSET {
        return Err(SnapshotV2BalloonStateDecodeError::Truncated);
    }
    if read_array_at::<8>(bytes, HEADER_MAGIC_OFFSET)? != MAGIC {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidMagic);
    }
    if usize::from(read_u16_at(bytes, HEADER_BYTES_OFFSET)?) != NATIVE_V2_BALLOON_STATE_HEADER_BYTES
        || read_u16_at(bytes, HEADER_PROFILE_OFFSET)? != PROFILE
    {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidProfile);
    }
    let transport = match read_u16_at(bytes, HEADER_TRANSPORT_OFFSET)? {
        TRANSPORT_MMIO => SnapshotV2DeviceTransportKind::Mmio,
        TRANSPORT_PCI => SnapshotV2DeviceTransportKind::Pci,
        _ => return Err(SnapshotV2BalloonStateDecodeError::InvalidTransport),
    };
    if read_u16_at(bytes, HEADER_SECTION_COUNT_OFFSET)? != SECTION_COUNT
        || read_u32_at(bytes, HEADER_FLAGS_OFFSET)? != FLAGS
        || read_u32_at(bytes, HEADER_RESERVED_OFFSET)? != 0
        || read_usize_u64_at(bytes, HEADER_TOTAL_LENGTH_OFFSET)? != bytes.len()
        || read_usize_u64_at(bytes, HEADER_DIRECTORY_OFFSET_OFFSET)? != DIRECTORY_OFFSET
        || read_usize_u64_at(bytes, HEADER_PAYLOAD_OFFSET_OFFSET)? != PAYLOAD_OFFSET
    {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
    }
    require_zeroes(slice_at(
        bytes,
        HEADER_RESERVED_TAIL_OFFSET,
        HEADER_RESERVED_TAIL_BYTES,
    )?)?;

    let expected_kinds = [
        SECTION_LOCAL,
        SECTION_COMMON,
        SECTION_ACCOUNTING,
        SECTION_TRANSPORT,
    ];
    let mut sections = [SectionBounds {
        offset: 0,
        length: 0,
    }; SECTION_COUNT_USIZE];
    let mut expected_offset = PAYLOAD_OFFSET;
    for (index, kind) in expected_kinds.into_iter().enumerate() {
        let entry_offset = DIRECTORY_OFFSET
            .checked_add(
                index
                    .checked_mul(NATIVE_V2_BALLOON_STATE_SECTION_ENTRY_BYTES)
                    .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?,
            )
            .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
        if read_u16_at(bytes, entry_offset + DIRECTORY_KIND_OFFSET)? != kind
            || read_u16_at(bytes, entry_offset + DIRECTORY_FLAGS_OFFSET)? != 0
            || read_u32_at(bytes, entry_offset + DIRECTORY_RESERVED_OFFSET)? != 0
            || read_u64_at(bytes, entry_offset + DIRECTORY_RESERVED_TAIL_OFFSET)? != 0
        {
            return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
        }
        let offset = read_usize_u64_at(bytes, entry_offset + DIRECTORY_PAYLOAD_OFFSET)?;
        let length = read_usize_u64_at(bytes, entry_offset + DIRECTORY_LENGTH_OFFSET)?;
        if length == 0
            || offset != expected_offset
            || !offset.is_multiple_of(ALIGNMENT)
            || !length.is_multiple_of(ALIGNMENT)
        {
            return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
        }
        expected_offset = offset
            .checked_add(length)
            .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
        if expected_offset > bytes.len() {
            return Err(SnapshotV2BalloonStateDecodeError::Truncated);
        }
        *sections
            .get_mut(index)
            .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)? =
            SectionBounds { offset, length };
    }
    if expected_offset != bytes.len() {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
    }

    let [local, common, accounting, transport_section] = sections;
    preflight_local(section_bytes(bytes, local)?)?;
    let queue_count = preflight_common(section_bytes(bytes, common)?)?;
    preflight_accounting(section_bytes(bytes, accounting)?)?;
    match transport {
        SnapshotV2DeviceTransportKind::Mmio => {
            preflight_mmio(section_bytes(bytes, transport_section)?)?
        }
        SnapshotV2DeviceTransportKind::Pci => {
            preflight_pci(section_bytes(bytes, transport_section)?, queue_count)?
        }
    }
    Ok(Preflight {
        transport,
        local,
        common,
        accounting,
        transport_section,
    })
}

fn preflight_local(bytes: &[u8]) -> Result<(), SnapshotV2BalloonStateDecodeError> {
    if bytes.len() != NATIVE_V2_BALLOON_STATE_LOCAL_BYTES {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    reader.read_u32()?;
    let config_flags = reader.read_u16()?;
    reader.read_u16()?;
    reader.read_u32()?;
    reader.read_u32()?;
    reader.read_u32()?;
    reader.read_u16()?;
    let local_flags = reader.read_u16()?;
    let statistic_flags = reader.read_u16()?;
    let active_mask = reader.read_u8()?;
    reader.read_zeroes(1)?;
    let pending_descriptor = reader.read_u16()?;
    reader.read_zeroes(2)?;
    reader.read_u32()?;
    let guest_cmd = reader.read_u32()?;
    reader.read_u32()?;
    let hint_flags = reader.read_u16()?;
    reader.read_zeroes(2)?;
    if config_flags & !CONFIG_KNOWN_FLAGS != 0
        || local_flags & !LOCAL_KNOWN_FLAGS != 0
        || active_mask & !ACTIVE_QUEUE_KNOWN_MASK != 0
        || hint_flags & !HINT_KNOWN_FLAGS != 0
        || (!has_flag(local_flags, LOCAL_PENDING_DESCRIPTOR) && pending_descriptor != 0)
        || (!has_flag(hint_flags, HINT_GUEST_PRESENT) && guest_cmd != 0)
    {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
    }
    for index in 0..LOCAL_CURSOR_COUNT {
        let next_available = reader.read_u16()?;
        let next_used = reader.read_u16()?;
        if active_mask & (1 << index) == 0 && (next_available != 0 || next_used != 0) {
            return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
        }
    }
    reader.read_zeroes(LOCAL_CURSOR_PADDING_BYTES)?;
    for index in 0..NATIVE_V2_BALLOON_STATISTIC_COUNT {
        let value = reader.read_u64()?;
        if statistic_flags & (1 << index) == 0 && value != 0 {
            return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
        }
    }
    reader.read_zeroes(LOCAL_RESERVED_TAIL_BYTES)?;
    reader.finish_exact()
}

fn decode_local(bytes: &[u8]) -> Result<LocalState, SnapshotV2BalloonStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let amount_mib = reader.read_u32()?;
    let config_flags = reader.read_u16()?;
    let config_interval = reader.read_u16()?;
    let config_space =
        VirtioBalloonConfigSpace::new(reader.read_u32()?, reader.read_u32()?, reader.read_u32()?);
    let retained_interval = reader.read_u16()?;
    let local_flags = reader.read_u16()?;
    let statistic_flags = reader.read_u16()?;
    let active_mask = reader.read_u8()?;
    reader.read_zeroes(1)?;
    let pending_descriptor_value = reader.read_u16()?;
    reader.read_zeroes(2)?;
    let host_cmd = reader.read_u32()?;
    let guest_cmd_value = reader.read_u32()?;
    let last_cmd = reader.read_u32()?;
    let hint_flags = reader.read_u16()?;
    reader.read_zeroes(2)?;
    let mut cursor_values = [SnapshotV2BalloonQueueState::from_parts(0, 0); LOCAL_CURSOR_COUNT];
    for cursor in &mut cursor_values {
        *cursor = SnapshotV2BalloonQueueState::from_parts(reader.read_u16()?, reader.read_u16()?);
    }
    reader.read_zeroes(LOCAL_CURSOR_PADDING_BYTES)?;
    let mut statistic_values = [None; NATIVE_V2_BALLOON_STATISTIC_COUNT];
    for (index, value) in statistic_values.iter_mut().enumerate() {
        let decoded = reader.read_u64()?;
        if statistic_flags & (1 << index) != 0 {
            *value = Some(decoded);
        }
    }
    reader.read_zeroes(LOCAL_RESERVED_TAIL_BYTES)?;
    reader.finish_exact()?;

    let config = BalloonConfigInput::new(amount_mib, has_flag(config_flags, CONFIG_DEFLATE_ON_OOM))
        .with_stats_polling_interval_s(config_interval)
        .with_free_page_hinting(has_flag(config_flags, CONFIG_FREE_PAGE_HINTING))
        .with_free_page_reporting(has_flag(config_flags, CONFIG_FREE_PAGE_REPORTING))
        .validate()
        .map_err(|_| SnapshotV2BalloonStateDecodeError::InvalidValue)?;
    let layout = VirtioBalloonQueueLayout::from_config(config);
    let queue_count = layout.queue_count();
    let expected_active_mask = ((1_u16 << queue_count) - 1) as u8;
    let active_queues = if active_mask == 0 {
        None
    } else {
        if active_mask != expected_active_mask {
            return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
        }
        Some(SnapshotV2BalloonActiveQueuesState::from_parts(
            cursor_values[0],
            cursor_values[1],
            layout
                .statistics()
                .map(|queue| {
                    cursor_values
                        .get(queue.index())
                        .copied()
                        .ok_or(SnapshotV2BalloonStateDecodeError::InvalidValue)
                })
                .transpose()?,
            layout
                .free_page_hinting()
                .map(|queue| {
                    cursor_values
                        .get(queue.index())
                        .copied()
                        .ok_or(SnapshotV2BalloonStateDecodeError::InvalidValue)
                })
                .transpose()?,
            layout
                .free_page_reporting()
                .map(|queue| {
                    cursor_values
                        .get(queue.index())
                        .copied()
                        .ok_or(SnapshotV2BalloonStateDecodeError::InvalidValue)
                })
                .transpose()?,
        ))
    };
    let pending_descriptor =
        has_flag(local_flags, LOCAL_PENDING_DESCRIPTOR).then_some(pending_descriptor_value);
    let guest_cmd = has_flag(hint_flags, HINT_GUEST_PRESENT).then_some(guest_cmd_value);
    let continuation = SnapshotV2BalloonContinuationState::new(
        active_queues,
        retained_interval,
        SnapshotV2BalloonStatistics::new(statistic_values),
        pending_descriptor,
        SnapshotV2BalloonHintState::new(
            host_cmd,
            guest_cmd,
            last_cmd,
            has_flag(hint_flags, HINT_ACKNOWLEDGE_ON_STOP),
        ),
    );
    Ok(LocalState {
        config,
        config_space,
        continuation,
    })
}

fn preflight_common(bytes: &[u8]) -> Result<usize, SnapshotV2BalloonStateDecodeError> {
    if bytes.len() < COMMON_FIXED_BYTES + 2 * COMMON_QUEUE_BYTES
        || bytes.len() > COMMON_MAX_BYTES
        || !bytes.len().is_multiple_of(ALIGNMENT)
    {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
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
    if !(2..=PCI_MAX_QUEUE_COUNT).contains(&queue_count)
        || notification_count > queue_count
        || intent_count > queue_count + 1
    {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
    }
    let semantic_length = COMMON_FIXED_BYTES
        .checked_add(
            queue_count
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?,
        )
        .and_then(|length| length.checked_add(notification_count.checked_mul(2)?))
        .and_then(|length| length.checked_add(intent_count.checked_mul(4)?))
        .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
    if align_up(semantic_length) != Some(bytes.len()) {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
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
            (INTERRUPT_QUEUE, _) | (INTERRUPT_CONFIGURATION, 0)
        ) {
            return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
        }
    }
    reader.finish_padded()?;
    Ok(queue_count)
}

fn decode_common<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2VirtioState, SnapshotV2BalloonStateDecodeError> {
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
        .map_err(|()| SnapshotV2BalloonStateDecodeError::Allocation)?;
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
        .map_err(|()| SnapshotV2BalloonStateDecodeError::Allocation)?;
    for _ in 0..notification_count {
        pending_notifications.push(reader.read_u16()?);
    }
    let mut interrupt_intents = Vec::new();
    reserve
        .reserve_vec(&mut interrupt_intents, intent_count)
        .map_err(|()| SnapshotV2BalloonStateDecodeError::Allocation)?;
    for _ in 0..intent_count {
        let tag = reader.read_u8()?;
        reader.read_zeroes(1)?;
        let queue_index = reader.read_u16()?;
        interrupt_intents.push(match tag {
            INTERRUPT_QUEUE => SnapshotV2InterruptIntent::Queue { queue_index },
            INTERRUPT_CONFIGURATION if queue_index == 0 => SnapshotV2InterruptIntent::Configuration,
            _ => return Err(SnapshotV2BalloonStateDecodeError::InvalidValue),
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

fn preflight_accounting(bytes: &[u8]) -> Result<(), SnapshotV2BalloonStateDecodeError> {
    if bytes.len() < ACCOUNTING_PREFIX_BYTES || !bytes.len().is_multiple_of(ALIGNMENT) {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    let count = usize::try_from(reader.read_u32()?)
        .map_err(|_| SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
    reader.read_zeroes(4)?;
    let expected_total = reader.read_u64()?;
    if count > NATIVE_V2_BALLOON_STATE_MAX_ACCOUNTING_RANGES
        || ACCOUNTING_PREFIX_BYTES.checked_add(
            count
                .checked_mul(ACCOUNTING_RANGE_BYTES)
                .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?,
        ) != Some(bytes.len())
    {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
    }
    let mut previous_end = None;
    let mut total = 0_u64;
    for _ in 0..count {
        let start = u64::from(reader.read_u32()?);
        let page_count = reader.read_u32()?;
        let end = start
            .checked_add(u64::from(page_count))
            .ok_or(SnapshotV2BalloonStateDecodeError::InvalidValue)?;
        if page_count == 0
            || end <= start
            || end > u64::from(u32::MAX) + 1
            || previous_end.is_some_and(|previous| start <= previous)
        {
            return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
        }
        total = total
            .checked_add(u64::from(page_count))
            .ok_or(SnapshotV2BalloonStateDecodeError::InvalidValue)?;
        previous_end = Some(end);
    }
    if total != expected_total {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
    }
    reader.finish_exact()
}

fn decode_accounting<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2BalloonAccountingState, SnapshotV2BalloonStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let count = usize::try_from(reader.read_u32()?)
        .map_err(|_| SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
    reader.read_zeroes(4)?;
    let total = reader.read_u64()?;
    let mut ranges = Vec::new();
    reserve
        .reserve_vec(&mut ranges, count)
        .map_err(|()| SnapshotV2BalloonStateDecodeError::Allocation)?;
    for _ in 0..count {
        ranges.push(SnapshotV2BalloonPfnRange::from_parts(
            reader.read_u32()?,
            reader.read_u32()?,
        ));
    }
    reader.finish_exact()?;
    SnapshotV2BalloonAccountingState::try_new(ranges, total)
        .map_err(SnapshotV2BalloonStateDecodeError::InvalidState)
}

fn preflight_mmio(bytes: &[u8]) -> Result<(), SnapshotV2BalloonStateDecodeError> {
    if bytes.len() != MMIO_SECTION_BYTES {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
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
) -> Result<SnapshotV2MmioDeviceState, SnapshotV2BalloonStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let device_feature_select = reader.read_u32()?;
    let driver_feature_select = reader.read_u32()?;
    let queue_select = reader.read_u32()?;
    let interrupt_line = GuestInterruptLine::new(reader.read_u32()?)
        .map_err(|_| SnapshotV2BalloonStateDecodeError::InvalidValue)?;
    let region_id = MmioRegionId::new(reader.read_u64()?);
    let region_start = GuestAddress::new(reader.read_u64()?);
    let region_size = reader.read_u64()?;
    reader.read_zeroes(8)?;
    reader.finish_exact()?;
    let region = MmioRegion::new(region_id, region_start, region_size)
        .map_err(|_| SnapshotV2BalloonStateDecodeError::InvalidValue)?;
    Ok(SnapshotV2MmioDeviceState::from_parts(
        device_feature_select,
        driver_feature_select,
        queue_select,
        region,
        interrupt_line,
    ))
}

fn preflight_pci(
    bytes: &[u8],
    queue_count: usize,
) -> Result<(), SnapshotV2BalloonStateDecodeError> {
    if bytes.len() < PCI_FIXED_BYTES + PCI_WRITABLE_BYTES + PCI_PROBE_BYTES
        || bytes.len() > PCI_MAX_BYTES
        || !bytes.len().is_multiple_of(ALIGNMENT)
    {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    if reader.read_u8()? != PCI_PHASE_ACTIVE || reader.read_u8()? != PCI_ORIGIN_STARTUP {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
    }
    reader.read_u8()?;
    if reader.read_u8()? != PCI_BAR_MEMORY64 || reader.read_u8()? != PCI_BAR_NOT_PREFETCHABLE {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
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
    let entry_count = queue_count
        .checked_add(1)
        .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
    if counts
        != [
            PCI_WRITABLE_COUNT,
            PCI_PROBE_COUNT,
            entry_count,
            PCI_PENDING_WORD_COUNT,
            queue_count,
        ]
    {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidValue);
    }
    let semantic_length = PCI_FIXED_BYTES
        .checked_add(PCI_WRITABLE_BYTES)
        .and_then(|length| length.checked_add(PCI_PROBE_BYTES))
        .and_then(|length| length.checked_add(entry_count.checked_mul(PCI_MSIX_ENTRY_BYTES)?))
        .and_then(|length| length.checked_add(PCI_PENDING_WORD_BYTES))
        .and_then(|length| length.checked_add(queue_count.checked_mul(PCI_QUEUE_VECTOR_BYTES)?))
        .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
    if align_up(semantic_length) != Some(bytes.len()) {
        return Err(SnapshotV2BalloonStateDecodeError::InvalidStructure);
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
    for _ in 0..entry_count {
        reader.read_bytes(PCI_MSIX_ENTRY_BYTES)?;
    }
    reader.read_u64()?;
    for _ in 0..queue_count {
        reader.read_u16()?;
    }
    reader.finish_padded()
}

fn decode_pci<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2PciDeviceState, SnapshotV2BalloonStateDecodeError> {
    let mut reader = Reader::new(bytes);
    let phase = match reader.read_u8()? {
        PCI_PHASE_ACTIVE => VirtioPciEndpointPhase::Active,
        _ => return Err(SnapshotV2BalloonStateDecodeError::InvalidValue),
    };
    let origin = match reader.read_u8()? {
        PCI_ORIGIN_STARTUP => StorageDeviceOrigin::Startup,
        _ => return Err(SnapshotV2BalloonStateDecodeError::InvalidValue),
    };
    let bar_index = reader.read_u8()?;
    let bar_address_space = match reader.read_u8()? {
        PCI_BAR_MEMORY64 => PciBarAddressSpace::Memory64,
        _ => return Err(SnapshotV2BalloonStateDecodeError::InvalidValue),
    };
    let bar_prefetchable = match reader.read_u8()? {
        PCI_BAR_NOT_PREFETCHABLE => PciBarPrefetchable::No,
        _ => return Err(SnapshotV2BalloonStateDecodeError::InvalidValue),
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
        .map_err(|()| SnapshotV2BalloonStateDecodeError::Allocation)?;
    for _ in 0..writable_count {
        let offset = reader.read_u16()?;
        let value = reader.read_u8()?;
        reader.read_zeroes(1)?;
        writable_bytes.push(SnapshotV2PciWritableByte::from_parts(offset, value));
    }
    let mut bar_probes = Vec::new();
    reserve
        .reserve_vec(&mut bar_probes, probe_count)
        .map_err(|()| SnapshotV2BalloonStateDecodeError::Allocation)?;
    for _ in 0..probe_count {
        let index = reader.read_u8()?;
        let pending = reader.read_bool()?;
        reader.read_zeroes(2)?;
        bar_probes.push(SnapshotV2PciBarProbeState::from_parts(index, pending));
    }
    let mut entries = Vec::new();
    reserve
        .reserve_vec(&mut entries, entry_count)
        .map_err(|()| SnapshotV2BalloonStateDecodeError::Allocation)?;
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
        .map_err(|()| SnapshotV2BalloonStateDecodeError::Allocation)?;
    for _ in 0..pending_count {
        pending_words.push(reader.read_u64()?);
    }
    let mut queue_vectors = Vec::new();
    reserve
        .reserve_vec(&mut queue_vectors, vector_count)
        .map_err(|()| SnapshotV2BalloonStateDecodeError::Allocation)?;
    for _ in 0..vector_count {
        queue_vectors.push(reader.read_u16()?);
    }
    reader.finish_padded()?;

    let sbdf = PciSbdf::new(segment, bus, device, function)
        .map_err(|_| SnapshotV2BalloonStateDecodeError::InvalidValue)?;
    let bar_range = GuestMemoryRange::new(bar_start, bar_size)
        .map_err(|_| SnapshotV2BalloonStateDecodeError::InvalidValue)?;
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

    fn read_u8(&mut self) -> Result<u8, SnapshotV2BalloonStateDecodeError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(SnapshotV2BalloonStateDecodeError::Truncated)?;
        self.position = self
            .position
            .checked_add(1)
            .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
        Ok(value)
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotV2BalloonStateDecodeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotV2BalloonStateDecodeError::InvalidValue),
        }
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotV2BalloonStateDecodeError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotV2BalloonStateDecodeError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotV2BalloonStateDecodeError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_array<const LENGTH: usize>(
        &mut self,
    ) -> Result<[u8; LENGTH], SnapshotV2BalloonStateDecodeError> {
        self.read_bytes(LENGTH)?
            .try_into()
            .map_err(|_| SnapshotV2BalloonStateDecodeError::Truncated)
    }

    fn read_bytes(&mut self, length: usize) -> Result<&'a [u8], SnapshotV2BalloonStateDecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(SnapshotV2BalloonStateDecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn read_zeroes(&mut self, length: usize) -> Result<(), SnapshotV2BalloonStateDecodeError> {
        require_zeroes(self.read_bytes(length)?)
    }

    fn finish_exact(self) -> Result<(), SnapshotV2BalloonStateDecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(SnapshotV2BalloonStateDecodeError::InvalidStructure)
        }
    }

    fn finish_padded(self) -> Result<(), SnapshotV2BalloonStateDecodeError> {
        require_zeroes(
            self.bytes
                .get(self.position..)
                .ok_or(SnapshotV2BalloonStateDecodeError::Truncated)?,
        )
    }
}

fn section_bytes(
    bytes: &[u8],
    bounds: SectionBounds,
) -> Result<&[u8], SnapshotV2BalloonStateDecodeError> {
    slice_at(bytes, bounds.offset, bounds.length)
}

fn slice_at(
    bytes: &[u8],
    offset: usize,
    length: usize,
) -> Result<&[u8], SnapshotV2BalloonStateDecodeError> {
    let end = offset
        .checked_add(length)
        .ok_or(SnapshotV2BalloonStateDecodeError::InvalidStructure)?;
    bytes
        .get(offset..end)
        .ok_or(SnapshotV2BalloonStateDecodeError::Truncated)
}

fn read_u16_at(bytes: &[u8], offset: usize) -> Result<u16, SnapshotV2BalloonStateDecodeError> {
    Ok(u16::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u32_at(bytes: &[u8], offset: usize) -> Result<u32, SnapshotV2BalloonStateDecodeError> {
    Ok(u32::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u64_at(bytes: &[u8], offset: usize) -> Result<u64, SnapshotV2BalloonStateDecodeError> {
    Ok(u64::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_usize_u64_at(
    bytes: &[u8],
    offset: usize,
) -> Result<usize, SnapshotV2BalloonStateDecodeError> {
    usize::try_from(read_u64_at(bytes, offset)?)
        .map_err(|_| SnapshotV2BalloonStateDecodeError::InvalidStructure)
}

fn read_array_at<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotV2BalloonStateDecodeError> {
    slice_at(bytes, offset, LENGTH)?
        .try_into()
        .map_err(|_| SnapshotV2BalloonStateDecodeError::Truncated)
}

fn require_zeroes(bytes: &[u8]) -> Result<(), SnapshotV2BalloonStateDecodeError> {
    if bytes.iter().any(|byte| *byte != 0) {
        Err(SnapshotV2BalloonStateDecodeError::NonzeroReserved)
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

fn set_flag(flags: &mut u16, bit: u16, enabled: bool) {
    if enabled {
        *flags |= bit;
    }
}

const fn has_flag(flags: u16, bit: u16) -> bool {
    flags & bit != 0
}

fn pad_section(
    output: &mut Vec<u8>,
    start: usize,
    section_length: usize,
) -> Result<(), SnapshotV2BalloonStateEncodeError> {
    let written = output
        .len()
        .checked_sub(start)
        .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    let padding = section_length
        .checked_sub(written)
        .ok_or(SnapshotV2BalloonStateEncodeError::LengthOverflow)?;
    write_zeroes(output, padding);
    Ok(())
}

fn finish_exact_section(
    output: &[u8],
    start: usize,
    section_length: usize,
) -> Result<(), SnapshotV2BalloonStateEncodeError> {
    if output
        .len()
        .checked_sub(start)
        .is_some_and(|written| written == section_length)
    {
        Ok(())
    } else {
        Err(SnapshotV2BalloonStateEncodeError::LengthOverflow)
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
