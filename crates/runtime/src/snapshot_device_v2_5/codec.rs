use std::mem::size_of;

use super::*;
use crate::block::{VirtioBlockDeviceId, VirtioBlockQueueState};
use crate::interrupt::GuestInterruptLine;
use crate::memory::GuestAddress;
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::pci::PciSbdf;
use crate::snapshot_device_v2::{
    SnapshotV2BlockBucketState, SnapshotV2PciBarProbeState, SnapshotV2PciDeviceStateParts,
    SnapshotV2PciMsixStateParts, SnapshotV2PciMsixTableEntry, SnapshotV2PciWritableByte,
    SnapshotV2VirtioStateParts,
};

const MAGIC: [u8; 8] = *b"BANGD2A\0";
const PROFILE: u16 = 2;
const FLAGS: u32 = 0;
const SECTION_COUNT_U32: u32 = 4;

const HEADER_MAGIC_OFFSET: usize = 0;
const HEADER_BYTES_OFFSET: usize = 8;
const HEADER_PROFILE_OFFSET: usize = 10;
const HEADER_TRANSPORT_OFFSET: usize = 12;
const HEADER_RECORD_COUNT_OFFSET: usize = 14;
const HEADER_SECTION_COUNT_OFFSET: usize = 16;
const HEADER_RESERVED_OFFSET: usize = 18;
const HEADER_FLAGS_OFFSET: usize = 20;
const HEADER_TOTAL_LENGTH_OFFSET: usize = 24;
const HEADER_ROOT_KIND_OFFSET: usize = 32;
const HEADER_ROOT_INSTANCE_OFFSET: usize = 36;
const HEADER_RECORD_DIRECTORY_OFFSET_OFFSET: usize = 40;
const HEADER_SECTION_DIRECTORY_OFFSET_OFFSET: usize = 48;
const HEADER_PAYLOAD_OFFSET_OFFSET: usize = 56;

const RECORD_KIND_OFFSET: usize = 0;
const RECORD_INSTANCE_OFFSET: usize = 4;
const RECORD_FIRST_SECTION_OFFSET: usize = 8;
const RECORD_SECTION_COUNT_OFFSET: usize = 12;
const RECORD_RESERVED_OFFSET: usize = 16;

const SECTION_RECORD_INDEX_OFFSET: usize = 0;
const SECTION_KIND_OFFSET: usize = 4;
const SECTION_FLAGS_OFFSET: usize = 6;
const SECTION_RESERVED_OFFSET: usize = 8;
const SECTION_PAYLOAD_OFFSET: usize = 16;
const SECTION_LENGTH_OFFSET: usize = 24;

const SECTION_KIND_CONFIG: u16 = 1;
const SECTION_KIND_BLOCK: u16 = 2;
const SECTION_KIND_COMMON: u16 = 3;
const SECTION_KIND_TRANSPORT: u16 = 4;

const TRANSPORT_MMIO: u16 = 1;
const TRANSPORT_PCI: u16 = 2;
const CACHE_UNSAFE: u8 = 0;
const CACHE_WRITEBACK: u8 = 1;
const ENGINE_SYNC: u8 = 1;
const ENGINE_ASYNC: u8 = 2;
const BACKING_REGULAR_FILE: u8 = 1;
const RETRY_NONE: u8 = 0;
const RETRY_IMMEDIATE: u8 = 1;
const RETRY_AFTER: u8 = 2;
const INTERRUPT_QUEUE: u8 = 1;
const INTERRUPT_CONFIGURATION: u8 = 2;
const PCI_PHASE_ACTIVE: u8 = 1;
const PCI_ORIGIN_STARTUP: u8 = 1;
const PCI_ORIGIN_RUNTIME: u8 = 2;
const PCI_BAR_MEMORY64: u8 = 2;
const PCI_BAR_NOT_PREFETCHABLE: u8 = 0;

const PCI_MSIX_ENTRY_BYTES: usize = 16;
const PCI_WRITABLE_COUNT: usize = 4;
const PCI_PROBE_COUNT: usize = 2;
const PCI_MSIX_ENTRY_COUNT: usize = 2;
const PCI_PENDING_WORD_COUNT: usize = 1;
const PCI_QUEUE_VECTOR_COUNT: usize = 1;

const MAX_RECORDS: usize = NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_RECORDS as usize;
const MAX_SECTIONS: usize = MAX_RECORDS * SECTION_COUNT_PER_RECORD;
const MAX_AGGREGATE_STRING_BYTES: usize = MAX_RECORDS
    * (NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES
        + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES
        + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES);

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
struct SectionLayout {
    record_index: u32,
    kind: u16,
    offset: usize,
    length: usize,
}

impl SectionLayout {
    const EMPTY: Self = Self {
        record_index: 0,
        kind: 0,
        offset: 0,
        length: 0,
    };
}

struct EncodeLayout {
    record_directory_offset: usize,
    section_directory_offset: usize,
    payload_offset: usize,
    total_length: usize,
    section_count: usize,
    sections: [SectionLayout; MAX_SECTIONS],
}

pub(super) fn encode(
    version: SnapshotFormatVersion,
    graph: &SnapshotV2MultiBlockDeviceGraph,
) -> Result<Vec<u8>, SnapshotV2MultiBlockDeviceGraphEncodeError> {
    encode_with_policy(version, graph, &mut FallibleReserve)
}

