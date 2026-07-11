//! Native snapshot-state envelope encoding and inspection.

use std::fmt;

use crc64::crc64;

const SNAPSHOT_MAGIC: [u8; 8] = *b"BANGSNAP";
const SNAPSHOT_MAGIC_OFFSET: usize = 0;
const SNAPSHOT_VERSION_MAJOR_OFFSET: usize = 8;
const SNAPSHOT_VERSION_MINOR_OFFSET: usize = 10;
const SNAPSHOT_VERSION_PATCH_OFFSET: usize = 12;
const SNAPSHOT_ARCHITECTURE_OFFSET: usize = 14;
const SNAPSHOT_GUEST_PAGE_SIZE_OFFSET: usize = 16;
const SNAPSHOT_RESERVED_FLAGS_OFFSET: usize = 20;
const SNAPSHOT_PAYLOAD_LENGTH_OFFSET: usize = 24;
pub(crate) const NATIVE_V1_ARM64_ARCHITECTURE_ID: u16 = 1;
const NATIVE_V1_RESERVED_FLAGS: u32 = 0;
const REDACTED: &str = "<redacted>";

/// Fixed native snapshot envelope header size in bytes.
pub const SNAPSHOT_ENVELOPE_HEADER_BYTES: usize = 32;

/// Native snapshot envelope CRC trailer size in bytes.
pub const SNAPSHOT_ENVELOPE_INTEGRITY_BYTES: usize = 8;

/// Maximum opaque state payload accepted by the native-v1 reader.
pub const NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// Maximum complete native-v1 state-file size accepted by the reader.
pub const NATIVE_V1_SNAPSHOT_MAX_FILE_BYTES: usize = SNAPSHOT_ENVELOPE_HEADER_BYTES
    + NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES
    + SNAPSHOT_ENVELOPE_INTEGRITY_BYTES;

/// Native-v1 guest-memory granule recorded in the state envelope.
pub const NATIVE_V1_GUEST_PAGE_SIZE: u32 = 4096;

/// Semantic snapshot data-format version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotFormatVersion {
    major: u16,
    minor: u16,
    patch: u16,
}

impl SnapshotFormatVersion {
    pub(crate) const fn new(major: u16, minor: u16, patch: u16) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Returns the major version.
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the minor version.
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Returns the patch version.
    pub const fn patch(self) -> u16 {
        self.patch
    }
}

impl fmt::Display for SnapshotFormatVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Native snapshot data-format version emitted and accepted by this binary.
pub const NATIVE_V1_SNAPSHOT_VERSION: SnapshotFormatVersion = SnapshotFormatVersion::new(1, 0, 0);

/// Guest architecture identified by a native snapshot envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotArchitecture {
    /// Arm 64-bit architecture.
    Arm64,
}

impl fmt::Display for SnapshotArchitecture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Arm64 => f.write_str("arm64"),
        }
    }
}

/// Integrity algorithm fixed by native snapshot format v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotIntegrity {
    /// CRC-64/Jones over the fixed header and opaque payload.
    Crc64Jones,
}

impl fmt::Display for SnapshotIntegrity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Crc64Jones => f.write_str("CRC-64/Jones"),
        }
    }
}

/// Stable, non-sensitive metadata inspected from a valid snapshot envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEnvelopeMetadata {
    version: SnapshotFormatVersion,
    architecture: SnapshotArchitecture,
    guest_page_size: u32,
    payload_length: u64,
    integrity: SnapshotIntegrity,
}

impl SnapshotEnvelopeMetadata {
    /// Returns the embedded snapshot data-format version.
    pub const fn version(self) -> SnapshotFormatVersion {
        self.version
    }

    /// Returns the embedded guest architecture.
    pub const fn architecture(self) -> SnapshotArchitecture {
        self.architecture
    }

    /// Returns the embedded guest-memory granule in bytes.
    pub const fn guest_page_size(self) -> u32 {
        self.guest_page_size
    }

    /// Returns the opaque payload length in bytes.
    pub const fn payload_length(self) -> u64 {
        self.payload_length
    }

    /// Returns the integrity algorithm fixed by the format version.
    pub const fn integrity(self) -> SnapshotIntegrity {
        self.integrity
    }
}

