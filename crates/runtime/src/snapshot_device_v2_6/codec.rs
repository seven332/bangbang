use std::mem::size_of;

use super::*;
use crate::pmem::{PmemRateLimiterConfig, PmemTokenBucketConfig};
use crate::snapshot_device_v2_5::codec as block_codec;
use crate::snapshot_device_v2_5::{
    SnapshotV2MultiBlockDeviceGraphDecodeError, SnapshotV2MultiBlockDeviceGraphEncodeError,
};

const MAGIC: [u8; 8] = *b"BANGD2A\0";
const PROFILE: u16 = 3;
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
const SECTION_KIND_PMEM: u16 = 5;

const TRANSPORT_MMIO: u16 = 1;
const TRANSPORT_PCI: u16 = 2;
const BACKING_REGULAR_FILE: u8 = 1;
const RETRY_NONE: u8 = 0;
const RETRY_IMMEDIATE: u8 = 1;
const RETRY_AFTER: u8 = 2;

const MAX_RECORDS: usize = NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_RECORDS as usize;
const MAX_SECTIONS: usize = MAX_RECORDS * SECTION_COUNT_PER_RECORD;
const MAX_AGGREGATE_STRING_BYTES: usize = MAX_RECORDS
    * const_max(
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_DRIVE_ID_BYTES
            + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_PARTUUID_BYTES
            + NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
        NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES
            + NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_SELECTOR_BYTES,
    );

pub(super) fn encode(
    version: SnapshotFormatVersion,
    graph: &SnapshotV2StorageDeviceGraph,
) -> Result<Vec<u8>, SnapshotV2StorageDeviceGraphEncodeError> {
    encode_with_policy(version, graph, &mut FallibleReserve)
}

pub(super) fn decode(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<SnapshotV2StorageDeviceGraph, SnapshotV2StorageDeviceGraphDecodeError> {
    decode_with_policy(version, bytes, &mut FallibleReserve)
}

pub(super) fn encode_with_policy<R: block_codec::ReservePolicy>(
    version: SnapshotFormatVersion,
    graph: &SnapshotV2StorageDeviceGraph,
    reserve: &mut R,
) -> Result<Vec<u8>, SnapshotV2StorageDeviceGraphEncodeError> {
    if version != NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        return Err(SnapshotV2StorageDeviceGraphEncodeError::UnsupportedVersion);
    }
    validate_graph(graph).map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?;
    let layout = calculate_layout(graph)?;
    let mut output = Vec::new();
    reserve
        .reserve_vec(&mut output, layout.total_length)
        .map_err(|()| SnapshotV2StorageDeviceGraphEncodeError::Allocation)?;
    write_header(&mut output, graph, &layout)?;
    write_record_directory(&mut output, graph)?;
    write_section_directory(&mut output, &layout)?;

    for record_index in 0..graph.record_count() {
        let record = storage_record_at(graph, record_index)
            .ok_or(SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?;
        let first_section = record_index
            .checked_mul(SECTION_COUNT_PER_RECORD)
            .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?;
        let sections = layout
            .sections
            .get(first_section..first_section + SECTION_COUNT_PER_RECORD)
            .ok_or(SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?;
        let [
            config_section,
            device_section,
            common_section,
            transport_section,
        ] = sections
        else {
            return Err(SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph);
        };
        match record {
            StorageRecordRef::Block(record) => {
                block_codec::encode_config(&mut output, record.config(), config_section.length)
                    .map_err(map_block_encode)?;
                block_codec::encode_block(&mut output, record.block(), device_section.length)
                    .map_err(map_block_encode)?;
                block_codec::encode_common(&mut output, record.virtio(), common_section.length)
                    .map_err(map_block_encode)?;
                block_codec::encode_transport(
                    &mut output,
                    record.transport(),
                    transport_section.length,
                )
                .map_err(map_block_encode)?;
            }
            StorageRecordRef::Pmem(record) => {
                encode_pmem_config(&mut output, &record.config, config_section.length)?;
                encode_pmem(&mut output, &record.pmem, device_section.length)?;
                block_codec::encode_common(&mut output, &record.virtio, common_section.length)
                    .map_err(map_block_encode)?;
                block_codec::encode_transport(
                    &mut output,
                    &record.transport,
                    transport_section.length,
                )
                .map_err(map_block_encode)?;
            }
        }
    }
    if output.len() != layout.total_length {
        return Err(SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph);
    }
    Ok(output)
}

pub(super) fn decode_with_policy<R: block_codec::ReservePolicy>(
    version: SnapshotFormatVersion,
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2StorageDeviceGraph, SnapshotV2StorageDeviceGraphDecodeError> {
    let preflight = preflight(version, bytes)?;
    let mut block_records = Vec::new();
    reserve
        .reserve_vec(&mut block_records, preflight.block_count)
        .map_err(|()| SnapshotV2StorageDeviceGraphDecodeError::Allocation)?;
    let mut pmem_records = Vec::new();
    reserve
        .reserve_vec(&mut pmem_records, preflight.pmem_count)
        .map_err(|()| SnapshotV2StorageDeviceGraphDecodeError::Allocation)?;

    for record_index in 0..preflight.record_count {
        let key = *preflight
            .record_keys
            .get(record_index)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
        let first_section = record_index
            .checked_mul(SECTION_COUNT_PER_RECORD)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
        let sections = preflight
            .sections
            .get(first_section..first_section + SECTION_COUNT_PER_RECORD)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
        let [
            config_section,
            device_section,
            common_section,
            transport_section,
        ] = sections
        else {
            return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure);
        };
        let common = block_codec::decode_common(section_bytes(bytes, *common_section)?, reserve)
            .map_err(map_block_decode)?;
        let transport_bytes = section_bytes(bytes, *transport_section)?;
        let transport = match preflight.transport_kind {
            SnapshotV2DeviceTransportKind::Mmio => SnapshotV2DeviceTransport::Mmio(
                block_codec::decode_mmio(transport_bytes).map_err(map_block_decode)?,
            ),
            SnapshotV2DeviceTransportKind::Pci => SnapshotV2DeviceTransport::Pci(
                block_codec::decode_pci(transport_bytes, reserve).map_err(map_block_decode)?,
            ),
        };
        match key.kind() {
            DEVICE_KIND_BLOCK => {
                let config =
                    block_codec::decode_config(section_bytes(bytes, *config_section)?, reserve)
                        .map_err(map_block_decode)?;
                let block = block_codec::decode_block(section_bytes(bytes, *device_section)?)
                    .map_err(map_block_decode)?;
                block_records.push(SnapshotV2MultiBlockDeviceRecord::from_parts(
                    key, config, block, common, transport,
                ));
            }
            DEVICE_KIND_PMEM => {
                let config = decode_pmem_config(section_bytes(bytes, *config_section)?, reserve)?;
                let pmem = decode_pmem(section_bytes(bytes, *device_section)?)?;
                pmem_records.push(SnapshotV2PmemDeviceRecord::from_decoded_parts(
                    key, config, pmem, common, transport,
                ));
            }
            _ => return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure),
        }
    }
    SnapshotV2StorageDeviceGraph::try_from_parts(
        preflight.root_key,
        preflight.transport_kind,
        block_records,
        pmem_records,
    )
    .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidGraph)
}