pub(super) fn encode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    graph: &SnapshotV2MultiBlockDeviceGraph,
    reserve: &mut R,
) -> Result<Vec<u8>, SnapshotV2MultiBlockDeviceGraphEncodeError> {
    if version != NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        return Err(SnapshotV2MultiBlockDeviceGraphEncodeError::UnsupportedVersion);
    }
    validate_graph(graph).map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?;
    let layout = calculate_layout(graph)?;
    let mut output = Vec::new();
    reserve
        .reserve_vec(&mut output, layout.total_length)
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphEncodeError::Allocation)?;

    write_header(&mut output, graph, &layout)?;
    write_record_directory(&mut output, graph)?;
    write_section_directory(&mut output, &layout)?;
    let sections = layout
        .sections
        .get(..layout.section_count)
        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?;
    for (record, sections) in graph
        .records
        .iter()
        .zip(sections.chunks_exact(SECTION_COUNT_PER_RECORD))
    {
        let config = sections
            .first()
            .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?;
        encode_config(&mut output, &record.config, config.length)?;
        let block = sections
            .get(1)
            .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?;
        encode_block(&mut output, &record.block, block.length)?;
        let common = sections
            .get(2)
            .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?;
        encode_common(&mut output, &record.virtio, common.length)?;
        let transport = sections
            .get(3)
            .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?;
        encode_transport(&mut output, &record.transport, transport.length)?;
    }
    if output.len() != layout.total_length {
        return Err(SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph);
    }
    Ok(output)
}

fn calculate_layout(
    graph: &SnapshotV2MultiBlockDeviceGraph,
) -> Result<EncodeLayout, SnapshotV2MultiBlockDeviceGraphEncodeError> {
    let record_count = graph.records.len();
    let section_count = record_count
        .checked_mul(SECTION_COUNT_PER_RECORD)
        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
    let record_directory_offset = NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_HEADER_BYTES;
    let section_directory_offset = record_directory_offset
        .checked_add(
            record_count
                .checked_mul(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_RECORD_ENTRY_BYTES)
                .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
        )
        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
    let payload_offset = section_directory_offset
        .checked_add(
            section_count
                .checked_mul(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_SECTION_ENTRY_BYTES)
                .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
        )
        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
    let mut sections = [SectionLayout::EMPTY; MAX_SECTIONS];
    let mut cursor = payload_offset;
    let mut section_index = 0usize;
    for (record_index, record) in graph.records.iter().enumerate() {
        let config_length = aligned_length(
            CONFIG_FIXED_BYTES
                .checked_add(record.config.drive_id.len())
                .and_then(|length| {
                    length.checked_add(record.config.partuuid.as_ref().map_or(0, String::len))
                })
                .and_then(|length| length.checked_add(record.config.selector.len()))
                .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
        )
        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
        let common_length = aligned_length(
            COMMON_FIXED_BYTES
                .checked_add(
                    record
                        .virtio
                        .queues()
                        .len()
                        .checked_mul(COMMON_QUEUE_BYTES)
                        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
                )
                .and_then(|length| {
                    length.checked_add(
                        record
                            .virtio
                            .pending_notifications()
                            .len()
                            .checked_mul(size_of::<u16>())?,
                    )
                })
                .and_then(|length| {
                    length.checked_add(
                        record
                            .virtio
                            .interrupt_intents()
                            .len()
                            .checked_mul(size_of::<u32>())?,
                    )
                })
                .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
        )
        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
        let transport_length = match record.transport {
            SnapshotV2DeviceTransport::Mmio(_) => MMIO_SECTION_BYTES,
            SnapshotV2DeviceTransport::Pci(_) => PCI_SECTION_BYTES,
        };
        for (kind, length) in [
            (SECTION_KIND_CONFIG, config_length),
            (SECTION_KIND_BLOCK, BLOCK_SECTION_BYTES),
            (SECTION_KIND_COMMON, common_length),
            (SECTION_KIND_TRANSPORT, transport_length),
        ] {
            let slot = sections
                .get_mut(section_index)
                .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
            *slot = SectionLayout {
                record_index: u32::try_from(record_index)
                    .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
                kind,
                offset: cursor,
                length,
            };
            cursor = cursor
                .checked_add(length)
                .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
            section_index = section_index
                .checked_add(1)
                .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
        }
    }
    if cursor > NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_BYTES {
        return Err(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge);
    }
    Ok(EncodeLayout {
        record_directory_offset,
        section_directory_offset,
        payload_offset,
        total_length: cursor,
        section_count,
        sections,
    })
}

fn write_header(
    output: &mut Vec<u8>,
    graph: &SnapshotV2MultiBlockDeviceGraph,
    layout: &EncodeLayout,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    write_bytes(output, &MAGIC);
    write_u16(
        output,
        u16::try_from(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_HEADER_BYTES)
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
    );
    write_u16(output, PROFILE);
    write_u16(output, transport_tag(graph.transport_kind));
    write_u16(
        output,
        u16::try_from(graph.records.len())
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
    );
    write_u16(
        output,
        u16::try_from(layout.section_count)
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
    );
    write_u16(output, 0);
    write_u32(output, FLAGS);
    write_u64(
        output,
        u64::try_from(layout.total_length)
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
    );
    match graph.root_key {
        Some(key) => {
            write_u32(output, key.kind());
            write_u32(output, key.instance());
        }
        None => {
            write_u32(output, 0);
            write_u32(output, 0);
        }
    }
    for offset in [
        layout.record_directory_offset,
        layout.section_directory_offset,
        layout.payload_offset,
    ] {
        write_u64(
            output,
            u64::try_from(offset)
                .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
        );
    }
    Ok(())
}

fn write_record_directory(
    output: &mut Vec<u8>,
    graph: &SnapshotV2MultiBlockDeviceGraph,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    for (index, record) in graph.records.iter().enumerate() {
        write_u32(output, record.key.kind());
        write_u32(output, record.key.instance());
        write_u32(
            output,
            u32::try_from(
                index
                    .checked_mul(SECTION_COUNT_PER_RECORD)
                    .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
            )
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
        );
        write_u32(output, SECTION_COUNT_U32);
        write_zeroes(output, 16)?;
    }
    Ok(())
}

