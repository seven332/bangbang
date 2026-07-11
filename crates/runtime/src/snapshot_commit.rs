//! Native-v1 snapshot commit-record encoding.

use std::collections::TryReserveError;
use std::fmt;

use crate::snapshot_format::{
    NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES, NATIVE_V1_SNAPSHOT_VERSION, SnapshotFormatError,
    SnapshotFormatVersion, decode_snapshot_envelope, encode_snapshot_envelope,
};
use crate::snapshot_memory::{
    NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES, SnapshotMemoryBinding, SnapshotMemoryBindingError,
    decode_snapshot_memory_binding, encode_snapshot_memory_binding,
};

const SNAPSHOT_COMMIT_MAGIC: [u8; 8] = *b"BANGCMT\0";
const SNAPSHOT_COMMIT_MAGIC_OFFSET: usize = 0;
const SNAPSHOT_COMMIT_VERSION_MAJOR_OFFSET: usize = 8;
const SNAPSHOT_COMMIT_VERSION_MINOR_OFFSET: usize = 10;
const SNAPSHOT_COMMIT_VERSION_PATCH_OFFSET: usize = 12;
const SNAPSHOT_COMMIT_KIND_OFFSET: usize = 14;
const SNAPSHOT_COMMIT_FLAGS_OFFSET: usize = 16;
const SNAPSHOT_COMMIT_BINDING_LENGTH_OFFSET: usize = 20;
const SNAPSHOT_COMMIT_RESERVED_OFFSET: usize = 24;
const SNAPSHOT_COMMIT_MEMORY_ONLY_KIND: u16 = 1;
const SNAPSHOT_COMMIT_FLAGS: u32 = 0;
const SNAPSHOT_COMMIT_RESERVED: u64 = 0;
const REDACTED: &str = "<redacted>";

/// Fixed native-v1 snapshot commit-record header size.
pub const SNAPSHOT_COMMIT_HEADER_BYTES: usize = 32;

/// Maximum complete native-v1 snapshot commit payload size.
pub const NATIVE_V1_SNAPSHOT_COMMIT_MAX_BYTES: usize =
    SNAPSHOT_COMMIT_HEADER_BYTES + NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES;

const _: () = assert!(NATIVE_V1_SNAPSHOT_COMMIT_MAX_BYTES <= NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES);

/// Validated native-v1 memory-only snapshot commit record.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotCommitRecord {
    memory_binding: SnapshotMemoryBinding,
}

impl SnapshotCommitRecord {
    /// Creates a commit record around an already validated memory binding.
    pub const fn new(memory_binding: SnapshotMemoryBinding) -> Self {
        Self { memory_binding }
    }

    /// Returns the exact native snapshot commit-record version.
    pub const fn version(&self) -> SnapshotFormatVersion {
        NATIVE_V1_SNAPSHOT_VERSION
    }

    /// Returns the validated state-to-memory artifact binding.
    pub const fn memory_binding(&self) -> &SnapshotMemoryBinding {
        &self.memory_binding
    }
}

impl fmt::Debug for SnapshotCommitRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotCommitRecord")
            .field("version", &NATIVE_V1_SNAPSHOT_VERSION)
            .field("kind", &"memory-only")
            .field("memory_binding", &REDACTED)
            .finish()
    }
}

/// Native-v1 commit-record encoding or validation failure.
#[derive(Debug)]
pub enum SnapshotCommitError {
    /// The outer native snapshot envelope is invalid.
    Envelope(SnapshotFormatError),
    /// Input ended before the declared or minimum commit-record length.
    Truncated { expected: usize, actual: usize },
    /// Bytes remain after the exact declared commit-record length.
    TrailingData { expected: usize, actual: usize },
    /// The record does not carry bangbang native commit magic.
    InvalidMagic,
    /// The commit-record semantic version is unsupported.
    UnsupportedVersion(SnapshotFormatVersion),
    /// The commit-record kind is unsupported.
    UnsupportedKind(u16),
    /// Native-v1 commit-record flags are nonzero.
    UnsupportedFlags(u32),
    /// Native-v1 commit-record reserved bytes are nonzero.
    UnsupportedReserved(u64),
    /// The declared binding length is empty or exceeds native-v1 policy.
    BindingLengthOutOfBounds { length: u64, maximum: usize },
    /// Commit-record length or offset arithmetic overflowed.
    LengthOverflow,
    /// Commit-record allocation failed.
    AllocationFailed { source: TryReserveError },
    /// The nested memory binding is invalid.
    MemoryBinding(SnapshotMemoryBindingError),
}

