use std::mem::size_of;

use crc64::crc64;

use crate::memory::{GuestAddress, GuestMemoryRange};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::snapshot_memory_v2::{
    NATIVE_V2_MEMORY_ALIGNMENT, NATIVE_V2_MEMORY_GUEST_GRANULE, SnapshotV2MemoryImageId,
    decode_snapshot_v2_memory_binding_payload,
};

use super::{
    FallibleReserve, NATIVE_V2_DIFF_EXTENT_BYTES, NATIVE_V2_DIFF_HEADER_BYTES,
    NATIVE_V2_DIFF_MAGIC, NATIVE_V2_DIFF_MAX_EXTENTS, NATIVE_V2_DIFF_MAX_METADATA_BYTES,
    NATIVE_V2_DIFF_PROFILE, NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION, ReservePolicy,
    SnapshotV2DiffBase, SnapshotV2DiffDataExtent, SnapshotV2DiffLayerBinding,
    SnapshotV2DiffLayerBindingError, build_binding, calculate_layout,
};

const FLAGS: u32 = 0;
const BASE_ZERO: u16 = 0;
const BASE_IMAGE: u16 = 1;
const MAGIC_OFFSET: usize = 0;
const VERSION_MAJOR_OFFSET: usize = 8;
const VERSION_MINOR_OFFSET: usize = 10;
const VERSION_PATCH_OFFSET: usize = 12;
const HEADER_BYTES_OFFSET: usize = 14;
const PROFILE_OFFSET: usize = 16;
const BASE_KIND_OFFSET: usize = 18;
const FLAGS_OFFSET: usize = 20;
const GUEST_GRANULE_OFFSET: usize = 24;
const EXTENT_COUNT_OFFSET: usize = 28;
const RESULT_BINDING_LENGTH_OFFSET: usize = 32;
const EXTENT_BYTES_OFFSET: usize = 36;
const METADATA_LENGTH_OFFSET: usize = 40;
const DATA_OFFSET_OFFSET: usize = 48;
const FILE_LENGTH_OFFSET: usize = 56;
const CHECKSUM_OFFSET: usize = 64;
const BASE_IMAGE_ID_OFFSET: usize = 72;
const BASE_IMAGE_ID_BYTES: usize = 16;
const RESERVED_OFFSET: usize = 88;
const RESERVED_BYTES: usize = 8;
const EXTENT_GPA_OFFSET: usize = 0;
const EXTENT_LENGTH_OFFSET: usize = 8;
const EXTENT_FILE_OFFSET: usize = 16;

pub(super) fn encode(
    binding: &SnapshotV2DiffLayerBinding,
) -> Result<Vec<u8>, SnapshotV2DiffLayerBindingError> {
    let mut reserve = FallibleReserve;
    let bytes = encode_with_reserve(binding, &mut reserve)?;
    if metadata_checksum(&bytes)? != binding.metadata_checksum() {
        return Err(SnapshotV2DiffLayerBindingError::IntegrityMismatch);
    }
    Ok(bytes)
}

pub(super) fn encode_with_reserve<R: ReservePolicy>(
    binding: &SnapshotV2DiffLayerBinding,
    reserve: &mut R,
) -> Result<Vec<u8>, SnapshotV2DiffLayerBindingError> {
    let layout = calculate_layout(binding.result(), binding.data_extents().len())?;
    if binding.version() != NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION
        || binding.metadata_length() != layout.metadata_length
        || binding.data_offset() != layout.data_offset
        || binding.file_length()
            != binding
                .data_extents()
                .iter()
                .try_fold(layout.data_offset, |offset, extent| {
                    offset.checked_add(extent.range().size())
                })
                .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?
    {
        return Err(SnapshotV2DiffLayerBindingError::InvalidLength);
    }
    let result = binding
        .result()
        .encode()
        .map_err(|source| SnapshotV2DiffLayerBindingError::ResultBinding { source })?;
    let metadata_length = usize::try_from(layout.metadata_length)
        .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    if metadata_length > NATIVE_V2_DIFF_MAX_METADATA_BYTES {
        return Err(SnapshotV2DiffLayerBindingError::MetadataTooLarge);
    }
    let mut bytes = Vec::new();
    reserve
        .reserve_bytes(&mut bytes, metadata_length)
        .map_err(|source| SnapshotV2DiffLayerBindingError::MetadataAllocationFailed { source })?;
    bytes.extend_from_slice(&NATIVE_V2_DIFF_MAGIC);
    bytes.extend_from_slice(&binding.version().major().to_le_bytes());
    bytes.extend_from_slice(&binding.version().minor().to_le_bytes());
    bytes.extend_from_slice(&binding.version().patch().to_le_bytes());
    bytes.extend_from_slice(
        &u16::try_from(NATIVE_V2_DIFF_HEADER_BYTES)
            .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&NATIVE_V2_DIFF_PROFILE.to_le_bytes());
    let (base_kind, base_image_id) = match binding.base() {
        SnapshotV2DiffBase::Zero => (BASE_ZERO, [0_u8; BASE_IMAGE_ID_BYTES]),
        SnapshotV2DiffBase::Image(image_id) => (BASE_IMAGE, image_id.to_bytes()),
    };
    bytes.extend_from_slice(&base_kind.to_le_bytes());
    bytes.extend_from_slice(&FLAGS.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(NATIVE_V2_MEMORY_GUEST_GRANULE)
            .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(binding.data_extents().len())
            .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(result.len())
            .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(NATIVE_V2_DIFF_EXTENT_BYTES)
            .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&binding.metadata_length().to_le_bytes());
    bytes.extend_from_slice(&binding.data_offset().to_le_bytes());
    bytes.extend_from_slice(&binding.file_length().to_le_bytes());
    bytes.extend_from_slice(&binding.metadata_checksum().to_le_bytes());
    bytes.extend_from_slice(&base_image_id);
    bytes.extend_from_slice(&[0_u8; RESERVED_BYTES]);
    if bytes.len() != NATIVE_V2_DIFF_HEADER_BYTES {
        return Err(SnapshotV2DiffLayerBindingError::InvalidLength);
    }
    bytes.extend_from_slice(&result);
    for extent in binding.data_extents() {
        bytes.extend_from_slice(&extent.range().start().raw_value().to_le_bytes());
        bytes.extend_from_slice(&extent.range().size().to_le_bytes());
        bytes.extend_from_slice(&extent.file_offset().to_le_bytes());
    }
    if bytes.len() != metadata_length {
        return Err(SnapshotV2DiffLayerBindingError::InvalidLength);
    }
    Ok(bytes)
}