fn write_section_directory(
    output: &mut Vec<u8>,
    layout: &EncodeLayout,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    for section in layout
        .sections
        .get(..layout.section_count)
        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?
    {
        write_u32(output, section.record_index);
        write_u16(output, section.kind);
        write_u16(output, 0);
        write_u64(output, 0);
        write_u64(
            output,
            u64::try_from(section.offset)
                .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
        );
        write_u64(
            output,
            u64::try_from(section.length)
                .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?,
        );
    }
    Ok(())
}

fn encode_config(
    output: &mut Vec<u8>,
    config: &SnapshotV2MultiBlockConfig,
    section_length: usize,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    let start = output.len();
    write_bool(output, config.is_read_only);
    write_u8(
        output,
        match config.io_engine {
            DriveIoEngine::Sync => ENGINE_SYNC,
            DriveIoEngine::Async => ENGINE_ASYNC,
        },
    );
    write_u8(
        output,
        match config.cache_type {
            DriveCacheType::Unsafe => CACHE_UNSAFE,
            DriveCacheType::Writeback => CACHE_WRITEBACK,
        },
    );
    write_bool(output, config.is_root);
    write_bool(output, config.partuuid.is_some());
    write_bool(
        output,
        config
            .rate_limiter
            .and_then(DriveRateLimiterConfig::bandwidth)
            .is_some(),
    );
    write_bool(
        output,
        config
            .rate_limiter
            .and_then(DriveRateLimiterConfig::ops)
            .is_some(),
    );
    write_u8(output, BACKING_REGULAR_FILE);
    for length in [
        config.drive_id.len(),
        config.partuuid.as_ref().map_or(0, String::len),
        config.selector.len(),
    ] {
        write_u16(
            output,
            u16::try_from(length)
                .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?,
        );
    }
    write_u16(output, 0);
    encode_bucket_config(
        output,
        config
            .rate_limiter
            .and_then(DriveRateLimiterConfig::bandwidth),
    )?;
    encode_bucket_config(
        output,
        config.rate_limiter.and_then(DriveRateLimiterConfig::ops),
    )?;
    write_bytes(output, config.drive_id.as_bytes());
    if let Some(partuuid) = config.partuuid.as_deref() {
        write_bytes(output, partuuid.as_bytes());
    }
    write_bytes(output, config.selector.as_bytes());
    pad_section(output, start, section_length)
}

fn encode_bucket_config(
    output: &mut Vec<u8>,
    bucket: Option<DriveTokenBucketConfig>,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    let (size, burst, refill_time, burst_present) = match bucket {
        Some(bucket) => (
            bucket.size(),
            bucket.one_time_burst().unwrap_or(0),
            bucket.refill_time(),
            bucket.one_time_burst().is_some(),
        ),
        None => (0, 0, 0, false),
    };
    write_u64(output, size);
    write_u64(output, burst);
    write_u64(output, refill_time);
    write_bool(output, burst_present);
    write_zeroes(output, 7)
}

fn encode_block(
    output: &mut Vec<u8>,
    block: &SnapshotV2MultiBlockState,
    section_length: usize,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    let start = output.len();
    let continuation = &block.continuation;
    write_u16(
        output,
        u16::try_from(VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE)
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?,
    );
    write_bool(output, continuation.active_queue().is_some());
    write_bool(output, continuation.limiter().bandwidth().is_some());
    write_bool(output, continuation.limiter().ops().is_some());
    let (retry_tag, retry_nanos) = match continuation.retry() {
        StorageRetryState::None => (RETRY_NONE, 0),
        StorageRetryState::Immediate => (RETRY_IMMEDIATE, 0),
        StorageRetryState::After { remaining_nanos } => (RETRY_AFTER, remaining_nanos),
    };
    write_u8(output, retry_tag);
    write_zeroes(output, 2)?;
    write_u64(output, continuation.capacity_sectors());
    write_u64(output, block.backing_bytes);
    write_bytes(output, continuation.device_id().as_bytes());
    write_zeroes(output, 4)?;
    let (next_available, next_used) = continuation
        .active_queue()
        .map_or((0, 0), |queue| (queue.next_available(), queue.next_used()));
    write_u16(output, next_available);
    write_u16(output, next_used);
    write_zeroes(output, 4)?;
    encode_bucket_state(output, continuation.limiter().bandwidth())?;
    encode_bucket_state(output, continuation.limiter().ops())?;
    write_u64(output, retry_nanos);
    pad_section(output, start, section_length)
}

fn encode_bucket_state(
    output: &mut Vec<u8>,
    bucket: Option<SnapshotV2BlockBucketState>,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    let (budget, remaining_burst, age_nanos) = bucket.map_or((0, 0, 0), |bucket| {
        (
            bucket.budget(),
            bucket.remaining_burst(),
            bucket.age_nanos(),
        )
    });
    write_u64(output, budget);
    write_u64(output, remaining_burst);
    write_u64(output, age_nanos);
    Ok(())
}