impl fmt::Display for SnapshotCommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Envelope(source) => write!(f, "invalid snapshot envelope: {source}"),
            Self::Truncated { expected, actual } => write!(
                f,
                "snapshot commit record is truncated: expected {expected} bytes, found {actual}"
            ),
            Self::TrailingData { expected, actual } => write!(
                f,
                "snapshot commit record has trailing data: expected {expected} bytes, found {actual}"
            ),
            Self::InvalidMagic => f.write_str("snapshot commit record magic is invalid"),
            Self::UnsupportedVersion(version) => {
                write!(f, "snapshot commit record version {version} is unsupported")
            }
            Self::UnsupportedKind(kind) => {
                write!(f, "snapshot commit record kind {kind} is unsupported")
            }
            Self::UnsupportedFlags(flags) => write!(
                f,
                "snapshot commit record has unsupported flags 0x{flags:08x}"
            ),
            Self::UnsupportedReserved(reserved) => write!(
                f,
                "snapshot commit record has unsupported reserved value 0x{reserved:016x}"
            ),
            Self::BindingLengthOutOfBounds { length, maximum } => write!(
                f,
                "snapshot commit binding length {length} is outside 1..={maximum}"
            ),
            Self::LengthOverflow => {
                f.write_str("snapshot commit record length arithmetic overflowed")
            }
            Self::AllocationFailed { source } => {
                write!(f, "failed to allocate snapshot commit record: {source}")
            }
            Self::MemoryBinding(source) => {
                write!(f, "invalid snapshot commit memory binding: {source}")
            }
        }
    }
}

impl std::error::Error for SnapshotCommitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Envelope(source) => Some(source),
            Self::AllocationFailed { source } => Some(source),
            Self::MemoryBinding(source) => Some(source),
            Self::Truncated { .. }
            | Self::TrailingData { .. }
            | Self::InvalidMagic
            | Self::UnsupportedVersion(_)
            | Self::UnsupportedKind(_)
            | Self::UnsupportedFlags(_)
            | Self::UnsupportedReserved(_)
            | Self::BindingLengthOutOfBounds { .. }
            | Self::LengthOverflow => None,
        }
    }
}

impl From<SnapshotFormatError> for SnapshotCommitError {
    fn from(source: SnapshotFormatError) -> Self {
        Self::Envelope(source)
    }
}

impl From<SnapshotMemoryBindingError> for SnapshotCommitError {
    fn from(source: SnapshotMemoryBindingError) -> Self {
        Self::MemoryBinding(source)
    }
}

/// Deterministically encodes a validated native-v1 commit payload.
pub fn encode_snapshot_commit_payload(
    record: &SnapshotCommitRecord,
) -> Result<Vec<u8>, SnapshotCommitError> {
    let binding = encode_snapshot_memory_binding(record.memory_binding())?;
    let binding_length =
        u32::try_from(binding.len()).map_err(|_| SnapshotCommitError::LengthOverflow)?;
    validate_binding_length(u64::from(binding_length))?;
    let encoded_length = SNAPSHOT_COMMIT_HEADER_BYTES
        .checked_add(binding.len())
        .ok_or(SnapshotCommitError::LengthOverflow)?;

    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(encoded_length)
        .map_err(|source| SnapshotCommitError::AllocationFailed { source })?;
    encoded.extend_from_slice(&SNAPSHOT_COMMIT_MAGIC);
    encoded.extend_from_slice(&NATIVE_V1_SNAPSHOT_VERSION.major().to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_SNAPSHOT_VERSION.minor().to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_SNAPSHOT_VERSION.patch().to_le_bytes());
    encoded.extend_from_slice(&SNAPSHOT_COMMIT_MEMORY_ONLY_KIND.to_le_bytes());
    encoded.extend_from_slice(&SNAPSHOT_COMMIT_FLAGS.to_le_bytes());
    encoded.extend_from_slice(&binding_length.to_le_bytes());
    encoded.extend_from_slice(&SNAPSHOT_COMMIT_RESERVED.to_le_bytes());
    encoded.extend_from_slice(&binding);
    Ok(encoded)
}