struct FallibleReserve;

impl block_codec::ReservePolicy for FallibleReserve {
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

fn calculate_layout(
    graph: &SnapshotV2StorageDeviceGraph,
) -> Result<EncodeLayout, SnapshotV2StorageDeviceGraphEncodeError> {
    let record_count = graph.record_count();
    let section_count = record_count
        .checked_mul(SECTION_COUNT_PER_RECORD)
        .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?;
    let record_directory_offset = NATIVE_V2_STORAGE_DEVICE_GRAPH_HEADER_BYTES;
    let section_directory_offset = record_directory_offset
        .checked_add(
            record_count
                .checked_mul(NATIVE_V2_STORAGE_DEVICE_GRAPH_RECORD_ENTRY_BYTES)
                .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
        )
        .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?;
    let payload_offset = section_directory_offset
        .checked_add(
            section_count
                .checked_mul(NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES)
                .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
        )
        .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?;
    let mut sections = [SectionLayout::EMPTY; MAX_SECTIONS];
    let mut offset = payload_offset;
    for record_index in 0..record_count {
        let record = storage_record_at(graph, record_index)
            .ok_or(SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?;
        let (kinds, lengths) = match record {
            StorageRecordRef::Block(record) => (
                [
                    SECTION_KIND_CONFIG,
                    SECTION_KIND_BLOCK,
                    SECTION_KIND_COMMON,
                    SECTION_KIND_TRANSPORT,
                ],
                [
                    block_config_length(record)?,
                    BLOCK_SECTION_BYTES,
                    common_length(record.virtio())?,
                    transport_length(record.transport()),
                ],
            ),
            StorageRecordRef::Pmem(record) => (
                [
                    SECTION_KIND_CONFIG,
                    SECTION_KIND_PMEM,
                    SECTION_KIND_COMMON,
                    SECTION_KIND_TRANSPORT,
                ],
                [
                    pmem_config_length(&record.config)?,
                    PMEM_SECTION_BYTES,
                    common_length(&record.virtio)?,
                    transport_length(&record.transport),
                ],
            ),
        };
        for (section_in_record, (kind, length)) in kinds.into_iter().zip(lengths).enumerate() {
            let section_index = record_index
                .checked_mul(SECTION_COUNT_PER_RECORD)
                .and_then(|value| value.checked_add(section_in_record))
                .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?;
            *sections
                .get_mut(section_index)
                .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)? = SectionLayout {
                record_index: u32::try_from(record_index)
                    .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
                kind,
                offset,
                length,
            };
            offset = offset
                .checked_add(length)
                .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?;
        }
    }
    if offset > NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_BYTES {
        return Err(SnapshotV2StorageDeviceGraphEncodeError::TooLarge);
    }
    Ok(EncodeLayout {
        record_directory_offset,
        section_directory_offset,
        payload_offset,
        total_length: offset,
        section_count,
        sections,
    })
}

fn block_config_length(
    record: &SnapshotV2MultiBlockDeviceRecord,
) -> Result<usize, SnapshotV2StorageDeviceGraphEncodeError> {
    CONFIG_FIXED_BYTES
        .checked_add(record.config().drive_id().len())
        .and_then(|value| value.checked_add(record.config().partuuid().map_or(0, str::len)))
        .and_then(|value| value.checked_add(record.config().selector().len()))
        .and_then(aligned_length)
        .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)
}

fn pmem_config_length(
    config: &SnapshotV2PmemConfig,
) -> Result<usize, SnapshotV2StorageDeviceGraphEncodeError> {
    PMEM_CONFIG_FIXED_BYTES
        .checked_add(config.pmem_id.len())
        .and_then(|value| value.checked_add(config.selector.len()))
        .and_then(aligned_length)
        .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)
}