fn encode_common(
    output: &mut Vec<u8>,
    common: &SnapshotV2VirtioState,
    section_length: usize,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
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
                .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?,
        );
    }
    for queue in common.queues() {
        write_u16(output, queue.max_size());
        write_u16(output, queue.size());
        write_bool(output, queue.ready());
        write_zeroes(output, 3)?;
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

fn encode_transport(
    output: &mut Vec<u8>,
    transport: &SnapshotV2DeviceTransport,
    section_length: usize,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    match transport {
        SnapshotV2DeviceTransport::Mmio(state) => encode_mmio(output, state, section_length),
        SnapshotV2DeviceTransport::Pci(state) => encode_pci(output, state, section_length),
    }
}

fn encode_mmio(
    output: &mut Vec<u8>,
    state: &SnapshotV2MmioDeviceState,
    section_length: usize,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    let start = output.len();
    write_u32(output, state.device_feature_select());
    write_u32(output, state.driver_feature_select());
    write_u32(output, state.queue_select());
    write_u32(output, state.interrupt_line().raw_value());
    write_u64(output, state.region().id().raw_value());
    write_u64(output, state.region().range().start().raw_value());
    write_u64(output, state.region().range().size());
    write_zeroes(output, 8)?;
    pad_section(output, start, section_length)
}

fn encode_pci(
    output: &mut Vec<u8>,
    state: &SnapshotV2PciDeviceState,
    section_length: usize,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
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
    write_zeroes(output, 4)?;
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
                .map_err(|_| SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph)?,
        );
    }
    write_zeroes(output, 4)?;
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
        write_zeroes(output, 2)?;
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
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    let target = start
        .checked_add(section_length)
        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
    if output.len() > target {
        return Err(SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph);
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

fn write_zeroes(
    output: &mut Vec<u8>,
    count: usize,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphEncodeError> {
    let target = output
        .len()
        .checked_add(count)
        .ok_or(SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge)?;
    output.resize(target, 0);
    Ok(())
}

fn transport_tag(kind: SnapshotV2DeviceTransportKind) -> u16 {
    match kind {
        SnapshotV2DeviceTransportKind::Mmio => TRANSPORT_MMIO,
        SnapshotV2DeviceTransportKind::Pci => TRANSPORT_PCI,
    }
}

fn aligned_length(length: usize) -> Option<usize> {
    length
        .checked_add(ALIGNMENT - 1)
        .map(|rounded| rounded & !(ALIGNMENT - 1))
}

#[derive(Clone, Copy)]
struct SectionBounds {
    offset: usize,
    length: usize,
}

impl SectionBounds {
    const EMPTY: Self = Self {
        offset: 0,
        length: 0,
    };
}

struct Preflight {
    record_count: usize,
    transport_kind: SnapshotV2DeviceTransportKind,
    root_key: Option<SnapshotV2DeviceKey>,
    record_keys: [SnapshotV2DeviceKey; MAX_RECORDS],
    sections: [SectionBounds; MAX_SECTIONS],
}

pub(super) fn decode(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<SnapshotV2MultiBlockDeviceGraph, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    decode_with_policy(version, bytes, &mut FallibleReserve)
}

pub(super) fn decode_with_policy<R: ReservePolicy>(
    version: SnapshotFormatVersion,
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2MultiBlockDeviceGraph, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let preflight = preflight(version, bytes)?;
    let mut records = Vec::new();
    reserve
        .reserve_vec(&mut records, preflight.record_count)
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
    for record_index in 0..preflight.record_count {
        let section_index = record_index
            .checked_mul(SECTION_COUNT_PER_RECORD)
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
        let config = decode_config(
            section_bytes(
                bytes,
                *preflight
                    .sections
                    .get(section_index)
                    .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
            )?,
            reserve,
        )?;
        let block = decode_block(section_bytes(
            bytes,
            *preflight
                .sections
                .get(section_index + 1)
                .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
        )?)?;
        let virtio = decode_common(
            section_bytes(
                bytes,
                *preflight
                    .sections
                    .get(section_index + 2)
                    .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
            )?,
            reserve,
        )?;
        let transport_bytes = section_bytes(
            bytes,
            *preflight
                .sections
                .get(section_index + 3)
                .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
        )?;
        let transport = match preflight.transport_kind {
            SnapshotV2DeviceTransportKind::Mmio => {
                SnapshotV2DeviceTransport::Mmio(decode_mmio(transport_bytes)?)
            }
            SnapshotV2DeviceTransportKind::Pci => {
                SnapshotV2DeviceTransport::Pci(decode_pci(transport_bytes, reserve)?)
            }
        };
        records.push(SnapshotV2MultiBlockDeviceRecord {
            key: *preflight
                .record_keys
                .get(record_index)
                .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
            config,
            block,
            virtio,
            transport,
        });
    }
    SnapshotV2MultiBlockDeviceGraph::try_from_parts(
        preflight.root_key,
        preflight.transport_kind,
        records,
    )
    .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidGraph)
}

fn preflight(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<Preflight, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    if version != NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::UnsupportedVersion);
    }
    if bytes.len() < NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_HEADER_BYTES {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::TooSmall);
    }
    if bytes.len() > NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_BYTES {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::TooLarge);
    }
    if read_array_at::<8>(bytes, HEADER_MAGIC_OFFSET)? != MAGIC {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidMagic);
    }
    let transport_kind = match read_u16_at(bytes, HEADER_TRANSPORT_OFFSET)? {
        TRANSPORT_MMIO => SnapshotV2DeviceTransportKind::Mmio,
        TRANSPORT_PCI => SnapshotV2DeviceTransportKind::Pci,
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::UnsupportedProfile),
    };
    let record_count = usize::from(read_u16_at(bytes, HEADER_RECORD_COUNT_OFFSET)?);
    let section_count = usize::from(read_u16_at(bytes, HEADER_SECTION_COUNT_OFFSET)?);
    if usize::from(read_u16_at(bytes, HEADER_BYTES_OFFSET)?)
        != NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_HEADER_BYTES
        || read_u16_at(bytes, HEADER_PROFILE_OFFSET)? != PROFILE
        || record_count == 0
        || record_count > MAX_RECORDS
        || section_count
            != record_count
                .checked_mul(SECTION_COUNT_PER_RECORD)
                .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?
        || read_u32_at(bytes, HEADER_FLAGS_OFFSET)? != FLAGS
    {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::UnsupportedProfile);
    }
    if read_u16_at(bytes, HEADER_RESERVED_OFFSET)? != 0 {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::NonzeroReserved);
    }
    let total_length = read_usize_u64_at(bytes, HEADER_TOTAL_LENGTH_OFFSET)?;
    let record_directory_offset = NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_HEADER_BYTES;
    let section_directory_offset = record_directory_offset
        .checked_add(
            record_count
                .checked_mul(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_RECORD_ENTRY_BYTES)
                .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
        )
        .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
    let payload_offset = section_directory_offset
        .checked_add(
            section_count
                .checked_mul(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_SECTION_ENTRY_BYTES)
                .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
        )
        .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
    if total_length != bytes.len()
        || read_usize_u64_at(bytes, HEADER_RECORD_DIRECTORY_OFFSET_OFFSET)?
            != record_directory_offset
        || read_usize_u64_at(bytes, HEADER_SECTION_DIRECTORY_OFFSET_OFFSET)?
            != section_directory_offset
        || read_usize_u64_at(bytes, HEADER_PAYLOAD_OFFSET_OFFSET)? != payload_offset
    {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
    }
    let root_kind = read_u32_at(bytes, HEADER_ROOT_KIND_OFFSET)?;
    let root_instance = read_u32_at(bytes, HEADER_ROOT_INSTANCE_OFFSET)?;
    let root_key = match (root_kind, root_instance) {
        (0, 0) => None,
        (DEVICE_KIND_BLOCK, 0) => Some(SnapshotV2DeviceKey::block(0)),
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::UnsupportedProfile),
    };

    let mut record_keys = [SnapshotV2DeviceKey::block(0); MAX_RECORDS];
    for record_index in 0..record_count {
        let offset = record_directory_offset
            .checked_add(
                record_index
                    .checked_mul(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_RECORD_ENTRY_BYTES)
                    .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
            )
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
        let entry = slice_at(
            bytes,
            offset,
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_RECORD_ENTRY_BYTES,
        )?;
        if read_u32_at(entry, RECORD_KIND_OFFSET)? != DEVICE_KIND_BLOCK
            || read_u32_at(entry, RECORD_INSTANCE_OFFSET)?
                != u32::try_from(record_index)
                    .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?
            || read_u32_at(entry, RECORD_FIRST_SECTION_OFFSET)?
                != u32::try_from(
                    record_index
                        .checked_mul(SECTION_COUNT_PER_RECORD)
                        .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
                )
                .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?
            || read_u32_at(entry, RECORD_SECTION_COUNT_OFFSET)? != SECTION_COUNT_U32
        {
            return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
        }
        require_zeroes(
            entry
                .get(RECORD_RESERVED_OFFSET..)
                .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated)?,
        )?;
        let key = record_keys
            .get_mut(record_index)
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
        *key = SnapshotV2DeviceKey::block(
            u32::try_from(record_index)
                .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
        );
    }

    let mut sections = [SectionBounds::EMPTY; MAX_SECTIONS];
    let mut expected_payload_offset = payload_offset;
    let mut aggregate_string_bytes = 0usize;
    for section_index in 0..section_count {
        let entry_offset = section_directory_offset
            .checked_add(
                section_index
                    .checked_mul(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_SECTION_ENTRY_BYTES)
                    .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
            )
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
        let entry = slice_at(
            bytes,
            entry_offset,
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_SECTION_ENTRY_BYTES,
        )?;
        let record_index = section_index / SECTION_COUNT_PER_RECORD;
        let expected_kind = match section_index % SECTION_COUNT_PER_RECORD {
            0 => SECTION_KIND_CONFIG,
            1 => SECTION_KIND_BLOCK,
            2 => SECTION_KIND_COMMON,
            3 => SECTION_KIND_TRANSPORT,
            _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure),
        };
        if read_u32_at(entry, SECTION_RECORD_INDEX_OFFSET)?
            != u32::try_from(record_index)
                .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?
            || read_u16_at(entry, SECTION_KIND_OFFSET)? != expected_kind
            || read_u16_at(entry, SECTION_FLAGS_OFFSET)? != 0
            || read_u64_at(entry, SECTION_RESERVED_OFFSET)? != 0
        {
            return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
        }
        let offset = read_usize_u64_at(entry, SECTION_PAYLOAD_OFFSET)?;
        let length = read_usize_u64_at(entry, SECTION_LENGTH_OFFSET)?;
        let end = offset
            .checked_add(length)
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
        if offset != expected_payload_offset
            || length == 0
            || !offset.is_multiple_of(ALIGNMENT)
            || !length.is_multiple_of(ALIGNMENT)
            || end > bytes.len()
        {
            return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
        }
        let bounds = SectionBounds { offset, length };
        let section = section_bytes(bytes, bounds)?;
        match expected_kind {
            SECTION_KIND_CONFIG => {
                aggregate_string_bytes = aggregate_string_bytes
                    .checked_add(preflight_config(section)?)
                    .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
                if aggregate_string_bytes > MAX_AGGREGATE_STRING_BYTES {
                    return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidString);
                }
            }
            SECTION_KIND_BLOCK => preflight_block(section)?,
            SECTION_KIND_COMMON => preflight_common(section)?,
            SECTION_KIND_TRANSPORT => match transport_kind {
                SnapshotV2DeviceTransportKind::Mmio => preflight_mmio(section)?,
                SnapshotV2DeviceTransportKind::Pci => preflight_pci(section)?,
            },
            _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure),
        }
        let slot = sections
            .get_mut(section_index)
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
        *slot = bounds;
        expected_payload_offset = end;
    }
    if expected_payload_offset != bytes.len() {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
    }
    Ok(Preflight {
        record_count,
        transport_kind,
        root_key,
        record_keys,
        sections,
    })
}