/// Decodes and fully validates a native-v1 commit payload.
pub fn decode_snapshot_commit_payload(
    bytes: &[u8],
) -> Result<SnapshotCommitRecord, SnapshotCommitError> {
    if bytes.len() < SNAPSHOT_COMMIT_HEADER_BYTES {
        return Err(SnapshotCommitError::Truncated {
            expected: SNAPSHOT_COMMIT_HEADER_BYTES,
            actual: bytes.len(),
        });
    }
    if read_array::<8>(bytes, SNAPSHOT_COMMIT_MAGIC_OFFSET)? != SNAPSHOT_COMMIT_MAGIC {
        return Err(SnapshotCommitError::InvalidMagic);
    }

    let binding_length = u32::from_le_bytes(read_array::<4>(
        bytes,
        SNAPSHOT_COMMIT_BINDING_LENGTH_OFFSET,
    )?);
    validate_binding_length(u64::from(binding_length))?;
    let binding_length =
        usize::try_from(binding_length).map_err(|_| SnapshotCommitError::LengthOverflow)?;
    let expected_length = SNAPSHOT_COMMIT_HEADER_BYTES
        .checked_add(binding_length)
        .ok_or(SnapshotCommitError::LengthOverflow)?;
    if bytes.len() < expected_length {
        return Err(SnapshotCommitError::Truncated {
            expected: expected_length,
            actual: bytes.len(),
        });
    }
    if bytes.len() > expected_length {
        return Err(SnapshotCommitError::TrailingData {
            expected: expected_length,
            actual: bytes.len(),
        });
    }

    let version = SnapshotFormatVersion::new(
        u16::from_le_bytes(read_array::<2>(
            bytes,
            SNAPSHOT_COMMIT_VERSION_MAJOR_OFFSET,
        )?),
        u16::from_le_bytes(read_array::<2>(
            bytes,
            SNAPSHOT_COMMIT_VERSION_MINOR_OFFSET,
        )?),
        u16::from_le_bytes(read_array::<2>(
            bytes,
            SNAPSHOT_COMMIT_VERSION_PATCH_OFFSET,
        )?),
    );
    if version != NATIVE_V1_SNAPSHOT_VERSION {
        return Err(SnapshotCommitError::UnsupportedVersion(version));
    }
    let kind = u16::from_le_bytes(read_array::<2>(bytes, SNAPSHOT_COMMIT_KIND_OFFSET)?);
    if kind != SNAPSHOT_COMMIT_MEMORY_ONLY_KIND {
        return Err(SnapshotCommitError::UnsupportedKind(kind));
    }
    let flags = u32::from_le_bytes(read_array::<4>(bytes, SNAPSHOT_COMMIT_FLAGS_OFFSET)?);
    if flags != SNAPSHOT_COMMIT_FLAGS {
        return Err(SnapshotCommitError::UnsupportedFlags(flags));
    }
    let reserved = u64::from_le_bytes(read_array::<8>(bytes, SNAPSHOT_COMMIT_RESERVED_OFFSET)?);
    if reserved != SNAPSHOT_COMMIT_RESERVED {
        return Err(SnapshotCommitError::UnsupportedReserved(reserved));
    }

    let binding_bytes = bytes
        .get(SNAPSHOT_COMMIT_HEADER_BYTES..expected_length)
        .ok_or(SnapshotCommitError::LengthOverflow)?;
    let memory_binding = decode_snapshot_memory_binding(binding_bytes)?;
    Ok(SnapshotCommitRecord::new(memory_binding))
}

/// Encodes a validated commit record in the native-v1 state envelope.
pub fn encode_snapshot_commit_envelope(
    record: &SnapshotCommitRecord,
) -> Result<Vec<u8>, SnapshotCommitError> {
    let payload = encode_snapshot_commit_payload(record)?;
    encode_snapshot_envelope(&payload).map_err(SnapshotCommitError::Envelope)
}