fn common_length(
    common: &SnapshotV2VirtioState,
) -> Result<usize, SnapshotV2StorageDeviceGraphEncodeError> {
    let semantic = COMMON_FIXED_BYTES
        .checked_add(
            common
                .queues()
                .len()
                .checked_mul(COMMON_QUEUE_BYTES)
                .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
        )
        .and_then(|value| {
            value.checked_add(
                common
                    .pending_notifications()
                    .len()
                    .checked_mul(size_of::<u16>())?,
            )
        })
        .and_then(|value| {
            value.checked_add(
                common
                    .interrupt_intents()
                    .len()
                    .checked_mul(size_of::<u32>())?,
            )
        })
        .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?;
    aligned_length(semantic).ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)
}

const fn transport_length(transport: &SnapshotV2DeviceTransport) -> usize {
    match transport {
        SnapshotV2DeviceTransport::Mmio(_) => MMIO_SECTION_BYTES,
        SnapshotV2DeviceTransport::Pci(_) => PCI_SECTION_BYTES,
    }
}

fn write_header(
    output: &mut Vec<u8>,
    graph: &SnapshotV2StorageDeviceGraph,
    layout: &EncodeLayout,
) -> Result<(), SnapshotV2StorageDeviceGraphEncodeError> {
    write_bytes(output, &MAGIC);
    write_u16(
        output,
        u16::try_from(NATIVE_V2_STORAGE_DEVICE_GRAPH_HEADER_BYTES)
            .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
    );
    write_u16(output, PROFILE);
    write_u16(output, transport_tag(graph.transport_kind));
    write_u16(
        output,
        u16::try_from(graph.record_count())
            .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
    );
    write_u16(
        output,
        u16::try_from(layout.section_count)
            .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
    );
    write_u16(output, 0);
    write_u32(output, FLAGS);
    write_u64(
        output,
        u64::try_from(layout.total_length)
            .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
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
            u64::try_from(offset).map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
        );
    }
    Ok(())
}