fn preflight_config(bytes: &[u8]) -> Result<usize, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    if bytes.len() < CONFIG_FIXED_BYTES {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated);
    }
    let mut reader = Reader::new(bytes);
    reader.read_bool()?;
    match reader.read_u8()? {
        ENGINE_SYNC | ENGINE_ASYNC => {}
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
    }
    match reader.read_u8()? {
        CACHE_UNSAFE | CACHE_WRITEBACK => {}
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
    }
    reader.read_bool()?;
    let partuuid_present = reader.read_bool()?;
    let bandwidth_present = reader.read_bool()?;
    let ops_present = reader.read_bool()?;
    if reader.read_u8()? != BACKING_REGULAR_FILE {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
    }
    let drive_id_len = usize::from(reader.read_u16()?);
    let partuuid_len = usize::from(reader.read_u16()?);
    let selector_len = usize::from(reader.read_u16()?);
    reader.read_zeroes(2)?;
    preflight_bucket_config(&mut reader, bandwidth_present)?;
    preflight_bucket_config(&mut reader, ops_present)?;
    if drive_id_len == 0
        || drive_id_len > NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES
        || selector_len == 0
        || selector_len > NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES
        || partuuid_present != (partuuid_len != 0)
        || partuuid_len > NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES
    {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidString);
    }
    let semantic_length = CONFIG_FIXED_BYTES
        .checked_add(drive_id_len)
        .and_then(|length| length.checked_add(partuuid_len))
        .and_then(|length| length.checked_add(selector_len))
        .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
    if aligned_length(semantic_length) != Some(bytes.len()) {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
    }
    let drive_id = reader.read_bytes(drive_id_len)?;
    let partuuid = reader.read_bytes(partuuid_len)?;
    let selector = reader.read_bytes(selector_len)?;
    let drive_id = std::str::from_utf8(drive_id)
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidString)?;
    if !drive_id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
        || std::str::from_utf8(partuuid).is_err()
        || std::str::from_utf8(selector).is_err()
    {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidString);
    }
    reader.finish_padded()?;
    drive_id_len
        .checked_add(partuuid_len)
        .and_then(|length| length.checked_add(selector_len))
        .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)
}