pub(super) fn decode(
    bytes: &[u8],
) -> Result<SnapshotV2DiffLayerBinding, SnapshotV2DiffLayerBindingError> {
    let mut reserve = FallibleReserve;
    decode_with_reserve(bytes, &mut reserve)
}

pub(super) fn decode_with_reserve<R: ReservePolicy>(
    bytes: &[u8],
    reserve: &mut R,
) -> Result<SnapshotV2DiffLayerBinding, SnapshotV2DiffLayerBindingError> {
    if bytes.len() < NATIVE_V2_DIFF_HEADER_BYTES {
        return Err(SnapshotV2DiffLayerBindingError::InvalidLength);
    }
    if read_array::<8>(bytes, MAGIC_OFFSET)? != NATIVE_V2_DIFF_MAGIC {
        return Err(SnapshotV2DiffLayerBindingError::InvalidMagic);
    }
    let version = SnapshotFormatVersion::new(
        read_u16(bytes, VERSION_MAJOR_OFFSET)?,
        read_u16(bytes, VERSION_MINOR_OFFSET)?,
        read_u16(bytes, VERSION_PATCH_OFFSET)?,
    );
    if version != NATIVE_V2_DIFF_STATE_COMPATIBILITY_VERSION {
        return Err(SnapshotV2DiffLayerBindingError::UnsupportedVersion);
    }
    if usize::from(read_u16(bytes, HEADER_BYTES_OFFSET)?) != NATIVE_V2_DIFF_HEADER_BYTES
        || read_u16(bytes, PROFILE_OFFSET)? != NATIVE_V2_DIFF_PROFILE
        || read_u32(bytes, FLAGS_OFFSET)? != FLAGS
        || u64::from(read_u32(bytes, GUEST_GRANULE_OFFSET)?) != NATIVE_V2_MEMORY_GUEST_GRANULE
        || usize::try_from(read_u32(bytes, EXTENT_BYTES_OFFSET)?)
            .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?
            != NATIVE_V2_DIFF_EXTENT_BYTES
        || bytes
            .get(RESERVED_OFFSET..RESERVED_OFFSET + RESERVED_BYTES)
            .is_none_or(|reserved| reserved.iter().any(|byte| *byte != 0))
    {
        return Err(SnapshotV2DiffLayerBindingError::InvalidHeader);
    }
    let base_bytes = read_array::<BASE_IMAGE_ID_BYTES>(bytes, BASE_IMAGE_ID_OFFSET)?;
    let base = match read_u16(bytes, BASE_KIND_OFFSET)? {
        BASE_ZERO if base_bytes == [0_u8; BASE_IMAGE_ID_BYTES] => SnapshotV2DiffBase::Zero,
        BASE_IMAGE => SnapshotV2DiffBase::Image(SnapshotV2MemoryImageId::from_bytes(base_bytes)),
        _ => return Err(SnapshotV2DiffLayerBindingError::InvalidBase),
    };
    let count = usize::try_from(read_u32(bytes, EXTENT_COUNT_OFFSET)?)
        .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    if count > NATIVE_V2_DIFF_MAX_EXTENTS {
        return Err(SnapshotV2DiffLayerBindingError::CountOutOfBounds);
    }
    let result_length = usize::try_from(read_u32(bytes, RESULT_BINDING_LENGTH_OFFSET)?)
        .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    let expected_metadata_length = NATIVE_V2_DIFF_HEADER_BYTES
        .checked_add(result_length)
        .and_then(|length| {
            count
                .checked_mul(NATIVE_V2_DIFF_EXTENT_BYTES)
                .and_then(|extent_bytes| length.checked_add(extent_bytes))
        })
        .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    if expected_metadata_length > NATIVE_V2_DIFF_MAX_METADATA_BYTES {
        return Err(SnapshotV2DiffLayerBindingError::MetadataTooLarge);
    }
    let metadata_length = usize::try_from(read_u64(bytes, METADATA_LENGTH_OFFSET)?)
        .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    if metadata_length != expected_metadata_length || bytes.len() != metadata_length {
        return Err(SnapshotV2DiffLayerBindingError::InvalidLength);
    }
    let data_offset = read_u64(bytes, DATA_OFFSET_OFFSET)?;
    let expected_data_offset = align_up(
        u64::try_from(metadata_length)
            .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?,
        NATIVE_V2_MEMORY_ALIGNMENT,
    )?;
    if data_offset != expected_data_offset {
        return Err(SnapshotV2DiffLayerBindingError::InvalidLength);
    }
    let file_length = read_u64(bytes, FILE_LENGTH_OFFSET)?;
    if file_length < data_offset
        || file_length
            .checked_sub(data_offset)
            .is_none_or(|length| length > crate::memory::aarch64::DRAM_MEM_MAX_SIZE)
    {
        return Err(SnapshotV2DiffLayerBindingError::FileLengthMismatch);
    }
    let stored_checksum = read_u64(bytes, CHECKSUM_OFFSET)?;
    if metadata_checksum(bytes)? != stored_checksum {
        return Err(SnapshotV2DiffLayerBindingError::IntegrityMismatch);
    }

    let result_start = NATIVE_V2_DIFF_HEADER_BYTES;
    let result_end = result_start
        .checked_add(result_length)
        .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    let result_bytes = bytes
        .get(result_start..result_end)
        .ok_or(SnapshotV2DiffLayerBindingError::InvalidLength)?;
    let result = decode_snapshot_v2_memory_binding_payload(result_bytes)
        .map_err(|source| SnapshotV2DiffLayerBindingError::ResultBinding { source })?;
    let layout = calculate_layout(&result, count)?;
    if layout.metadata_length
        != u64::try_from(metadata_length)
            .map_err(|_| SnapshotV2DiffLayerBindingError::LengthOverflow)?
        || layout.data_offset != data_offset
    {
        return Err(SnapshotV2DiffLayerBindingError::InvalidLength);
    }

    let mut data_extents = Vec::new();
    reserve
        .reserve_extents(&mut data_extents, count)
        .map_err(|source| SnapshotV2DiffLayerBindingError::MetadataAllocationFailed { source })?;
    for index in 0..count {
        let offset = result_end
            .checked_add(
                index
                    .checked_mul(NATIVE_V2_DIFF_EXTENT_BYTES)
                    .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?,
            )
            .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
        let range = GuestMemoryRange::new(
            GuestAddress::new(read_u64(bytes, offset + EXTENT_GPA_OFFSET)?),
            read_u64(bytes, offset + EXTENT_LENGTH_OFFSET)?,
        )
        .map_err(|_| SnapshotV2DiffLayerBindingError::InvalidExtent)?;
        data_extents.push(SnapshotV2DiffDataExtent {
            range,
            file_offset: read_u64(bytes, offset + EXTENT_FILE_OFFSET)?,
        });
    }
    let binding = build_binding(base, result, data_extents, layout, stored_checksum)?;
    if binding.file_length() != file_length {
        return Err(SnapshotV2DiffLayerBindingError::FileLengthMismatch);
    }
    let canonical = encode_with_reserve(&binding, reserve)?;
    if canonical != bytes {
        return Err(SnapshotV2DiffLayerBindingError::InvalidHeader);
    }
    Ok(binding)
}