fn write_record_directory(
    output: &mut Vec<u8>,
    graph: &SnapshotV2StorageDeviceGraph,
) -> Result<(), SnapshotV2StorageDeviceGraphEncodeError> {
    for index in 0..graph.record_count() {
        let record = storage_record_at(graph, index)
            .ok_or(SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?;
        let key = match record {
            StorageRecordRef::Block(record) => record.key(),
            StorageRecordRef::Pmem(record) => record.key,
        };
        write_u32(output, key.kind());
        write_u32(output, key.instance());
        write_u32(
            output,
            u32::try_from(
                index
                    .checked_mul(SECTION_COUNT_PER_RECORD)
                    .ok_or(SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
            )
            .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
        );
        write_u32(output, SECTION_COUNT_U32);
        write_zeroes(output, 16);
    }
    Ok(())
}

fn write_section_directory(
    output: &mut Vec<u8>,
    layout: &EncodeLayout,
) -> Result<(), SnapshotV2StorageDeviceGraphEncodeError> {
    for section in layout
        .sections
        .get(..layout.section_count)
        .ok_or(SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?
    {
        write_u32(output, section.record_index);
        write_u16(output, section.kind);
        write_u16(output, 0);
        write_u64(output, 0);
        write_u64(
            output,
            u64::try_from(section.offset)
                .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
        );
        write_u64(
            output,
            u64::try_from(section.length)
                .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::TooLarge)?,
        );
    }
    Ok(())
}

fn encode_pmem_config(
    output: &mut Vec<u8>,
    config: &SnapshotV2PmemConfig,
    section_length: usize,
) -> Result<(), SnapshotV2StorageDeviceGraphEncodeError> {
    let start = output.len();
    write_bool(output, config.is_root);
    write_bool(output, config.is_read_only);
    write_bool(
        output,
        config
            .rate_limiter
            .and_then(PmemRateLimiterConfig::bandwidth)
            .is_some(),
    );
    write_bool(
        output,
        config
            .rate_limiter
            .and_then(PmemRateLimiterConfig::ops)
            .is_some(),
    );
    write_u8(output, BACKING_REGULAR_FILE);
    write_zeroes(output, 3);
    write_u16(
        output,
        u16::try_from(config.pmem_id.len())
            .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u16(
        output,
        u16::try_from(config.selector.len())
            .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?,
    );
    write_u32(output, 0);
    encode_bucket_config(
        output,
        config
            .rate_limiter
            .and_then(PmemRateLimiterConfig::bandwidth),
    );
    encode_bucket_config(
        output,
        config.rate_limiter.and_then(PmemRateLimiterConfig::ops),
    );
    write_bytes(output, config.pmem_id.as_bytes());
    write_bytes(output, config.selector.as_bytes());
    pad_section(output, start, section_length)
}

fn encode_bucket_config(output: &mut Vec<u8>, bucket: Option<PmemTokenBucketConfig>) {
    let (size, burst, refill_time, burst_present) = bucket.map_or((0, 0, 0, false), |bucket| {
        (
            bucket.size(),
            bucket.one_time_burst().unwrap_or(0),
            bucket.refill_time(),
            bucket.one_time_burst().is_some(),
        )
    });
    write_u64(output, size);
    write_u64(output, burst);
    write_u64(output, refill_time);
    write_bool(output, burst_present);
    write_zeroes(output, 7);
}

fn encode_pmem(
    output: &mut Vec<u8>,
    pmem: &SnapshotV2PmemState,
    section_length: usize,
) -> Result<(), SnapshotV2StorageDeviceGraphEncodeError> {
    let start = output.len();
    write_u16(
        output,
        u16::try_from(VIRTIO_PMEM_CONFIG_SPACE_SIZE)
            .map_err(|_| SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?,
    );
    write_bool(output, pmem.active_queue.is_some());
    write_bool(output, pmem.limiter.bandwidth.is_some());
    write_bool(output, pmem.limiter.ops.is_some());
    write_bool(output, pmem.pending_rate_limited_queue);
    let (retry_tag, retry_nanos) = match pmem.retry {
        StorageRetryState::None => (RETRY_NONE, 0),
        StorageRetryState::Immediate => (RETRY_IMMEDIATE, 0),
        StorageRetryState::After { remaining_nanos } => (RETRY_AFTER, remaining_nanos),
    };
    write_u8(output, retry_tag);
    write_u8(output, 0);
    write_u64(output, pmem.file_bytes);
    write_u64(output, pmem.mapped_bytes);
    write_u64(output, pmem.guest_range.start().raw_value());
    write_u64(output, pmem.guest_range.size());
    write_u64(output, pmem.config_space.start());
    write_u64(output, pmem.config_space.size());
    let (next_available, next_used) = pmem
        .active_queue
        .map_or((0, 0), |queue| (queue.next_available(), queue.next_used()));
    write_u16(output, next_available);
    write_u16(output, next_used);
    write_u32(output, 0);
    encode_bucket_state(output, pmem.limiter.bandwidth);
    encode_bucket_state(output, pmem.limiter.ops);
    write_u64(output, retry_nanos);
    write_zeroes(output, 8);
    pad_section(output, start, section_length)
}

fn encode_bucket_state(output: &mut Vec<u8>, bucket: Option<SnapshotV2PmemBucketState>) {
    let (budget, remaining_burst, age_nanos) = bucket.map_or((0, 0, 0), |bucket| {
        (bucket.budget, bucket.remaining_burst, bucket.age_nanos)
    });
    write_u64(output, budget);
    write_u64(output, remaining_burst);
    write_u64(output, age_nanos);
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
    block_count: usize,
    pmem_count: usize,
    transport_kind: SnapshotV2DeviceTransportKind,
    root_key: Option<SnapshotV2DeviceKey>,
    record_keys: [SnapshotV2DeviceKey; MAX_RECORDS],
    sections: [SectionBounds; MAX_SECTIONS],
}

fn preflight(
    version: SnapshotFormatVersion,
    bytes: &[u8],
) -> Result<Preflight, SnapshotV2StorageDeviceGraphDecodeError> {
    if version != NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::UnsupportedVersion);
    }
    if bytes.len() < NATIVE_V2_STORAGE_DEVICE_GRAPH_HEADER_BYTES {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::TooSmall);
    }
    if bytes.len() > NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_BYTES {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::TooLarge);
    }
    if read_array_at::<8>(bytes, HEADER_MAGIC_OFFSET)? != MAGIC {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidMagic);
    }
    let transport_kind = match read_u16_at(bytes, HEADER_TRANSPORT_OFFSET)? {
        TRANSPORT_MMIO => SnapshotV2DeviceTransportKind::Mmio,
        TRANSPORT_PCI => SnapshotV2DeviceTransportKind::Pci,
        _ => return Err(SnapshotV2StorageDeviceGraphDecodeError::UnsupportedProfile),
    };
    let record_count = usize::from(read_u16_at(bytes, HEADER_RECORD_COUNT_OFFSET)?);
    let section_count = usize::from(read_u16_at(bytes, HEADER_SECTION_COUNT_OFFSET)?);
    if usize::from(read_u16_at(bytes, HEADER_BYTES_OFFSET)?)
        != NATIVE_V2_STORAGE_DEVICE_GRAPH_HEADER_BYTES
        || read_u16_at(bytes, HEADER_PROFILE_OFFSET)? != PROFILE
        || record_count == 0
        || record_count > MAX_RECORDS
        || section_count
            != record_count
                .checked_mul(SECTION_COUNT_PER_RECORD)
                .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?
        || read_u32_at(bytes, HEADER_FLAGS_OFFSET)? != FLAGS
    {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::UnsupportedProfile);
    }
    if read_u16_at(bytes, HEADER_RESERVED_OFFSET)? != 0 {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::NonzeroReserved);
    }
    let total_length = read_usize_u64_at(bytes, HEADER_TOTAL_LENGTH_OFFSET)?;
    let record_directory_offset = NATIVE_V2_STORAGE_DEVICE_GRAPH_HEADER_BYTES;
    let section_directory_offset = record_directory_offset
        .checked_add(
            record_count
                .checked_mul(NATIVE_V2_STORAGE_DEVICE_GRAPH_RECORD_ENTRY_BYTES)
                .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?,
        )
        .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
    let payload_offset = section_directory_offset
        .checked_add(
            section_count
                .checked_mul(NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES)
                .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?,
        )
        .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
    if total_length != bytes.len()
        || read_usize_u64_at(bytes, HEADER_RECORD_DIRECTORY_OFFSET_OFFSET)?
            != record_directory_offset
        || read_usize_u64_at(bytes, HEADER_SECTION_DIRECTORY_OFFSET_OFFSET)?
            != section_directory_offset
        || read_usize_u64_at(bytes, HEADER_PAYLOAD_OFFSET_OFFSET)? != payload_offset
    {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure);
    }
    let root_key = match (
        read_u32_at(bytes, HEADER_ROOT_KIND_OFFSET)?,
        read_u32_at(bytes, HEADER_ROOT_INSTANCE_OFFSET)?,
    ) {
        (0, 0) => None,
        (DEVICE_KIND_BLOCK, 0) => Some(SnapshotV2DeviceKey::block(0)),
        (DEVICE_KIND_PMEM, 0) => Some(SnapshotV2DeviceKey::pmem(0)),
        _ => return Err(SnapshotV2StorageDeviceGraphDecodeError::UnsupportedProfile),
    };

    let mut record_keys = [SnapshotV2DeviceKey::block(0); MAX_RECORDS];
    let mut block_count = 0_usize;
    let mut pmem_count = 0_usize;
    let mut saw_pmem = false;
    for record_index in 0..record_count {
        let offset = record_directory_offset
            .checked_add(
                record_index
                    .checked_mul(NATIVE_V2_STORAGE_DEVICE_GRAPH_RECORD_ENTRY_BYTES)
                    .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?,
            )
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
        let entry = slice_at(
            bytes,
            offset,
            NATIVE_V2_STORAGE_DEVICE_GRAPH_RECORD_ENTRY_BYTES,
        )?;
        let kind = read_u32_at(entry, RECORD_KIND_OFFSET)?;
        let key = match kind {
            DEVICE_KIND_BLOCK if !saw_pmem => {
                let key = SnapshotV2DeviceKey::block(
                    u32::try_from(block_count)
                        .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?,
                );
                block_count += 1;
                key
            }
            DEVICE_KIND_PMEM => {
                saw_pmem = true;
                let key = SnapshotV2DeviceKey::pmem(
                    u32::try_from(pmem_count)
                        .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?,
                );
                pmem_count += 1;
                key
            }
            _ => return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure),
        };
        if read_u32_at(entry, RECORD_INSTANCE_OFFSET)? != key.instance()
            || read_u32_at(entry, RECORD_FIRST_SECTION_OFFSET)?
                != u32::try_from(
                    record_index
                        .checked_mul(SECTION_COUNT_PER_RECORD)
                        .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?,
                )
                .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?
            || read_u32_at(entry, RECORD_SECTION_COUNT_OFFSET)? != SECTION_COUNT_U32
        {
            return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure);
        }
        require_zeroes(
            entry
                .get(RECORD_RESERVED_OFFSET..)
                .ok_or(SnapshotV2StorageDeviceGraphDecodeError::Truncated)?,
        )?;
        *record_keys
            .get_mut(record_index)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)? = key;
    }

    let mut sections = [SectionBounds::EMPTY; MAX_SECTIONS];
    let mut expected_payload_offset = payload_offset;
    let mut aggregate_string_bytes = 0_usize;
    for section_index in 0..section_count {
        let entry_offset = section_directory_offset
            .checked_add(
                section_index
                    .checked_mul(NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES)
                    .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?,
            )
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
        let entry = slice_at(
            bytes,
            entry_offset,
            NATIVE_V2_STORAGE_DEVICE_GRAPH_SECTION_ENTRY_BYTES,
        )?;
        let record_index = section_index / SECTION_COUNT_PER_RECORD;
        let key = *record_keys
            .get(record_index)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
        let expected_kind = match (key.kind(), section_index % SECTION_COUNT_PER_RECORD) {
            (_, 0) => SECTION_KIND_CONFIG,
            (DEVICE_KIND_BLOCK, 1) => SECTION_KIND_BLOCK,
            (DEVICE_KIND_PMEM, 1) => SECTION_KIND_PMEM,
            (_, 2) => SECTION_KIND_COMMON,
            (_, 3) => SECTION_KIND_TRANSPORT,
            _ => return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure),
        };
        if read_u32_at(entry, SECTION_RECORD_INDEX_OFFSET)?
            != u32::try_from(record_index)
                .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?
            || read_u16_at(entry, SECTION_KIND_OFFSET)? != expected_kind
            || read_u16_at(entry, SECTION_FLAGS_OFFSET)? != 0
            || read_u64_at(entry, SECTION_RESERVED_OFFSET)? != 0
        {
            return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure);
        }
        let offset = read_usize_u64_at(entry, SECTION_PAYLOAD_OFFSET)?;
        let length = read_usize_u64_at(entry, SECTION_LENGTH_OFFSET)?;
        let end = offset
            .checked_add(length)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
        if offset != expected_payload_offset
            || length == 0
            || !offset.is_multiple_of(ALIGNMENT)
            || !length.is_multiple_of(ALIGNMENT)
            || end > bytes.len()
        {
            return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure);
        }
        let bounds = SectionBounds { offset, length };
        let section = section_bytes(bytes, bounds)?;
        match (key.kind(), expected_kind) {
            (DEVICE_KIND_BLOCK, SECTION_KIND_CONFIG) => {
                aggregate_string_bytes = aggregate_string_bytes
                    .checked_add(block_codec::preflight_config(section).map_err(map_block_decode)?)
                    .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
            }
            (DEVICE_KIND_PMEM, SECTION_KIND_CONFIG) => {
                aggregate_string_bytes = aggregate_string_bytes
                    .checked_add(preflight_pmem_config(section)?)
                    .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
            }
            (DEVICE_KIND_BLOCK, SECTION_KIND_BLOCK) => {
                block_codec::preflight_block(section).map_err(map_block_decode)?;
            }
            (DEVICE_KIND_PMEM, SECTION_KIND_PMEM) => preflight_pmem(section)?,
            (_, SECTION_KIND_COMMON) => {
                block_codec::preflight_common(section).map_err(map_block_decode)?;
            }
            (_, SECTION_KIND_TRANSPORT) => match transport_kind {
                SnapshotV2DeviceTransportKind::Mmio => {
                    block_codec::preflight_mmio(section).map_err(map_block_decode)?;
                }
                SnapshotV2DeviceTransportKind::Pci => {
                    block_codec::preflight_pci(section).map_err(map_block_decode)?;
                }
            },
            _ => return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure),
        }
        if aggregate_string_bytes > MAX_AGGREGATE_STRING_BYTES {
            return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidString);
        }
        *sections
            .get_mut(section_index)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)? = bounds;
        expected_payload_offset = end;
    }
    if expected_payload_offset != bytes.len() {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure);
    }
    Ok(Preflight {
        record_count,
        block_count,
        pmem_count,
        transport_kind,
        root_key,
        record_keys,
        sections,
    })
}