fn preflight_bucket_config(
    reader: &mut Reader<'_>,
    present: bool,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let size = reader.read_u64()?;
    let burst = reader.read_u64()?;
    let refill_time = reader.read_u64()?;
    let burst_present = reader.read_bool()?;
    reader.read_zeroes(7)?;
    if !present {
        if size != 0 || burst != 0 || refill_time != 0 || burst_present {
            return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
        }
        return Ok(());
    }
    let config = DriveTokenBucketConfig::new(
        size,
        if burst_present { Some(burst) } else { None },
        refill_time,
    );
    if (!burst_present && burst != 0) || !token_bucket_is_enabled(config) {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
    }
    Ok(())
}

fn preflight_block(bytes: &[u8]) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
    if bytes.len() != BLOCK_SECTION_BYTES {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    if usize::from(reader.read_u16()?) != VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
    }
    let active = reader.read_bool()?;
    let bandwidth = reader.read_bool()?;
    let ops = reader.read_bool()?;
    let retry = reader.read_u8()?;
    reader.read_zeroes(2)?;
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_bytes(20)?;
    reader.read_zeroes(4)?;
    let next_available = reader.read_u16()?;
    let next_used = reader.read_u16()?;
    reader.read_zeroes(4)?;
    preflight_bucket_state(&mut reader, bandwidth)?;
    preflight_bucket_state(&mut reader, ops)?;
    let retry_nanos = reader.read_u64()?;
    if (!active && (next_available != 0 || next_used != 0))
        || !matches!(
            (retry, retry_nanos),
            (RETRY_NONE | RETRY_IMMEDIATE, 0) | (RETRY_AFTER, 1..=u64::MAX)
        )
    {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
    }
    reader.finish_exact()
}

fn preflight_bucket_state(
    reader: &mut Reader<'_>,
    present: bool,
) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let budget = reader.read_u64()?;
    let burst = reader.read_u64()?;
    let age = reader.read_u64()?;
    if !present && (budget != 0 || burst != 0 || age != 0) {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
    }
    Ok(())
}

fn preflight_common(bytes: &[u8]) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
    if bytes.len() < COMMON_FIXED_BYTES + COMMON_QUEUE_BYTES {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated);
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
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
    }
    let semantic_length = COMMON_FIXED_BYTES
        .checked_add(
            queue_count
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?,
        )
        .and_then(|length| length.checked_add(notification_count.checked_mul(size_of::<u16>())?))
        .and_then(|length| length.checked_add(intent_count.checked_mul(size_of::<u32>())?))
        .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
    if aligned_length(semantic_length) != Some(bytes.len()) {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
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
            return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
        }
    }
    reader.finish_padded()
}