pub(super) fn metadata_checksum(bytes: &[u8]) -> Result<u64, SnapshotV2DiffLayerBindingError> {
    let before = bytes
        .get(..CHECKSUM_OFFSET)
        .ok_or(SnapshotV2DiffLayerBindingError::InvalidLength)?;
    let after = bytes
        .get(CHECKSUM_OFFSET + size_of::<u64>()..)
        .ok_or(SnapshotV2DiffLayerBindingError::InvalidLength)?;
    let checksum = crc64(0, before);
    let checksum = crc64(checksum, &[0_u8; size_of::<u64>()]);
    Ok(crc64(checksum, after))
}

fn align_up(value: u64, alignment: u64) -> Result<u64, SnapshotV2DiffLayerBindingError> {
    let mask = alignment
        .checked_sub(1)
        .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    value
        .checked_add(mask)
        .map(|value| value & !mask)
        .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, SnapshotV2DiffLayerBindingError> {
    Ok(u16::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, SnapshotV2DiffLayerBindingError> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, SnapshotV2DiffLayerBindingError> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

fn read_array<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotV2DiffLayerBindingError> {
    let end = offset
        .checked_add(LENGTH)
        .ok_or(SnapshotV2DiffLayerBindingError::LengthOverflow)?;
    let source = bytes
        .get(offset..end)
        .ok_or(SnapshotV2DiffLayerBindingError::InvalidLength)?;
    let mut value = [0_u8; LENGTH];
    value.copy_from_slice(source);
    Ok(value)
}