fn preflight_pmem_config(bytes: &[u8]) -> Result<usize, SnapshotV2StorageDeviceGraphDecodeError> {
    if bytes.len() < PMEM_CONFIG_FIXED_BYTES {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::Truncated);
    }
    let mut reader = Reader::new(bytes);
    reader.read_bool()?;
    reader.read_bool()?;
    let bandwidth_present = reader.read_bool()?;
    let ops_present = reader.read_bool()?;
    if reader.read_u8()? != BACKING_REGULAR_FILE {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue);
    }
    reader.read_zeroes(3)?;
    let pmem_id_len = usize::from(reader.read_u16()?);
    let selector_len = usize::from(reader.read_u16()?);
    reader.read_zeroes(4)?;
    preflight_bucket_config(&mut reader, bandwidth_present)?;
    preflight_bucket_config(&mut reader, ops_present)?;
    if pmem_id_len == 0
        || pmem_id_len > NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_PMEM_ID_BYTES
        || selector_len == 0
        || selector_len > NATIVE_V2_STORAGE_DEVICE_GRAPH_MAX_SELECTOR_BYTES
    {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidString);
    }
    let semantic_length = PMEM_CONFIG_FIXED_BYTES
        .checked_add(pmem_id_len)
        .and_then(|value| value.checked_add(selector_len))
        .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
    if aligned_length(semantic_length) != Some(bytes.len()) {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure);
    }
    let pmem_id = std::str::from_utf8(reader.read_bytes(pmem_id_len)?)
        .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidString)?;
    std::str::from_utf8(reader.read_bytes(selector_len)?)
        .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidString)?;
    if !pmem_id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidString);
    }
    reader.finish_padded()?;
    pmem_id_len
        .checked_add(selector_len)
        .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)
}