/// Decodes and fully validates a native-v1 commit envelope.
pub fn decode_snapshot_commit_envelope(
    bytes: &[u8],
) -> Result<SnapshotCommitRecord, SnapshotCommitError> {
    let envelope = decode_snapshot_envelope(bytes)?;
    decode_snapshot_commit_payload(envelope.payload())
}

fn validate_binding_length(length: u64) -> Result<(), SnapshotCommitError> {
    let maximum = u64::try_from(NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES)
        .map_err(|_| SnapshotCommitError::LengthOverflow)?;
    if length == 0 || length > maximum {
        return Err(SnapshotCommitError::BindingLengthOutOfBounds {
            length,
            maximum: NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES,
        });
    }
    Ok(())
}

fn read_array<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotCommitError> {
    let end = offset
        .checked_add(LENGTH)
        .ok_or(SnapshotCommitError::LengthOverflow)?;
    let source = bytes
        .get(offset..end)
        .ok_or(SnapshotCommitError::Truncated {
            expected: end,
            actual: bytes.len(),
        })?;
    let mut result = [0; LENGTH];
    result.copy_from_slice(source);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot_format::{
        SNAPSHOT_ENVELOPE_HEADER_BYTES, SNAPSHOT_ENVELOPE_INTEGRITY_BYTES,
    };
    use crate::snapshot_memory::{
        SNAPSHOT_MEMORY_BINDING_HEADER_BYTES, SNAPSHOT_MEMORY_BINDING_RANGE_BYTES,
        SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES, SNAPSHOT_MEMORY_IMAGE_INTEGRITY_BYTES,
    };

    #[test]
    fn payload_round_trips_and_pins_fixed_header() {
        let (binding, binding_bytes) = test_binding(1);
        let record = SnapshotCommitRecord::new(binding.clone());
        let encoded = encode_snapshot_commit_payload(&record).expect("fixture should encode");
        let expected_header = [
            0x42, 0x41, 0x4e, 0x47, 0x43, 0x4d, 0x54, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x60, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];

        assert_eq!(
            encoded.get(..SNAPSHOT_COMMIT_HEADER_BYTES),
            Some(expected_header.as_slice())
        );
        assert_eq!(
            encoded.get(SNAPSHOT_COMMIT_HEADER_BYTES..),
            Some(binding_bytes.as_slice())
        );
        assert_eq!(
            decode_snapshot_commit_payload(&encoded)
                .expect("fixture should decode")
                .memory_binding(),
            &binding
        );
        assert_eq!(
            encode_snapshot_commit_payload(&record).expect("fixture should re-encode"),
            encoded
        );
    }

    #[test]
    fn envelope_round_trips_exact_commit() {
        let (binding, binding_bytes) = test_binding(2);
        let record = SnapshotCommitRecord::new(binding);
        let payload = encode_snapshot_commit_payload(&record).expect("fixture should encode");
        let envelope = encode_snapshot_commit_envelope(&record).expect("fixture should encode");
        let mut expected_payload = vec![
            0x42, 0x41, 0x4e, 0x47, 0x43, 0x4d, 0x54, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x78, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        expected_payload.extend_from_slice(&binding_bytes);

        assert_eq!(payload, expected_payload);
        assert_eq!(
            envelope.len(),
            SNAPSHOT_ENVELOPE_HEADER_BYTES + payload.len() + SNAPSHOT_ENVELOPE_INTEGRITY_BYTES
        );
        assert_eq!(
            envelope,
            encode_snapshot_envelope(&expected_payload).expect("golden payload should envelope")
        );
        assert_eq!(
            decode_snapshot_commit_envelope(&envelope).expect("fixture should decode"),
            record
        );
    }

    #[test]
    fn maximum_binding_fits_outer_payload() {
        let (binding, binding_bytes) = test_binding(4096);
        let payload = encode_snapshot_commit_payload(&SnapshotCommitRecord::new(binding))
            .expect("maximum fixture should encode");

        assert_eq!(
            binding_bytes.len(),
            NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES
        );
        assert_eq!(payload.len(), NATIVE_V1_SNAPSHOT_COMMIT_MAX_BYTES);
        assert!(payload.len() <= NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES);
    }

    #[test]
    fn rejects_every_fixed_header_truncation() {
        let (binding, _) = test_binding(1);
        let encoded = encode_snapshot_commit_payload(&SnapshotCommitRecord::new(binding))
            .expect("fixture should encode");

        for actual in 0..SNAPSHOT_COMMIT_HEADER_BYTES {
            let bytes = encoded.get(..actual).expect("fixture prefix should exist");
            assert!(matches!(
                decode_snapshot_commit_payload(bytes),
                Err(SnapshotCommitError::Truncated {
                    expected: SNAPSHOT_COMMIT_HEADER_BYTES,
                    actual: observed,
                }) if observed == actual
            ));
        }
    }

    #[test]
    fn rejects_declared_binding_truncation_and_trailing_data() {
        let (binding, _) = test_binding(1);
        let encoded = encode_snapshot_commit_payload(&SnapshotCommitRecord::new(binding))
            .expect("fixture should encode");
        let truncated = encoded
            .get(..encoded.len() - 1)
            .expect("prefix should exist");
        assert!(matches!(
            decode_snapshot_commit_payload(truncated),
            Err(SnapshotCommitError::Truncated { expected, actual })
                if expected == encoded.len() && actual == encoded.len() - 1
        ));

        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(matches!(
            decode_snapshot_commit_payload(&trailing),
            Err(SnapshotCommitError::TrailingData { expected, actual })
                if expected == encoded.len() && actual == encoded.len() + 1
        ));
    }

    #[test]
    fn rejects_magic_version_kind_flags_and_reserved() {
        let (binding, _) = test_binding(1);
        let encoded = encode_snapshot_commit_payload(&SnapshotCommitRecord::new(binding))
            .expect("fixture should encode");

        let mut invalid_magic = encoded.clone();
        *invalid_magic
            .get_mut(SNAPSHOT_COMMIT_MAGIC_OFFSET)
            .expect("magic should exist") ^= 0xff;
        assert!(matches!(
            decode_snapshot_commit_payload(&invalid_magic),
            Err(SnapshotCommitError::InvalidMagic)
        ));

        let mut invalid_version = encoded.clone();
        *invalid_version
            .get_mut(SNAPSHOT_COMMIT_VERSION_MINOR_OFFSET)
            .expect("version should exist") = 1;
        assert!(matches!(
            decode_snapshot_commit_payload(&invalid_version),
            Err(SnapshotCommitError::UnsupportedVersion(version))
                if version == SnapshotFormatVersion::new(1, 1, 0)
        ));

        let mut invalid_kind = encoded.clone();
        *invalid_kind
            .get_mut(SNAPSHOT_COMMIT_KIND_OFFSET)
            .expect("kind should exist") = 2;
        assert!(matches!(
            decode_snapshot_commit_payload(&invalid_kind),
            Err(SnapshotCommitError::UnsupportedKind(2))
        ));

        let mut invalid_flags = encoded.clone();
        *invalid_flags
            .get_mut(SNAPSHOT_COMMIT_FLAGS_OFFSET)
            .expect("flags should exist") = 1;
        assert!(matches!(
            decode_snapshot_commit_payload(&invalid_flags),
            Err(SnapshotCommitError::UnsupportedFlags(1))
        ));

        let mut invalid_reserved = encoded;
        *invalid_reserved
            .get_mut(SNAPSHOT_COMMIT_RESERVED_OFFSET)
            .expect("reserved should exist") = 1;
        assert!(matches!(
            decode_snapshot_commit_payload(&invalid_reserved),
            Err(SnapshotCommitError::UnsupportedReserved(1))
        ));
    }

    #[test]
    fn rejects_empty_and_oversized_binding_lengths_before_nested_decode() {
        for (length, expected_length) in [
            (0_u32, 0_u64),
            (
                u32::try_from(NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES)
                    .expect("maximum should fit u32")
                    + 1,
                u64::try_from(NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES)
                    .expect("maximum should fit u64")
                    + 1,
            ),
        ] {
            let mut encoded = vec![0; SNAPSHOT_COMMIT_HEADER_BYTES];
            encoded
                .get_mut(..8)
                .expect("magic field should exist")
                .copy_from_slice(&SNAPSHOT_COMMIT_MAGIC);
            encoded
                .get_mut(
                    SNAPSHOT_COMMIT_BINDING_LENGTH_OFFSET
                        ..SNAPSHOT_COMMIT_BINDING_LENGTH_OFFSET + 4,
                )
                .expect("length field should exist")
                .copy_from_slice(&length.to_le_bytes());

            assert!(matches!(
                decode_snapshot_commit_payload(&encoded),
                Err(SnapshotCommitError::BindingLengthOutOfBounds { length, maximum })
                    if length == expected_length
                        && maximum == NATIVE_V1_SNAPSHOT_MEMORY_MAX_BINDING_BYTES
            ));
        }
    }

    #[test]
    fn rejects_invalid_nested_binding_and_outer_envelope() {
        let (binding, _) = test_binding(1);
        let record = SnapshotCommitRecord::new(binding);
        let mut payload = encode_snapshot_commit_payload(&record).expect("fixture should encode");
        *payload
            .get_mut(SNAPSHOT_COMMIT_HEADER_BYTES)
            .expect("nested magic should exist") ^= 0xff;
        assert!(matches!(
            decode_snapshot_commit_payload(&payload),
            Err(SnapshotCommitError::MemoryBinding(
                SnapshotMemoryBindingError::InvalidMagic
            ))
        ));

        let mut envelope = encode_snapshot_commit_envelope(&record).expect("fixture should encode");
        *envelope.get_mut(0).expect("outer magic should exist") ^= 0xff;
        assert!(matches!(
            decode_snapshot_commit_envelope(&envelope),
            Err(SnapshotCommitError::Envelope(
                SnapshotFormatError::InvalidMagic
            ))
        ));
    }

    #[test]
    fn diagnostics_redact_binding_identity_and_checksum() {
        let (binding, _) = test_binding(1);
        let identity = format!("{:02x?}", binding.image_id().as_bytes());
        let checksum = format!("{:016x}", binding.checksum());
        let record = SnapshotCommitRecord::new(binding);
        let debug = format!("{record:?}");

        assert!(debug.contains(REDACTED));
        assert!(!debug.contains(&identity));
        assert!(!debug.contains(&checksum));
    }

    fn test_binding(range_count: usize) -> (SnapshotMemoryBinding, Vec<u8>) {
        let data_length =
            u64::try_from(range_count).expect("fixture range count should fit u64") * 4096;
        let file_length = u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
            .expect("header should fit u64")
            + data_length
            + u64::try_from(SNAPSHOT_MEMORY_IMAGE_INTEGRITY_BYTES).expect("trailer should fit u64");
        let mut bytes = Vec::with_capacity(
            SNAPSHOT_MEMORY_BINDING_HEADER_BYTES
                + range_count * SNAPSHOT_MEMORY_BINDING_RANGE_BYTES,
        );
        bytes.extend_from_slice(b"BANGMBND");
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&4096_u32.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        bytes.extend_from_slice(&[0x5a; 16]);
        bytes.extend_from_slice(&data_length.to_le_bytes());
        bytes.extend_from_slice(&file_length.to_le_bytes());
        bytes.extend_from_slice(&0x1122_3344_5566_7788_u64.to_le_bytes());
        bytes.extend_from_slice(
            &u32::try_from(range_count)
                .expect("fixture range count should fit u32")
                .to_le_bytes(),
        );
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        for index in 0..range_count {
            let index = u64::try_from(index).expect("fixture index should fit u64");
            bytes.extend_from_slice(&(index * 4096).to_le_bytes());
            bytes.extend_from_slice(&4096_u64.to_le_bytes());
            bytes.extend_from_slice(
                &(u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
                    .expect("header should fit u64")
                    + index * 4096)
                    .to_le_bytes(),
            );
        }

        let binding = decode_snapshot_memory_binding(&bytes).expect("fixture should be valid");
        (binding, bytes)
    }
}