fn preflight_mmio(bytes: &[u8]) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
    if bytes.len() != MMIO_SECTION_BYTES {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
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

fn preflight_pci(bytes: &[u8]) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
    if bytes.len() != PCI_SECTION_BYTES {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    if reader.read_u8()? != PCI_PHASE_ACTIVE
        || !matches!(reader.read_u8()?, PCI_ORIGIN_STARTUP | PCI_ORIGIN_RUNTIME)
    {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
    }
    reader.read_u8()?;
    if reader.read_u8()? != PCI_BAR_MEMORY64 || reader.read_u8()? != PCI_BAR_NOT_PREFETCHABLE {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
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
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
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
    for _ in 0..PCI_PENDING_WORD_COUNT {
        reader.read_u64()?;
    }
    for _ in 0..PCI_QUEUE_VECTOR_COUNT {
        reader.read_u16()?;
    }
    reader.finish_padded()
}

fn decode_config<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2MultiBlockConfig, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let mut reader = Reader::new(bytes);
    let is_read_only = reader.read_bool()?;
    let io_engine = match reader.read_u8()? {
        ENGINE_SYNC => DriveIoEngine::Sync,
        ENGINE_ASYNC => DriveIoEngine::Async,
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
    };
    let cache_type = match reader.read_u8()? {
        CACHE_UNSAFE => DriveCacheType::Unsafe,
        CACHE_WRITEBACK => DriveCacheType::Writeback,
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
    };
    let is_root = reader.read_bool()?;
    let partuuid_present = reader.read_bool()?;
    let bandwidth_present = reader.read_bool()?;
    let ops_present = reader.read_bool()?;
    if reader.read_u8()? != BACKING_REGULAR_FILE {
        return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue);
    }
    let drive_id_len = usize::from(reader.read_u16()?);
    let partuuid_len = usize::from(reader.read_u16()?);
    let selector_len = usize::from(reader.read_u16()?);
    reader.read_zeroes(2)?;
    let bandwidth = decode_bucket_config(&mut reader, bandwidth_present)?;
    let ops = decode_bucket_config(&mut reader, ops_present)?;
    let drive_id = reader.read_string(drive_id_len, reserve)?;
    let partuuid = if partuuid_present {
        Some(reader.read_string(partuuid_len, reserve)?)
    } else {
        None
    };
    let selector = reader.read_string(selector_len, reserve)?;
    reader.finish_padded()?;
    let rate_limiter = if bandwidth.is_some() || ops.is_some() {
        Some(DriveRateLimiterConfig::new(bandwidth, ops))
    } else {
        None
    };
    Ok(SnapshotV2MultiBlockConfig {
        drive_id,
        partuuid,
        is_root,
        is_read_only,
        cache_type,
        io_engine,
        rate_limiter,
        selector,
    })
}

fn decode_bucket_config(
    reader: &mut Reader<'_>,
    present: bool,
) -> Result<Option<DriveTokenBucketConfig>, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let size = reader.read_u64()?;
    let burst = reader.read_u64()?;
    let refill_time = reader.read_u64()?;
    let burst_present = reader.read_bool()?;
    reader.read_zeroes(7)?;
    if !present {
        return Ok(None);
    }
    Ok(Some(DriveTokenBucketConfig::new(
        size,
        if burst_present { Some(burst) } else { None },
        refill_time,
    )))
}

fn decode_block(
    bytes: &[u8],
) -> Result<SnapshotV2MultiBlockState, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let mut reader = Reader::new(bytes);
    reader.read_u16()?;
    let active = reader.read_bool()?;
    let bandwidth = reader.read_bool()?;
    let ops = reader.read_bool()?;
    let retry_tag = reader.read_u8()?;
    reader.read_zeroes(2)?;
    let capacity_sectors = reader.read_u64()?;
    let backing_bytes = reader.read_u64()?;
    let device_id = VirtioBlockDeviceId::new(reader.read_array::<20>()?);
    reader.read_zeroes(4)?;
    let next_available = reader.read_u16()?;
    let next_used = reader.read_u16()?;
    reader.read_zeroes(4)?;
    let bandwidth = decode_bucket_state(&mut reader, bandwidth)?;
    let ops = decode_bucket_state(&mut reader, ops)?;
    let retry_nanos = reader.read_u64()?;
    reader.finish_exact()?;
    let active_queue = active.then_some(VirtioBlockQueueState::new(next_available, next_used));
    let retry = match (retry_tag, retry_nanos) {
        (RETRY_NONE, 0) => StorageRetryState::None,
        (RETRY_IMMEDIATE, 0) => StorageRetryState::Immediate,
        (RETRY_AFTER, remaining_nanos) if remaining_nanos != 0 => {
            StorageRetryState::After { remaining_nanos }
        }
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
    };
    Ok(SnapshotV2MultiBlockState {
        backing_bytes,
        continuation: SnapshotV2BlockState::from_parts(
            capacity_sectors,
            device_id,
            active_queue,
            SnapshotV2BlockLimiterState::from_parts(bandwidth, ops),
            retry,
        ),
    })
}

fn decode_bucket_state(
    reader: &mut Reader<'_>,
    present: bool,
) -> Result<Option<SnapshotV2BlockBucketState>, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let budget = reader.read_u64()?;
    let remaining_burst = reader.read_u64()?;
    let age_nanos = reader.read_u64()?;
    Ok(present.then_some(SnapshotV2BlockBucketState::from_parts(
        budget,
        remaining_burst,
        age_nanos,
    )))
}