fn preflight_bucket_config(
    reader: &mut Reader<'_>,
    present: bool,
) -> Result<(), SnapshotV2StorageDeviceGraphDecodeError> {
    let size = reader.read_u64()?;
    let burst = reader.read_u64()?;
    let refill_time = reader.read_u64()?;
    let burst_present = reader.read_bool()?;
    reader.read_zeroes(7)?;
    if !present {
        if size != 0 || burst != 0 || refill_time != 0 || burst_present {
            return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue);
        }
        return Ok(());
    }
    let config = PmemTokenBucketConfig::new(size, burst_present.then_some(burst), refill_time);
    if (!burst_present && burst != 0) || !pmem_token_bucket_is_enabled(config) {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue);
    }
    Ok(())
}

fn preflight_pmem(bytes: &[u8]) -> Result<(), SnapshotV2StorageDeviceGraphDecodeError> {
    if bytes.len() != PMEM_SECTION_BYTES {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure);
    }
    let mut reader = Reader::new(bytes);
    if usize::from(reader.read_u16()?) != VIRTIO_PMEM_CONFIG_SPACE_SIZE {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue);
    }
    let active = reader.read_bool()?;
    let bandwidth = reader.read_bool()?;
    let ops = reader.read_bool()?;
    reader.read_bool()?;
    let retry = reader.read_u8()?;
    reader.read_zeroes(1)?;
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_u64()?;
    reader.read_u64()?;
    let next_available = reader.read_u16()?;
    let next_used = reader.read_u16()?;
    reader.read_zeroes(4)?;
    preflight_bucket_state(&mut reader, bandwidth)?;
    preflight_bucket_state(&mut reader, ops)?;
    let retry_nanos = reader.read_u64()?;
    reader.read_zeroes(8)?;
    if (!active && (next_available != 0 || next_used != 0))
        || !matches!(
            (retry, retry_nanos),
            (RETRY_NONE | RETRY_IMMEDIATE, 0) | (RETRY_AFTER, 1..=u64::MAX)
        )
    {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue);
    }
    reader.finish_exact()
}

fn preflight_bucket_state(
    reader: &mut Reader<'_>,
    present: bool,
) -> Result<(), SnapshotV2StorageDeviceGraphDecodeError> {
    let budget = reader.read_u64()?;
    let burst = reader.read_u64()?;
    let age = reader.read_u64()?;
    if !present && (budget != 0 || burst != 0 || age != 0) {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue);
    }
    Ok(())
}

fn decode_pmem_config<R: block_codec::ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2PmemConfig, SnapshotV2StorageDeviceGraphDecodeError> {
    let mut reader = Reader::new(bytes);
    let is_root = reader.read_bool()?;
    let is_read_only = reader.read_bool()?;
    let bandwidth_present = reader.read_bool()?;
    let ops_present = reader.read_bool()?;
    if reader.read_u8()? != BACKING_REGULAR_FILE {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue);
    }
    reader.read_zeroes(3)?;
    let pmem_id_len = usize::from(reader.read_u16()?);
    let selector_len = usize::from(reader.read_u16()?);
    reader.read_zeroes(4)?;
    let bandwidth = decode_bucket_config(&mut reader, bandwidth_present)?;
    let ops = decode_bucket_config(&mut reader, ops_present)?;
    let pmem_id = reader.read_string(pmem_id_len, reserve)?;
    let selector = reader.read_string(selector_len, reserve)?;
    reader.finish_padded()?;
    Ok(SnapshotV2PmemConfig {
        pmem_id,
        is_root,
        is_read_only,
        rate_limiter: (bandwidth.is_some() || ops.is_some())
            .then_some(PmemRateLimiterConfig::new(bandwidth, ops)),
        selector,
    })
}