/// A fully validated native snapshot envelope borrowing its opaque payload.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotEnvelope<'payload> {
    metadata: SnapshotEnvelopeMetadata,
    payload: &'payload [u8],
}

impl SnapshotEnvelope<'_> {
    /// Returns stable, non-sensitive envelope metadata.
    pub const fn metadata(&self) -> SnapshotEnvelopeMetadata {
        self.metadata
    }

    /// Returns the bounded opaque payload.
    pub const fn payload(&self) -> &[u8] {
        self.payload
    }
}

impl fmt::Debug for SnapshotEnvelope<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotEnvelope")
            .field("metadata", &self.metadata)
            .field("payload", &REDACTED)
            .finish()
    }
}

/// Native snapshot envelope encoding or validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotFormatError {
    /// Input ended before the declared or minimum envelope length.
    Truncated { expected: usize, actual: usize },
    /// The file does not carry bangbang native snapshot magic.
    InvalidMagic,
    /// The embedded payload length cannot be represented safely.
    PayloadLengthOverflow,
    /// The embedded or supplied payload exceeds the reader policy.
    PayloadTooLarge { length: u64, maximum: usize },
    /// Bytes remain after the exact declared envelope length.
    TrailingData { expected: usize, actual: usize },
    /// The CRC-64/Jones trailer does not match the header and payload.
    IntegrityMismatch,
    /// Native-v1 reserved flags are nonzero.
    UnsupportedFlags(u32),
    /// The snapshot format version is not accepted by this binary.
    UnsupportedVersion(SnapshotFormatVersion),
    /// The snapshot architecture is not the supported arm64 architecture.
    IncompatibleArchitecture(u16),
    /// The snapshot guest-memory granule differs from native v1.
    IncompatibleGuestPageSize(u32),
}

impl fmt::Display for SnapshotFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { expected, actual } => write!(
                f,
                "snapshot envelope is truncated: expected {expected} bytes, found {actual}"
            ),
            Self::InvalidMagic => f.write_str("snapshot envelope magic is invalid"),
            Self::PayloadLengthOverflow => {
                f.write_str("snapshot envelope payload length overflows the supported address size")
            }
            Self::PayloadTooLarge { length, maximum } => write!(
                f,
                "snapshot envelope payload length {length} exceeds {maximum} byte limit"
            ),
            Self::TrailingData { expected, actual } => write!(
                f,
                "snapshot envelope has trailing data: expected {expected} bytes, found {actual}"
            ),
            Self::IntegrityMismatch => {
                f.write_str("snapshot envelope CRC-64/Jones integrity check failed")
            }
            Self::UnsupportedFlags(flags) => {
                write!(f, "snapshot envelope has unsupported flags 0x{flags:08x}")
            }
            Self::UnsupportedVersion(version) => {
                write!(f, "snapshot format version {version} is unsupported")
            }
            Self::IncompatibleArchitecture(architecture) => write!(
                f,
                "snapshot architecture identifier {architecture} is incompatible"
            ),
            Self::IncompatibleGuestPageSize(page_size) => {
                write!(f, "snapshot guest page size {page_size} is incompatible")
            }
        }
    }
}

impl std::error::Error for SnapshotFormatError {}

/// Encodes an opaque payload in the native-v1 snapshot envelope.
pub fn encode_snapshot_envelope(payload: &[u8]) -> Result<Vec<u8>, SnapshotFormatError> {
    let payload_length =
        u64::try_from(payload.len()).map_err(|_| SnapshotFormatError::PayloadLengthOverflow)?;
    validate_payload_limit(payload_length)?;

    let mut encoded = Vec::with_capacity(
        SNAPSHOT_ENVELOPE_HEADER_BYTES + payload.len() + SNAPSHOT_ENVELOPE_INTEGRITY_BYTES,
    );
    encoded.extend_from_slice(&SNAPSHOT_MAGIC);
    encoded.extend_from_slice(&NATIVE_V1_SNAPSHOT_VERSION.major().to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_SNAPSHOT_VERSION.minor().to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_SNAPSHOT_VERSION.patch().to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_ARM64_ARCHITECTURE_ID.to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_GUEST_PAGE_SIZE.to_le_bytes());
    encoded.extend_from_slice(&NATIVE_V1_RESERVED_FLAGS.to_le_bytes());
    encoded.extend_from_slice(&payload_length.to_le_bytes());
    encoded.extend_from_slice(payload);

    let checksum = crc64(0, &encoded);
    encoded.extend_from_slice(&checksum.to_le_bytes());
    Ok(encoded)
}