fn decode_common<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2VirtioState, SnapshotV2MultiBlockDeviceGraphDecodeError> {
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
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
    for _ in 0..queue_count {
        queues.push(SnapshotV2VirtioQueueState::from_parts(
            reader.read_u16()?,
            reader.read_u16()?,
            reader.read_bool()?,
            {
                reader.read_zeroes(3)?;
                GuestAddress::new(reader.read_u64()?)
            },
            GuestAddress::new(reader.read_u64()?),
            GuestAddress::new(reader.read_u64()?),
        ));
    }
    let mut pending_notifications = Vec::new();
    reserve
        .reserve_vec(&mut pending_notifications, notification_count)
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
    for _ in 0..notification_count {
        pending_notifications.push(reader.read_u16()?);
    }
    let mut interrupt_intents = Vec::new();
    reserve
        .reserve_vec(&mut interrupt_intents, intent_count)
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
    for _ in 0..intent_count {
        let tag = reader.read_u8()?;
        reader.read_zeroes(1)?;
        let queue_index = reader.read_u16()?;
        interrupt_intents.push(match tag {
            INTERRUPT_QUEUE => SnapshotV2InterruptIntent::Queue { queue_index },
            INTERRUPT_CONFIGURATION if queue_index == 0 => SnapshotV2InterruptIntent::Configuration,
            _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
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

fn decode_mmio(
    bytes: &[u8],
) -> Result<SnapshotV2MmioDeviceState, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let mut reader = Reader::new(bytes);
    let device_feature_select = reader.read_u32()?;
    let driver_feature_select = reader.read_u32()?;
    let queue_select = reader.read_u32()?;
    let interrupt_line = GuestInterruptLine::new(reader.read_u32()?)
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue)?;
    let region_id = MmioRegionId::new(reader.read_u64()?);
    let start = GuestAddress::new(reader.read_u64()?);
    let size = reader.read_u64()?;
    reader.read_zeroes(8)?;
    reader.finish_exact()?;
    let region = MmioRegion::new(region_id, start, size)
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue)?;
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
) -> Result<SnapshotV2PciDeviceState, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let mut reader = Reader::new(bytes);
    let phase = match reader.read_u8()? {
        PCI_PHASE_ACTIVE => VirtioPciEndpointPhase::Active,
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
    };
    let origin = match reader.read_u8()? {
        PCI_ORIGIN_STARTUP => StorageDeviceOrigin::Startup,
        PCI_ORIGIN_RUNTIME => StorageDeviceOrigin::Runtime,
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
    };
    let bar_index = reader.read_u8()?;
    let bar_address_space = match reader.read_u8()? {
        PCI_BAR_MEMORY64 => PciBarAddressSpace::Memory64,
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
    };
    let bar_prefetchable = match reader.read_u8()? {
        PCI_BAR_NOT_PREFETCHABLE => PciBarPrefetchable::No,
        _ => return Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
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
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
    for _ in 0..writable_count {
        let offset = reader.read_u16()?;
        let value = reader.read_u8()?;
        reader.read_zeroes(1)?;
        writable_bytes.push(SnapshotV2PciWritableByte::from_parts(offset, value));
    }
    let mut bar_probes = Vec::new();
    reserve
        .reserve_vec(&mut bar_probes, probe_count)
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
    for _ in 0..probe_count {
        let index = reader.read_u8()?;
        let pending = reader.read_bool()?;
        reader.read_zeroes(2)?;
        bar_probes.push(SnapshotV2PciBarProbeState::from_parts(index, pending));
    }
    let mut entries = Vec::new();
    reserve
        .reserve_vec(&mut entries, entry_count)
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
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
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
    for _ in 0..pending_count {
        pending_words.push(reader.read_u64()?);
    }
    let mut queue_vectors = Vec::new();
    reserve
        .reserve_vec(&mut queue_vectors, vector_count)
        .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
    for _ in 0..vector_count {
        queue_vectors.push(reader.read_u16()?);
    }
    reader.finish_padded()?;
    let sbdf = PciSbdf::new(segment, bus, device, function)
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue)?;
    let bar_range = GuestMemoryRange::new(bar_start, bar_size)
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue)?;
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

    fn read_u8(&mut self) -> Result<u8, SnapshotV2MultiBlockDeviceGraphDecodeError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated)?;
        self.position = self
            .position
            .checked_add(1)
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
        Ok(value)
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotV2MultiBlockDeviceGraphDecodeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue),
        }
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotV2MultiBlockDeviceGraphDecodeError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotV2MultiBlockDeviceGraphDecodeError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotV2MultiBlockDeviceGraphDecodeError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_array<const LENGTH: usize>(
        &mut self,
    ) -> Result<[u8; LENGTH], SnapshotV2MultiBlockDeviceGraphDecodeError> {
        let source = self.read_bytes(LENGTH)?;
        source
            .try_into()
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated)
    }

    fn read_bytes(
        &mut self,
        length: usize,
    ) -> Result<&'a [u8], SnapshotV2MultiBlockDeviceGraphDecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn read_zeroes(
        &mut self,
        length: usize,
    ) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
        require_zeroes(self.read_bytes(length)?)
    }

    fn read_string<R: ReservePolicy>(
        &mut self,
        length: usize,
        reserve: &mut R,
    ) -> Result<String, SnapshotV2MultiBlockDeviceGraphDecodeError> {
        let bytes = self.read_bytes(length)?;
        let value = std::str::from_utf8(bytes)
            .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidString)?;
        let mut owned = String::new();
        reserve
            .reserve_string(&mut owned, length)
            .map_err(|()| SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation)?;
        owned.push_str(value);
        Ok(owned)
    }

    fn finish_exact(self) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)
        }
    }

    fn finish_padded(self) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
        require_zeroes(
            self.bytes
                .get(self.position..)
                .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated)?,
        )
    }
}

fn section_bytes(
    bytes: &[u8],
    bounds: SectionBounds,
) -> Result<&[u8], SnapshotV2MultiBlockDeviceGraphDecodeError> {
    slice_at(bytes, bounds.offset, bounds.length)
}

fn slice_at(
    bytes: &[u8],
    offset: usize,
    length: usize,
) -> Result<&[u8], SnapshotV2MultiBlockDeviceGraphDecodeError> {
    let end = offset
        .checked_add(length)
        .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)?;
    bytes
        .get(offset..end)
        .ok_or(SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated)
}

fn read_u16_at(
    bytes: &[u8],
    offset: usize,
) -> Result<u16, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    Ok(u16::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u32_at(
    bytes: &[u8],
    offset: usize,
) -> Result<u32, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    Ok(u32::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u64_at(
    bytes: &[u8],
    offset: usize,
) -> Result<u64, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    Ok(u64::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_usize_u64_at(
    bytes: &[u8],
    offset: usize,
) -> Result<usize, SnapshotV2MultiBlockDeviceGraphDecodeError> {
    usize::try_from(read_u64_at(bytes, offset)?)
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure)
}

fn read_array_at<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotV2MultiBlockDeviceGraphDecodeError> {
    slice_at(bytes, offset, LENGTH)?
        .try_into()
        .map_err(|_| SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated)
}

fn require_zeroes(bytes: &[u8]) -> Result<(), SnapshotV2MultiBlockDeviceGraphDecodeError> {
    if bytes.iter().any(|byte| *byte != 0) {
        Err(SnapshotV2MultiBlockDeviceGraphDecodeError::NonzeroReserved)
    } else {
        Ok(())
    }
}