fn decode_bucket_config(
    reader: &mut Reader<'_>,
    present: bool,
) -> Result<Option<PmemTokenBucketConfig>, SnapshotV2StorageDeviceGraphDecodeError> {
    let size = reader.read_u64()?;
    let burst = reader.read_u64()?;
    let refill_time = reader.read_u64()?;
    let burst_present = reader.read_bool()?;
    reader.read_zeroes(7)?;
    Ok(present.then_some(PmemTokenBucketConfig::new(
        size,
        burst_present.then_some(burst),
        refill_time,
    )))
}

fn decode_pmem(
    bytes: &[u8],
) -> Result<SnapshotV2PmemState, SnapshotV2StorageDeviceGraphDecodeError> {
    let mut reader = Reader::new(bytes);
    if usize::from(reader.read_u16()?) != VIRTIO_PMEM_CONFIG_SPACE_SIZE {
        return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue);
    }
    let active = reader.read_bool()?;
    let bandwidth = reader.read_bool()?;
    let ops = reader.read_bool()?;
    let pending_rate_limited_queue = reader.read_bool()?;
    let retry_tag = reader.read_u8()?;
    reader.read_zeroes(1)?;
    let file_bytes = reader.read_u64()?;
    let mapped_bytes = reader.read_u64()?;
    let guest_start = reader.read_u64()?;
    let guest_size = reader.read_u64()?;
    let config_start = reader.read_u64()?;
    let config_size = reader.read_u64()?;
    let next_available = reader.read_u16()?;
    let next_used = reader.read_u16()?;
    reader.read_zeroes(4)?;
    let bandwidth = decode_bucket_state(&mut reader, bandwidth)?;
    let ops = decode_bucket_state(&mut reader, ops)?;
    let retry_nanos = reader.read_u64()?;
    reader.read_zeroes(8)?;
    reader.finish_exact()?;
    let guest_range =
        GuestMemoryRange::new(crate::memory::GuestAddress::new(guest_start), guest_size)
            .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidValue)?;
    let retry = match (retry_tag, retry_nanos) {
        (RETRY_NONE, 0) => StorageRetryState::None,
        (RETRY_IMMEDIATE, 0) => StorageRetryState::Immediate,
        (RETRY_AFTER, remaining_nanos) if remaining_nanos != 0 => {
            StorageRetryState::After { remaining_nanos }
        }
        _ => return Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue),
    };
    Ok(SnapshotV2PmemState {
        file_bytes,
        mapped_bytes,
        guest_range,
        config_space: VirtioPmemConfigSpace::new(config_start, config_size),
        active_queue: active.then_some(VirtioPmemQueueState::new(next_available, next_used)),
        limiter: SnapshotV2PmemLimiterState::new(bandwidth, ops),
        pending_rate_limited_queue,
        retry,
    })
}

fn decode_bucket_state(
    reader: &mut Reader<'_>,
    present: bool,
) -> Result<Option<SnapshotV2PmemBucketState>, SnapshotV2StorageDeviceGraphDecodeError> {
    let state =
        SnapshotV2PmemBucketState::new(reader.read_u64()?, reader.read_u64()?, reader.read_u64()?);
    Ok(present.then_some(state))
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, SnapshotV2StorageDeviceGraphDecodeError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::Truncated)?;
        self.position = self
            .position
            .checked_add(1)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
        Ok(value)
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotV2StorageDeviceGraphDecodeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidValue),
        }
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotV2StorageDeviceGraphDecodeError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotV2StorageDeviceGraphDecodeError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_array<const LENGTH: usize>(
        &mut self,
    ) -> Result<[u8; LENGTH], SnapshotV2StorageDeviceGraphDecodeError> {
        self.read_bytes(LENGTH)?
            .try_into()
            .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::Truncated)
    }

    fn read_bytes(
        &mut self,
        length: usize,
    ) -> Result<&'a [u8], SnapshotV2StorageDeviceGraphDecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(SnapshotV2StorageDeviceGraphDecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn read_zeroes(
        &mut self,
        length: usize,
    ) -> Result<(), SnapshotV2StorageDeviceGraphDecodeError> {
        require_zeroes(self.read_bytes(length)?)
    }

    fn read_string<R: block_codec::ReservePolicy>(
        &mut self,
        length: usize,
        reserve: &mut R,
    ) -> Result<String, SnapshotV2StorageDeviceGraphDecodeError> {
        let bytes = self.read_bytes(length)?;
        let value = std::str::from_utf8(bytes)
            .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidString)?;
        let mut owned = String::new();
        reserve
            .reserve_string(&mut owned, length)
            .map_err(|()| SnapshotV2StorageDeviceGraphDecodeError::Allocation)?;
        owned.push_str(value);
        Ok(owned)
    }

    fn finish_exact(self) -> Result<(), SnapshotV2StorageDeviceGraphDecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)
        }
    }

    fn finish_padded(self) -> Result<(), SnapshotV2StorageDeviceGraphDecodeError> {
        require_zeroes(
            self.bytes
                .get(self.position..)
                .ok_or(SnapshotV2StorageDeviceGraphDecodeError::Truncated)?,
        )
    }
}