/// Decodes and fully validates a native-v1 snapshot envelope.
pub fn decode_snapshot_envelope(bytes: &[u8]) -> Result<SnapshotEnvelope<'_>, SnapshotFormatError> {
    let minimum_length = SNAPSHOT_ENVELOPE_HEADER_BYTES + SNAPSHOT_ENVELOPE_INTEGRITY_BYTES;
    if bytes.len() < minimum_length {
        return Err(SnapshotFormatError::Truncated {
            expected: minimum_length,
            actual: bytes.len(),
        });
    }

    if read_array::<8>(bytes, SNAPSHOT_MAGIC_OFFSET)? != SNAPSHOT_MAGIC {
        return Err(SnapshotFormatError::InvalidMagic);
    }

    let payload_length =
        u64::from_le_bytes(read_array::<8>(bytes, SNAPSHOT_PAYLOAD_LENGTH_OFFSET)?);
    let payload_length_usize =
        usize::try_from(payload_length).map_err(|_| SnapshotFormatError::PayloadLengthOverflow)?;
    let expected_length = SNAPSHOT_ENVELOPE_HEADER_BYTES
        .checked_add(payload_length_usize)
        .and_then(|length| length.checked_add(SNAPSHOT_ENVELOPE_INTEGRITY_BYTES))
        .ok_or(SnapshotFormatError::PayloadLengthOverflow)?;
    validate_payload_limit(payload_length)?;

    if bytes.len() < expected_length {
        return Err(SnapshotFormatError::Truncated {
            expected: expected_length,
            actual: bytes.len(),
        });
    }
    if bytes.len() > expected_length {
        return Err(SnapshotFormatError::TrailingData {
            expected: expected_length,
            actual: bytes.len(),
        });
    }

    let payload_end = SNAPSHOT_ENVELOPE_HEADER_BYTES
        .checked_add(payload_length_usize)
        .ok_or(SnapshotFormatError::PayloadLengthOverflow)?;
    let checksummed = bytes
        .get(..payload_end)
        .ok_or(SnapshotFormatError::PayloadLengthOverflow)?;
    let stored_checksum = u64::from_le_bytes(read_array::<8>(bytes, payload_end)?);
    if crc64(0, checksummed) != stored_checksum {
        return Err(SnapshotFormatError::IntegrityMismatch);
    }

    let flags = u32::from_le_bytes(read_array::<4>(bytes, SNAPSHOT_RESERVED_FLAGS_OFFSET)?);
    if flags != NATIVE_V1_RESERVED_FLAGS {
        return Err(SnapshotFormatError::UnsupportedFlags(flags));
    }

    let version = SnapshotFormatVersion::new(
        u16::from_le_bytes(read_array::<2>(bytes, SNAPSHOT_VERSION_MAJOR_OFFSET)?),
        u16::from_le_bytes(read_array::<2>(bytes, SNAPSHOT_VERSION_MINOR_OFFSET)?),
        u16::from_le_bytes(read_array::<2>(bytes, SNAPSHOT_VERSION_PATCH_OFFSET)?),
    );
    if version != NATIVE_V1_SNAPSHOT_VERSION {
        return Err(SnapshotFormatError::UnsupportedVersion(version));
    }

    let architecture = u16::from_le_bytes(read_array::<2>(bytes, SNAPSHOT_ARCHITECTURE_OFFSET)?);
    if architecture != NATIVE_V1_ARM64_ARCHITECTURE_ID {
        return Err(SnapshotFormatError::IncompatibleArchitecture(architecture));
    }

    let guest_page_size =
        u32::from_le_bytes(read_array::<4>(bytes, SNAPSHOT_GUEST_PAGE_SIZE_OFFSET)?);
    if guest_page_size != NATIVE_V1_GUEST_PAGE_SIZE {
        return Err(SnapshotFormatError::IncompatibleGuestPageSize(
            guest_page_size,
        ));
    }

    let payload = bytes
        .get(SNAPSHOT_ENVELOPE_HEADER_BYTES..payload_end)
        .ok_or(SnapshotFormatError::PayloadLengthOverflow)?;
    Ok(SnapshotEnvelope {
        metadata: SnapshotEnvelopeMetadata {
            version,
            architecture: SnapshotArchitecture::Arm64,
            guest_page_size,
            payload_length,
            integrity: SnapshotIntegrity::Crc64Jones,
        },
        payload,
    })
}