fn section_bytes(
    bytes: &[u8],
    bounds: SectionBounds,
) -> Result<&[u8], SnapshotV2StorageDeviceGraphDecodeError> {
    slice_at(bytes, bounds.offset, bounds.length)
}

fn slice_at(
    bytes: &[u8],
    offset: usize,
    length: usize,
) -> Result<&[u8], SnapshotV2StorageDeviceGraphDecodeError> {
    let end = offset
        .checked_add(length)
        .ok_or(SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)?;
    bytes
        .get(offset..end)
        .ok_or(SnapshotV2StorageDeviceGraphDecodeError::Truncated)
}

fn read_u16_at(
    bytes: &[u8],
    offset: usize,
) -> Result<u16, SnapshotV2StorageDeviceGraphDecodeError> {
    Ok(u16::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u32_at(
    bytes: &[u8],
    offset: usize,
) -> Result<u32, SnapshotV2StorageDeviceGraphDecodeError> {
    Ok(u32::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_u64_at(
    bytes: &[u8],
    offset: usize,
) -> Result<u64, SnapshotV2StorageDeviceGraphDecodeError> {
    Ok(u64::from_le_bytes(read_array_at(bytes, offset)?))
}

fn read_usize_u64_at(
    bytes: &[u8],
    offset: usize,
) -> Result<usize, SnapshotV2StorageDeviceGraphDecodeError> {
    usize::try_from(read_u64_at(bytes, offset)?)
        .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::InvalidStructure)
}

fn read_array_at<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotV2StorageDeviceGraphDecodeError> {
    slice_at(bytes, offset, LENGTH)?
        .try_into()
        .map_err(|_| SnapshotV2StorageDeviceGraphDecodeError::Truncated)
}

fn require_zeroes(bytes: &[u8]) -> Result<(), SnapshotV2StorageDeviceGraphDecodeError> {
    if bytes.iter().any(|byte| *byte != 0) {
        Err(SnapshotV2StorageDeviceGraphDecodeError::NonzeroReserved)
    } else {
        Ok(())
    }
}

fn aligned_length(value: usize) -> Option<usize> {
    value
        .checked_add(ALIGNMENT - 1)
        .map(|value| value & !(ALIGNMENT - 1))
}

fn pad_section(
    output: &mut Vec<u8>,
    start: usize,
    section_length: usize,
) -> Result<(), SnapshotV2StorageDeviceGraphEncodeError> {
    let written = output
        .len()
        .checked_sub(start)
        .ok_or(SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?;
    let padding = section_length
        .checked_sub(written)
        .ok_or(SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph)?;
    write_zeroes(output, padding);
    Ok(())
}

const fn transport_tag(kind: SnapshotV2DeviceTransportKind) -> u16 {
    match kind {
        SnapshotV2DeviceTransportKind::Mmio => TRANSPORT_MMIO,
        SnapshotV2DeviceTransportKind::Pci => TRANSPORT_PCI,
    }
}

fn write_bool(output: &mut Vec<u8>, value: bool) {
    write_u8(output, u8::from(value));
}

fn write_u8(output: &mut Vec<u8>, value: u8) {
    output.push(value);
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

fn write_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(value);
}

fn write_zeroes(output: &mut Vec<u8>, count: usize) {
    output.resize(output.len() + count, 0);
}

fn map_block_encode(
    error: SnapshotV2MultiBlockDeviceGraphEncodeError,
) -> SnapshotV2StorageDeviceGraphEncodeError {
    match error {
        SnapshotV2MultiBlockDeviceGraphEncodeError::Allocation => {
            SnapshotV2StorageDeviceGraphEncodeError::Allocation
        }
        SnapshotV2MultiBlockDeviceGraphEncodeError::TooLarge => {
            SnapshotV2StorageDeviceGraphEncodeError::TooLarge
        }
        SnapshotV2MultiBlockDeviceGraphEncodeError::UnsupportedVersion
        | SnapshotV2MultiBlockDeviceGraphEncodeError::InvalidGraph => {
            SnapshotV2StorageDeviceGraphEncodeError::InvalidGraph
        }
    }
}

fn map_block_decode(
    error: SnapshotV2MultiBlockDeviceGraphDecodeError,
) -> SnapshotV2StorageDeviceGraphDecodeError {
    match error {
        SnapshotV2MultiBlockDeviceGraphDecodeError::Allocation => {
            SnapshotV2StorageDeviceGraphDecodeError::Allocation
        }
        SnapshotV2MultiBlockDeviceGraphDecodeError::Truncated
        | SnapshotV2MultiBlockDeviceGraphDecodeError::TooSmall => {
            SnapshotV2StorageDeviceGraphDecodeError::Truncated
        }
        SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidString => {
            SnapshotV2StorageDeviceGraphDecodeError::InvalidString
        }
        SnapshotV2MultiBlockDeviceGraphDecodeError::NonzeroReserved => {
            SnapshotV2StorageDeviceGraphDecodeError::NonzeroReserved
        }
        SnapshotV2MultiBlockDeviceGraphDecodeError::TooLarge => {
            SnapshotV2StorageDeviceGraphDecodeError::TooLarge
        }
        SnapshotV2MultiBlockDeviceGraphDecodeError::UnsupportedVersion
        | SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidMagic
        | SnapshotV2MultiBlockDeviceGraphDecodeError::UnsupportedProfile
        | SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidStructure
        | SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidValue
        | SnapshotV2MultiBlockDeviceGraphDecodeError::InvalidGraph => {
            SnapshotV2StorageDeviceGraphDecodeError::InvalidValue
        }
    }
}