/// Inspects stable metadata after fully validating a native-v1 envelope.
pub fn inspect_snapshot_envelope(
    bytes: &[u8],
) -> Result<SnapshotEnvelopeMetadata, SnapshotFormatError> {
    decode_snapshot_envelope(bytes).map(|envelope| envelope.metadata())
}

fn validate_payload_limit(payload_length: u64) -> Result<(), SnapshotFormatError> {
    let maximum = u64::try_from(NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES)
        .map_err(|_| SnapshotFormatError::PayloadLengthOverflow)?;
    if payload_length > maximum {
        return Err(SnapshotFormatError::PayloadTooLarge {
            length: payload_length,
            maximum: NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES,
        });
    }

    Ok(())
}

fn read_array<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotFormatError> {
    let end = offset
        .checked_add(LENGTH)
        .ok_or(SnapshotFormatError::PayloadLengthOverflow)?;
    let source = bytes
        .get(offset..end)
        .ok_or(SnapshotFormatError::Truncated {
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

    const TEST_PAYLOAD: &[u8] = b"state";

    #[test]
    fn native_v1_encoding_matches_golden_bytes() {
        let encoded = encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should encode");
        let expected = [
            0x42, 0x41, 0x4e, 0x47, 0x53, 0x4e, 0x41, 0x50, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x73, 0x74, 0x61, 0x74, 0x65, 0x9e, 0xf0, 0xc0, 0x25, 0xe2,
            0x0a, 0x82, 0x32,
        ];

        assert_eq!(encoded, expected);
        assert_eq!(
            encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should re-encode"),
            expected
        );
    }

    #[test]
    fn round_trips_empty_ordinary_and_maximum_payloads() {
        for payload in [
            Vec::new(),
            TEST_PAYLOAD.to_vec(),
            vec![0xa5; NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES],
        ] {
            let encoded = encode_snapshot_envelope(&payload).expect("payload should encode");
            let envelope = decode_snapshot_envelope(&encoded).expect("payload should decode");

            assert_eq!(envelope.payload(), payload);
            assert_eq!(
                envelope.metadata(),
                SnapshotEnvelopeMetadata {
                    version: NATIVE_V1_SNAPSHOT_VERSION,
                    architecture: SnapshotArchitecture::Arm64,
                    guest_page_size: NATIVE_V1_GUEST_PAGE_SIZE,
                    payload_length: u64::try_from(payload.len())
                        .expect("supported payload length should fit u64"),
                    integrity: SnapshotIntegrity::Crc64Jones,
                }
            );
            assert_eq!(
                inspect_snapshot_envelope(&encoded).expect("payload should inspect"),
                envelope.metadata()
            );
        }
    }

    #[test]
    fn decoded_payload_borrows_the_input() {
        let encoded = encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should encode");
        let envelope = decode_snapshot_envelope(&encoded).expect("fixture should decode");
        let expected = encoded
            .get(
                SNAPSHOT_ENVELOPE_HEADER_BYTES..SNAPSHOT_ENVELOPE_HEADER_BYTES + TEST_PAYLOAD.len(),
            )
            .expect("fixture payload should exist");

        assert!(std::ptr::eq(envelope.payload().as_ptr(), expected.as_ptr()));
    }

    #[test]
    fn rejects_payload_over_limit() {
        let oversized = vec![0; NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES + 1];
        let err = encode_snapshot_envelope(&oversized).expect_err("oversized payload should fail");

        assert_eq!(
            err,
            SnapshotFormatError::PayloadTooLarge {
                length: u64::try_from(oversized.len()).expect("fixture length should fit u64"),
                maximum: NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES,
            }
        );
    }

    #[test]
    fn rejects_every_minimum_length_truncation() {
        let encoded = encode_snapshot_envelope(&[]).expect("fixture should encode");

        for actual in 0..(SNAPSHOT_ENVELOPE_HEADER_BYTES + SNAPSHOT_ENVELOPE_INTEGRITY_BYTES) {
            let bytes = encoded.get(..actual).expect("fixture prefix should exist");
            assert_eq!(
                decode_snapshot_envelope(bytes),
                Err(SnapshotFormatError::Truncated {
                    expected: SNAPSHOT_ENVELOPE_HEADER_BYTES + SNAPSHOT_ENVELOPE_INTEGRITY_BYTES,
                    actual,
                })
            );
        }
    }

    #[test]
    fn rejects_invalid_magic() {
        let mut encoded = encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should encode");
        let magic_byte = encoded
            .get_mut(SNAPSHOT_MAGIC_OFFSET)
            .expect("fixture magic should exist");
        *magic_byte ^= 0xff;

        assert_eq!(
            decode_snapshot_envelope(&encoded),
            Err(SnapshotFormatError::InvalidMagic)
        );
    }

    #[test]
    fn rejects_declared_length_overflow_and_limit() {
        let overflow = with_u64_field(SNAPSHOT_PAYLOAD_LENGTH_OFFSET, u64::MAX);
        assert_eq!(
            decode_snapshot_envelope(&overflow),
            Err(SnapshotFormatError::PayloadLengthOverflow)
        );

        let over_limit = with_u64_field(
            SNAPSHOT_PAYLOAD_LENGTH_OFFSET,
            u64::try_from(NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES).expect("maximum should fit u64")
                + 1,
        );
        assert_eq!(
            decode_snapshot_envelope(&over_limit),
            Err(SnapshotFormatError::PayloadTooLarge {
                length: u64::try_from(NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES)
                    .expect("maximum should fit u64")
                    + 1,
                maximum: NATIVE_V1_SNAPSHOT_MAX_PAYLOAD_BYTES,
            })
        );
    }

    #[test]
    fn rejects_inconsistent_and_trailing_lengths() {
        let encoded = encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should encode");
        let truncated = encoded
            .get(..encoded.len() - 1)
            .expect("fixture prefix should exist");
        assert_eq!(
            decode_snapshot_envelope(truncated),
            Err(SnapshotFormatError::Truncated {
                expected: encoded.len(),
                actual: encoded.len() - 1,
            })
        );

        let mut trailing = encoded.clone();
        trailing.push(0);
        assert_eq!(
            decode_snapshot_envelope(&trailing),
            Err(SnapshotFormatError::TrailingData {
                expected: encoded.len(),
                actual: encoded.len() + 1,
            })
        );
    }

    #[test]
    fn rejects_corrupt_header_payload_and_checksum() {
        for offset in [
            SNAPSHOT_VERSION_MAJOR_OFFSET,
            SNAPSHOT_ENVELOPE_HEADER_BYTES,
            SNAPSHOT_ENVELOPE_HEADER_BYTES + TEST_PAYLOAD.len(),
        ] {
            let mut encoded =
                encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should encode");
            let byte = encoded.get_mut(offset).expect("fixture byte should exist");
            *byte ^= 0x80;

            assert_eq!(
                decode_snapshot_envelope(&encoded),
                Err(SnapshotFormatError::IntegrityMismatch)
            );
        }
    }

    #[test]
    fn rejects_valid_checksum_unsupported_metadata() {
        for (offset, value, expected) in [
            (
                SNAPSHOT_RESERVED_FLAGS_OFFSET,
                1_u32,
                SnapshotFormatError::UnsupportedFlags(1),
            ),
            (
                SNAPSHOT_GUEST_PAGE_SIZE_OFFSET,
                16_384_u32,
                SnapshotFormatError::IncompatibleGuestPageSize(16_384),
            ),
        ] {
            let encoded = with_u32_field(offset, value);
            assert_eq!(decode_snapshot_envelope(&encoded), Err(expected));
        }

        let encoded = with_u16_field(SNAPSHOT_VERSION_MINOR_OFFSET, 1);
        assert_eq!(
            decode_snapshot_envelope(&encoded),
            Err(SnapshotFormatError::UnsupportedVersion(
                SnapshotFormatVersion::new(1, 1, 0)
            ))
        );

        let encoded = with_u16_field(SNAPSHOT_ARCHITECTURE_OFFSET, 2);
        assert_eq!(
            decode_snapshot_envelope(&encoded),
            Err(SnapshotFormatError::IncompatibleArchitecture(2))
        );
    }

    #[test]
    fn version_architecture_integrity_and_metadata_display_stably() {
        assert_eq!(NATIVE_V1_SNAPSHOT_VERSION.to_string(), "1.0.0");
        assert_eq!(SnapshotArchitecture::Arm64.to_string(), "arm64");
        assert_eq!(SnapshotIntegrity::Crc64Jones.to_string(), "CRC-64/Jones");

        let encoded = encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should encode");
        let metadata = inspect_snapshot_envelope(&encoded).expect("fixture should inspect");
        assert_eq!(metadata.version().major(), 1);
        assert_eq!(metadata.version().minor(), 0);
        assert_eq!(metadata.version().patch(), 0);
        assert_eq!(metadata.architecture(), SnapshotArchitecture::Arm64);
        assert_eq!(metadata.guest_page_size(), 4096);
        assert_eq!(metadata.payload_length(), 5);
        assert_eq!(metadata.integrity(), SnapshotIntegrity::Crc64Jones);
    }

    #[test]
    fn diagnostics_redact_payload_bytes() {
        let sensitive = b"private-guest-state";
        let encoded = encode_snapshot_envelope(sensitive).expect("fixture should encode");
        let envelope = decode_snapshot_envelope(&encoded).expect("fixture should decode");
        let debug = format!("{envelope:?}");

        assert!(debug.contains(REDACTED));
        assert!(!debug.contains("private-guest-state"));

        let error = SnapshotFormatError::IntegrityMismatch;
        assert!(!error.to_string().contains("private-guest-state"));
        assert!(!format!("{error:?}").contains("private-guest-state"));
    }

    #[test]
    fn native_page_size_matches_runtime_memory_granule() {
        assert_eq!(
            u64::from(NATIVE_V1_GUEST_PAGE_SIZE),
            crate::memory::aarch64::GUEST_PAGE_SIZE
        );
    }

    fn with_u16_field(offset: usize, value: u16) -> Vec<u8> {
        let mut encoded = encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should encode");
        replace_field_and_checksum(&mut encoded, offset, &value.to_le_bytes());
        encoded
    }

    fn with_u32_field(offset: usize, value: u32) -> Vec<u8> {
        let mut encoded = encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should encode");
        replace_field_and_checksum(&mut encoded, offset, &value.to_le_bytes());
        encoded
    }

    fn with_u64_field(offset: usize, value: u64) -> Vec<u8> {
        let mut encoded = encode_snapshot_envelope(TEST_PAYLOAD).expect("fixture should encode");
        let end = offset + std::mem::size_of::<u64>();
        encoded
            .get_mut(offset..end)
            .expect("fixture field should exist")
            .copy_from_slice(&value.to_le_bytes());
        encoded
    }

    fn replace_field_and_checksum(encoded: &mut [u8], offset: usize, value: &[u8]) {
        let end = offset + value.len();
        encoded
            .get_mut(offset..end)
            .expect("fixture field should exist")
            .copy_from_slice(value);
        let checksum_offset = encoded.len() - SNAPSHOT_ENVELOPE_INTEGRITY_BYTES;
        let checksum = crc64(
            0,
            encoded
                .get(..checksum_offset)
                .expect("checksummed fixture bytes should exist"),
        );
        encoded
            .get_mut(checksum_offset..)
            .expect("fixture checksum should exist")
            .copy_from_slice(&checksum.to_le_bytes());
    }
}
