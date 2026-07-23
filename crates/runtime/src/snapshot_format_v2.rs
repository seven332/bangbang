//! Canonical bangbang-native v2 snapshot-state container.

use std::collections::TryReserveError;
use std::fmt;

use crc64::crc64;

use crate::snapshot_format::{SnapshotArchitecture, SnapshotFormatVersion, SnapshotIntegrity};

pub(crate) const NATIVE_V2_ARM64_MAGIC: [u8; 8] = *b"BANGV2A\0";
const MAGIC_OFFSET: usize = 0;
const VERSION_MAJOR_OFFSET: usize = 8;
const VERSION_MINOR_OFFSET: usize = 10;
const VERSION_PATCH_OFFSET: usize = 12;
const HEADER_BYTES_OFFSET: usize = 14;
const FLAGS_OFFSET: usize = 16;
const REQUIRED_FEATURE_COUNT_OFFSET: usize = 20;
const COMPONENT_COUNT_OFFSET: usize = 24;
const RESERVED_OFFSET: usize = 28;
const TOTAL_LENGTH_OFFSET: usize = 32;
const REQUIRED_FEATURE_OFFSET_OFFSET: usize = 40;
const COMPONENT_DIRECTORY_OFFSET_OFFSET: usize = 48;
const COMPONENT_PAYLOAD_OFFSET_OFFSET: usize = 56;
const HEADER_FLAGS: u32 = 0;
const HEADER_RESERVED: u32 = 0;
const COMPONENT_KIND_OFFSET: usize = 0;
const COMPONENT_INSTANCE_OFFSET: usize = 4;
const COMPONENT_FLAGS_OFFSET: usize = 8;
const COMPONENT_RESERVED_OFFSET: usize = 12;
const COMPONENT_PAYLOAD_OFFSET: usize = 16;
const COMPONENT_PAYLOAD_LENGTH_OFFSET: usize = 24;
const COMPONENT_RESERVED: u32 = 0;
const COMPONENT_FLAG_SEMANTIC: u32 = 0;
const COMPONENT_FLAG_NONSEMANTIC: u32 = 1;
const REDACTED: &str = "<redacted>";

/// Semantic version of the structural native-v2 foundation.
pub const NATIVE_V2_SNAPSHOT_VERSION: SnapshotFormatVersion = SnapshotFormatVersion::new(2, 0, 0);

/// Fixed native-v2 state header size.
pub const NATIVE_V2_SNAPSHOT_HEADER_BYTES: usize = 64;

/// Native-v2 state integrity trailer size.
pub const NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES: usize = 8;

/// Current maximum complete native-v2 state-file size.
pub const NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum required features admitted by the structural reader.
pub const NATIVE_V2_SNAPSHOT_MAX_REQUIRED_FEATURES: usize = 256;

/// Maximum component directory entries admitted by the structural reader.
pub const NATIVE_V2_SNAPSHOT_MAX_COMPONENTS: usize = 4096;

/// Fixed native-v2 component directory entry size.
pub const NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES: usize = 32;

#[derive(Clone, Copy)]
struct CatalogEntry {
    id: u32,
    introduced_minor: u16,
}

const PRODUCTION_REQUIRED_FEATURES: &[CatalogEntry] = &[];
const PRODUCTION_SEMANTIC_COMPONENTS: &[CatalogEntry] = &[];

const _: () = assert!(catalog_is_canonical(PRODUCTION_REQUIRED_FEATURES));
const _: () = assert!(catalog_is_canonical(PRODUCTION_SEMANTIC_COMPONENTS));

const fn catalog_is_canonical(mut entries: &[CatalogEntry]) -> bool {
    let mut previous = 0;
    while let [entry, remaining @ ..] = entries {
        if entry.id == 0 || entry.id <= previous {
            return false;
        }
        previous = entry.id;
        entries = remaining;
    }
    true
}

fn catalog_contains(entries: &[CatalogEntry], id: u32, encoded_minor: u16) -> bool {
    entries
        .binary_search_by_key(&id, |entry| entry.id)
        .ok()
        .and_then(|index| entries.get(index))
        .is_some_and(|entry| entry.introduced_minor <= encoded_minor)
}

/// Canonical component identity inside a native-v2 state container.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SnapshotV2ComponentKey {
    kind: u32,
    instance: u32,
}

impl SnapshotV2ComponentKey {
    /// Creates a component key. Encoding rejects kind zero.
    pub const fn new(kind: u32, instance: u32) -> Self {
        Self { kind, instance }
    }

    /// Returns the component kind to trusted typed-codec code.
    pub const fn kind(self) -> u32 {
        self.kind
    }

    /// Returns the component instance identifier to trusted typed-codec code.
    pub const fn instance(self) -> u32 {
        self.instance
    }
}

impl fmt::Debug for SnapshotV2ComponentKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2ComponentKey")
            .field("identity", &REDACTED)
            .finish()
    }
}

/// Whether a v2 component affects VM semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2ComponentDisposition {
    /// The component must be understood before a VM can use the state.
    Semantic,
    /// The component is explicitly safe to ignore after structural validation.
    NonSemantic,
}

impl SnapshotV2ComponentDisposition {
    const fn flags(self) -> u32 {
        match self {
            Self::Semantic => COMPONENT_FLAG_SEMANTIC,
            Self::NonSemantic => COMPONENT_FLAG_NONSEMANTIC,
        }
    }
}

/// One trusted encoding input or validated borrowed v2 component.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2Component<'payload> {
    key: SnapshotV2ComponentKey,
    disposition: SnapshotV2ComponentDisposition,
    payload: &'payload [u8],
}

impl<'payload> SnapshotV2Component<'payload> {
    /// Creates a component encoding input.
    pub const fn new(
        key: SnapshotV2ComponentKey,
        disposition: SnapshotV2ComponentDisposition,
        payload: &'payload [u8],
    ) -> Self {
        Self {
            key,
            disposition,
            payload,
        }
    }

    /// Returns the validated component key.
    pub const fn key(self) -> SnapshotV2ComponentKey {
        self.key
    }

    /// Returns whether the component affects VM semantics.
    pub const fn disposition(self) -> SnapshotV2ComponentDisposition {
        self.disposition
    }

    /// Returns the validated borrowed component payload.
    pub const fn payload(self) -> &'payload [u8] {
        self.payload
    }
}

impl fmt::Debug for SnapshotV2Component<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2Component")
            .field("key", &REDACTED)
            .field("disposition", &self.disposition)
            .field("payload", &REDACTED)
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

/// Stable non-sensitive metadata from a validated native-v2 container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2Metadata {
    version: SnapshotFormatVersion,
    architecture: SnapshotArchitecture,
    required_feature_count: u32,
    component_count: u32,
    total_length: u64,
    integrity: SnapshotIntegrity,
}

impl SnapshotV2Metadata {
    /// Returns the embedded semantic format version.
    pub const fn version(self) -> SnapshotFormatVersion {
        self.version
    }

    /// Returns the architecture identified by the format magic.
    pub const fn architecture(self) -> SnapshotArchitecture {
        self.architecture
    }

    /// Returns the bounded required-feature count.
    pub const fn required_feature_count(self) -> u32 {
        self.required_feature_count
    }

    /// Returns the bounded component count.
    pub const fn component_count(self) -> u32 {
        self.component_count
    }

    /// Returns the exact complete state-file size.
    pub const fn total_length(self) -> u64 {
        self.total_length
    }

    /// Returns the state-file integrity algorithm.
    pub const fn integrity(self) -> SnapshotIntegrity {
        self.integrity
    }
}

/// A compatible native-v2 state container borrowing all source bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2State<'state> {
    bytes: &'state [u8],
    metadata: SnapshotV2Metadata,
    required_feature_offset: usize,
    component_directory_offset: usize,
    component_payload_offset: usize,
}

impl SnapshotV2State<'_> {
    /// Returns stable non-sensitive metadata.
    pub const fn metadata(&self) -> SnapshotV2Metadata {
        self.metadata
    }

    /// Iterates over already validated required-feature identifiers.
    pub fn required_features(&self) -> SnapshotV2RequiredFeatures<'_> {
        SnapshotV2RequiredFeatures {
            bytes: self.bytes,
            position: self.required_feature_offset,
            remaining: self.metadata.required_feature_count as usize,
        }
    }

    /// Iterates over already validated borrowed components.
    pub fn components(&self) -> SnapshotV2Components<'_> {
        SnapshotV2Components {
            bytes: self.bytes,
            directory_position: self.component_directory_offset,
            remaining: self.metadata.component_count as usize,
        }
    }

    /// Finds one already validated component by its trusted key.
    pub fn component(&self, key: SnapshotV2ComponentKey) -> Option<SnapshotV2Component<'_>> {
        self.components().find(|component| component.key == key)
    }

    /// Returns the byte offset at which component payloads begin.
    pub const fn component_payload_offset(&self) -> usize {
        self.component_payload_offset
    }
}

impl fmt::Debug for SnapshotV2State<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2State")
            .field("metadata", &self.metadata)
            .field("required_features", &REDACTED)
            .field("components", &REDACTED)
            .finish()
    }
}

/// Borrowed iterator over a validated required-feature table.
#[derive(Clone)]
pub struct SnapshotV2RequiredFeatures<'state> {
    bytes: &'state [u8],
    position: usize,
    remaining: usize,
}

impl Iterator for SnapshotV2RequiredFeatures<'_> {
    type Item = u32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let value = read_u32_option(self.bytes, self.position)?;
        self.position = self.position.checked_add(size_of::<u32>())?;
        self.remaining -= 1;
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for SnapshotV2RequiredFeatures<'_> {}

impl fmt::Debug for SnapshotV2RequiredFeatures<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2RequiredFeatures")
            .field("remaining", &self.remaining)
            .field("values", &REDACTED)
            .finish()
    }
}

/// Borrowed iterator over a validated component directory.
#[derive(Clone)]
pub struct SnapshotV2Components<'state> {
    bytes: &'state [u8],
    directory_position: usize,
    remaining: usize,
}

impl<'state> Iterator for SnapshotV2Components<'state> {
    type Item = SnapshotV2Component<'state>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let entry = self.bytes.get(
            self.directory_position
                ..self
                    .directory_position
                    .checked_add(NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES)?,
        )?;
        let key = SnapshotV2ComponentKey::new(
            read_u32_option(entry, COMPONENT_KIND_OFFSET)?,
            read_u32_option(entry, COMPONENT_INSTANCE_OFFSET)?,
        );
        let disposition = match read_u32_option(entry, COMPONENT_FLAGS_OFFSET)? {
            COMPONENT_FLAG_SEMANTIC => SnapshotV2ComponentDisposition::Semantic,
            COMPONENT_FLAG_NONSEMANTIC => SnapshotV2ComponentDisposition::NonSemantic,
            _ => return None,
        };
        let payload_offset =
            usize::try_from(read_u64_option(entry, COMPONENT_PAYLOAD_OFFSET)?).ok()?;
        let payload_length =
            usize::try_from(read_u64_option(entry, COMPONENT_PAYLOAD_LENGTH_OFFSET)?).ok()?;
        let payload_end = payload_offset.checked_add(payload_length)?;
        let payload = self.bytes.get(payload_offset..payload_end)?;

        self.directory_position = self
            .directory_position
            .checked_add(NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES)?;
        self.remaining -= 1;
        Some(SnapshotV2Component::new(key, disposition, payload))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for SnapshotV2Components<'_> {}

impl fmt::Debug for SnapshotV2Components<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2Components")
            .field("remaining", &self.remaining)
            .field("values", &REDACTED)
            .finish()
    }
}

/// Native-v2 structural or compatibility validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotV2DecodeError {
    /// Input is smaller than the fixed header and integrity trailer.
    Truncated { minimum: usize, actual: usize },
    /// The input is not an arm64 native-v2 container.
    InvalidMagic,
    /// The semantic major is unsupported or the minor is newer than this reader.
    UnsupportedVersion {
        found: SnapshotFormatVersion,
        supported: SnapshotFormatVersion,
    },
    /// Fixed header fields are noncanonical.
    InvalidHeader,
    /// A required-feature or component count exceeds reader policy.
    CountOutOfBounds { count: u64, maximum: usize },
    /// A declared length or offset cannot be represented safely.
    LengthOverflow,
    /// The complete input length differs from the declared total.
    LengthMismatch { declared: u64, actual: usize },
    /// The complete state file exceeds reader policy.
    FileTooLarge { length: u64, maximum: usize },
    /// State CRC-64/Jones validation failed.
    IntegrityMismatch,
    /// Required-feature entries are zero, duplicated, or not canonical.
    InvalidRequiredFeatureInventory,
    /// The container requires behavior not supported by this reader.
    UnknownRequiredFeature,
    /// Component entries, flags, ordering, or ranges are invalid.
    InvalidComponentDirectory,
    /// A semantic component kind is not supported by this reader.
    UnknownSemanticComponent,
}

impl fmt::Display for SnapshotV2DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { minimum, actual } => write!(
                formatter,
                "native-v2 state is truncated: requires at least {minimum} bytes, found {actual}"
            ),
            Self::InvalidMagic => {
                formatter.write_str("native-v2 state architecture magic is invalid")
            }
            Self::UnsupportedVersion { found, supported } => write!(
                formatter,
                "native-v2 state version {found} is unsupported by {supported}"
            ),
            Self::InvalidHeader => formatter.write_str("native-v2 state header is noncanonical"),
            Self::CountOutOfBounds { count, maximum } => write!(
                formatter,
                "native-v2 state metadata count {count} exceeds {maximum}"
            ),
            Self::LengthOverflow => {
                formatter.write_str("native-v2 state length arithmetic overflowed")
            }
            Self::LengthMismatch { declared, actual } => write!(
                formatter,
                "native-v2 state declares {declared} bytes but contains {actual}"
            ),
            Self::FileTooLarge { length, maximum } => write!(
                formatter,
                "native-v2 state length {length} exceeds {maximum} byte limit"
            ),
            Self::IntegrityMismatch => {
                formatter.write_str("native-v2 state CRC-64/Jones integrity check failed")
            }
            Self::InvalidRequiredFeatureInventory => {
                formatter.write_str("native-v2 required-feature inventory is noncanonical")
            }
            Self::UnknownRequiredFeature => {
                formatter.write_str("native-v2 state requires unsupported behavior")
            }
            Self::InvalidComponentDirectory => {
                formatter.write_str("native-v2 component directory is noncanonical")
            }
            Self::UnknownSemanticComponent => {
                formatter.write_str("native-v2 state contains an unsupported semantic component")
            }
        }
    }
}

impl std::error::Error for SnapshotV2DecodeError {}

/// Native-v2 canonical encoding failure.
#[derive(Debug)]
pub enum SnapshotV2EncodeError {
    /// A required-feature or component count exceeds writer policy.
    CountOutOfBounds { count: usize, maximum: usize },
    /// Required-feature inputs are zero, duplicated, or not canonical.
    InvalidRequiredFeatureInventory,
    /// A required feature is not declared by the current production catalog.
    UnknownRequiredFeature,
    /// Component inputs, flags, ordering, or payload lengths are invalid.
    InvalidComponentDirectory,
    /// A semantic component is not declared by the current production catalog.
    UnknownSemanticComponent,
    /// Encoded length arithmetic overflowed.
    LengthOverflow,
    /// The encoded state would exceed writer policy.
    FileTooLarge { length: u64, maximum: usize },
    /// The canonical output buffer could not be allocated.
    AllocationFailed { source: TryReserveError },
}

impl fmt::Display for SnapshotV2EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CountOutOfBounds { count, maximum } => write!(
                formatter,
                "native-v2 state metadata count {count} exceeds {maximum}"
            ),
            Self::InvalidRequiredFeatureInventory => {
                formatter.write_str("native-v2 required-feature inventory is noncanonical")
            }
            Self::UnknownRequiredFeature => {
                formatter.write_str("native-v2 state requires undeclared behavior")
            }
            Self::InvalidComponentDirectory => {
                formatter.write_str("native-v2 component inputs are noncanonical")
            }
            Self::UnknownSemanticComponent => {
                formatter.write_str("native-v2 semantic component is undeclared")
            }
            Self::LengthOverflow => {
                formatter.write_str("native-v2 state length arithmetic overflowed")
            }
            Self::FileTooLarge { length, maximum } => write!(
                formatter,
                "native-v2 state length {length} exceeds {maximum} byte limit"
            ),
            Self::AllocationFailed { .. } => {
                formatter.write_str("native-v2 state output allocation failed")
            }
        }
    }
}

impl std::error::Error for SnapshotV2EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocationFailed { source } => Some(source),
            _ => None,
        }
    }
}

/// Decodes and fully validates the current production native-v2 profile.
pub fn decode_snapshot_v2_state(
    bytes: &[u8],
) -> Result<SnapshotV2State<'_>, SnapshotV2DecodeError> {
    decode_snapshot_v2_state_with_catalog(
        bytes,
        NATIVE_V2_SNAPSHOT_VERSION,
        PRODUCTION_REQUIRED_FEATURES,
        PRODUCTION_SEMANTIC_COMPONENTS,
    )
}

/// Encodes the current production native-v2 profile canonically.
pub fn encode_snapshot_v2_state(
    required_features: &[u32],
    components: &[SnapshotV2Component<'_>],
) -> Result<Vec<u8>, SnapshotV2EncodeError> {
    encode_snapshot_v2_state_with_catalog_and_reserve(
        required_features,
        components,
        NATIVE_V2_SNAPSHOT_VERSION,
        PRODUCTION_REQUIRED_FEATURES,
        PRODUCTION_SEMANTIC_COMPONENTS,
        Vec::try_reserve_exact,
    )
}

fn decode_snapshot_v2_state_with_catalog<'state>(
    bytes: &'state [u8],
    supported_version: SnapshotFormatVersion,
    required_feature_catalog: &[CatalogEntry],
    semantic_component_catalog: &[CatalogEntry],
) -> Result<SnapshotV2State<'state>, SnapshotV2DecodeError> {
    let minimum = NATIVE_V2_SNAPSHOT_HEADER_BYTES + NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES;
    if bytes.len() < minimum {
        return Err(SnapshotV2DecodeError::Truncated {
            minimum,
            actual: bytes.len(),
        });
    }
    if read_array::<8>(bytes, MAGIC_OFFSET)? != NATIVE_V2_ARM64_MAGIC {
        return Err(SnapshotV2DecodeError::InvalidMagic);
    }

    let version = SnapshotFormatVersion::new(
        read_u16(bytes, VERSION_MAJOR_OFFSET)?,
        read_u16(bytes, VERSION_MINOR_OFFSET)?,
        read_u16(bytes, VERSION_PATCH_OFFSET)?,
    );
    if version.major() != supported_version.major() || version.minor() > supported_version.minor() {
        return Err(SnapshotV2DecodeError::UnsupportedVersion {
            found: version,
            supported: supported_version,
        });
    }
    if usize::from(read_u16(bytes, HEADER_BYTES_OFFSET)?) != NATIVE_V2_SNAPSHOT_HEADER_BYTES
        || read_u32(bytes, FLAGS_OFFSET)? != HEADER_FLAGS
        || read_u32(bytes, RESERVED_OFFSET)? != HEADER_RESERVED
    {
        return Err(SnapshotV2DecodeError::InvalidHeader);
    }

    let required_feature_count = read_u32(bytes, REQUIRED_FEATURE_COUNT_OFFSET)?;
    let component_count = read_u32(bytes, COMPONENT_COUNT_OFFSET)?;
    validate_decode_count(
        u64::from(required_feature_count),
        NATIVE_V2_SNAPSHOT_MAX_REQUIRED_FEATURES,
    )?;
    validate_decode_count(
        u64::from(component_count),
        NATIVE_V2_SNAPSHOT_MAX_COMPONENTS,
    )?;

    let total_length = read_u64(bytes, TOTAL_LENGTH_OFFSET)?;
    if total_length
        > u64::try_from(NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES)
            .map_err(|_| SnapshotV2DecodeError::LengthOverflow)?
    {
        return Err(SnapshotV2DecodeError::FileTooLarge {
            length: total_length,
            maximum: NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES,
        });
    }
    let total_length_usize =
        usize::try_from(total_length).map_err(|_| SnapshotV2DecodeError::LengthOverflow)?;
    if total_length_usize != bytes.len() {
        return Err(SnapshotV2DecodeError::LengthMismatch {
            declared: total_length,
            actual: bytes.len(),
        });
    }

    let required_feature_offset = usize::try_from(read_u64(bytes, REQUIRED_FEATURE_OFFSET_OFFSET)?)
        .map_err(|_| SnapshotV2DecodeError::LengthOverflow)?;
    let component_directory_offset =
        usize::try_from(read_u64(bytes, COMPONENT_DIRECTORY_OFFSET_OFFSET)?)
            .map_err(|_| SnapshotV2DecodeError::LengthOverflow)?;
    let component_payload_offset =
        usize::try_from(read_u64(bytes, COMPONENT_PAYLOAD_OFFSET_OFFSET)?)
            .map_err(|_| SnapshotV2DecodeError::LengthOverflow)?;
    let expected_directory_offset = NATIVE_V2_SNAPSHOT_HEADER_BYTES
        .checked_add(
            usize::try_from(required_feature_count)
                .map_err(|_| SnapshotV2DecodeError::LengthOverflow)?
                .checked_mul(size_of::<u32>())
                .ok_or(SnapshotV2DecodeError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2DecodeError::LengthOverflow)?;
    let expected_payload_offset = expected_directory_offset
        .checked_add(
            usize::try_from(component_count)
                .map_err(|_| SnapshotV2DecodeError::LengthOverflow)?
                .checked_mul(NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES)
                .ok_or(SnapshotV2DecodeError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2DecodeError::LengthOverflow)?;
    let checksum_offset = total_length_usize
        .checked_sub(NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES)
        .ok_or(SnapshotV2DecodeError::LengthOverflow)?;
    if required_feature_offset != NATIVE_V2_SNAPSHOT_HEADER_BYTES
        || component_directory_offset != expected_directory_offset
        || component_payload_offset != expected_payload_offset
        || component_payload_offset > checksum_offset
    {
        return Err(SnapshotV2DecodeError::InvalidHeader);
    }

    let stored_checksum = read_u64(bytes, checksum_offset)?;
    let checksummed = bytes
        .get(..checksum_offset)
        .ok_or(SnapshotV2DecodeError::LengthOverflow)?;
    if crc64(0, checksummed) != stored_checksum {
        return Err(SnapshotV2DecodeError::IntegrityMismatch);
    }

    validate_required_features(
        bytes,
        required_feature_offset,
        required_feature_count,
        version.minor(),
        required_feature_catalog,
    )?;
    validate_component_directory(
        bytes,
        component_directory_offset,
        component_payload_offset,
        checksum_offset,
        component_count,
        version.minor(),
        semantic_component_catalog,
    )?;

    Ok(SnapshotV2State {
        bytes,
        metadata: SnapshotV2Metadata {
            version,
            architecture: SnapshotArchitecture::Arm64,
            required_feature_count,
            component_count,
            total_length,
            integrity: SnapshotIntegrity::Crc64Jones,
        },
        required_feature_offset,
        component_directory_offset,
        component_payload_offset,
    })
}

fn validate_required_features(
    bytes: &[u8],
    mut position: usize,
    count: u32,
    encoded_minor: u16,
    catalog: &[CatalogEntry],
) -> Result<(), SnapshotV2DecodeError> {
    let mut previous = 0;
    for _ in 0..count {
        let feature = read_u32(bytes, position)?;
        if feature == 0 || feature <= previous {
            return Err(SnapshotV2DecodeError::InvalidRequiredFeatureInventory);
        }
        if !catalog_contains(catalog, feature, encoded_minor) {
            return Err(SnapshotV2DecodeError::UnknownRequiredFeature);
        }
        previous = feature;
        position = position
            .checked_add(size_of::<u32>())
            .ok_or(SnapshotV2DecodeError::LengthOverflow)?;
    }
    Ok(())
}

fn validate_component_directory(
    bytes: &[u8],
    mut directory_position: usize,
    component_payload_offset: usize,
    checksum_offset: usize,
    count: u32,
    encoded_minor: u16,
    catalog: &[CatalogEntry],
) -> Result<(), SnapshotV2DecodeError> {
    let mut previous_key = None;
    let mut expected_payload_offset = component_payload_offset;
    for _ in 0..count {
        let entry_end = directory_position
            .checked_add(NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES)
            .ok_or(SnapshotV2DecodeError::LengthOverflow)?;
        let entry = bytes
            .get(directory_position..entry_end)
            .ok_or(SnapshotV2DecodeError::InvalidComponentDirectory)?;
        let key = SnapshotV2ComponentKey::new(
            read_u32(entry, COMPONENT_KIND_OFFSET)?,
            read_u32(entry, COMPONENT_INSTANCE_OFFSET)?,
        );
        if key.kind == 0 || previous_key.is_some_and(|previous| key <= previous) {
            return Err(SnapshotV2DecodeError::InvalidComponentDirectory);
        }
        let flags = read_u32(entry, COMPONENT_FLAGS_OFFSET)?;
        if flags != COMPONENT_FLAG_SEMANTIC && flags != COMPONENT_FLAG_NONSEMANTIC {
            return Err(SnapshotV2DecodeError::InvalidComponentDirectory);
        }
        if read_u32(entry, COMPONENT_RESERVED_OFFSET)? != COMPONENT_RESERVED {
            return Err(SnapshotV2DecodeError::InvalidComponentDirectory);
        }
        let payload_offset = usize::try_from(read_u64(entry, COMPONENT_PAYLOAD_OFFSET)?)
            .map_err(|_| SnapshotV2DecodeError::LengthOverflow)?;
        let payload_length = usize::try_from(read_u64(entry, COMPONENT_PAYLOAD_LENGTH_OFFSET)?)
            .map_err(|_| SnapshotV2DecodeError::LengthOverflow)?;
        if payload_length == 0 || payload_offset != expected_payload_offset {
            return Err(SnapshotV2DecodeError::InvalidComponentDirectory);
        }
        expected_payload_offset = payload_offset
            .checked_add(payload_length)
            .ok_or(SnapshotV2DecodeError::LengthOverflow)?;
        if expected_payload_offset > checksum_offset {
            return Err(SnapshotV2DecodeError::InvalidComponentDirectory);
        }
        if flags == COMPONENT_FLAG_SEMANTIC && !catalog_contains(catalog, key.kind, encoded_minor) {
            return Err(SnapshotV2DecodeError::UnknownSemanticComponent);
        }
        previous_key = Some(key);
        directory_position = entry_end;
    }
    if expected_payload_offset != checksum_offset {
        return Err(SnapshotV2DecodeError::InvalidComponentDirectory);
    }
    Ok(())
}

fn validate_decode_count(count: u64, maximum: usize) -> Result<(), SnapshotV2DecodeError> {
    if count > u64::try_from(maximum).map_err(|_| SnapshotV2DecodeError::LengthOverflow)? {
        Err(SnapshotV2DecodeError::CountOutOfBounds { count, maximum })
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct EncodeLayout {
    component_directory_offset: usize,
    component_payload_offset: usize,
    total_length: usize,
}

fn encode_snapshot_v2_state_with_catalog_and_reserve(
    required_features: &[u32],
    components: &[SnapshotV2Component<'_>],
    version: SnapshotFormatVersion,
    required_feature_catalog: &[CatalogEntry],
    semantic_component_catalog: &[CatalogEntry],
    reserve: impl FnOnce(&mut Vec<u8>, usize) -> Result<(), TryReserveError>,
) -> Result<Vec<u8>, SnapshotV2EncodeError> {
    let layout = validate_encode_inputs(
        required_features,
        components,
        version,
        required_feature_catalog,
        semantic_component_catalog,
    )?;
    let mut bytes = Vec::new();
    reserve(&mut bytes, layout.total_length)
        .map_err(|source| SnapshotV2EncodeError::AllocationFailed { source })?;

    bytes.extend_from_slice(&NATIVE_V2_ARM64_MAGIC);
    bytes.extend_from_slice(&version.major().to_le_bytes());
    bytes.extend_from_slice(&version.minor().to_le_bytes());
    bytes.extend_from_slice(&version.patch().to_le_bytes());
    bytes.extend_from_slice(
        &u16::try_from(NATIVE_V2_SNAPSHOT_HEADER_BYTES)
            .map_err(|_| SnapshotV2EncodeError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&HEADER_FLAGS.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(required_features.len())
            .map_err(|_| SnapshotV2EncodeError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(components.len())
            .map_err(|_| SnapshotV2EncodeError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&HEADER_RESERVED.to_le_bytes());
    bytes.extend_from_slice(
        &u64::try_from(layout.total_length)
            .map_err(|_| SnapshotV2EncodeError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u64::try_from(NATIVE_V2_SNAPSHOT_HEADER_BYTES)
            .map_err(|_| SnapshotV2EncodeError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u64::try_from(layout.component_directory_offset)
            .map_err(|_| SnapshotV2EncodeError::LengthOverflow)?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u64::try_from(layout.component_payload_offset)
            .map_err(|_| SnapshotV2EncodeError::LengthOverflow)?
            .to_le_bytes(),
    );
    for feature in required_features {
        bytes.extend_from_slice(&feature.to_le_bytes());
    }

    let mut payload_offset = layout.component_payload_offset;
    for component in components {
        bytes.extend_from_slice(&component.key.kind.to_le_bytes());
        bytes.extend_from_slice(&component.key.instance.to_le_bytes());
        bytes.extend_from_slice(&component.disposition.flags().to_le_bytes());
        bytes.extend_from_slice(&COMPONENT_RESERVED.to_le_bytes());
        bytes.extend_from_slice(
            &u64::try_from(payload_offset)
                .map_err(|_| SnapshotV2EncodeError::LengthOverflow)?
                .to_le_bytes(),
        );
        bytes.extend_from_slice(
            &u64::try_from(component.payload.len())
                .map_err(|_| SnapshotV2EncodeError::LengthOverflow)?
                .to_le_bytes(),
        );
        payload_offset = payload_offset
            .checked_add(component.payload.len())
            .ok_or(SnapshotV2EncodeError::LengthOverflow)?;
    }
    for component in components {
        bytes.extend_from_slice(component.payload);
    }
    let checksum = crc64(0, &bytes);
    bytes.extend_from_slice(&checksum.to_le_bytes());
    debug_assert_eq!(bytes.len(), layout.total_length);
    Ok(bytes)
}

fn validate_encode_inputs(
    required_features: &[u32],
    components: &[SnapshotV2Component<'_>],
    version: SnapshotFormatVersion,
    required_feature_catalog: &[CatalogEntry],
    semantic_component_catalog: &[CatalogEntry],
) -> Result<EncodeLayout, SnapshotV2EncodeError> {
    validate_encode_count(
        required_features.len(),
        NATIVE_V2_SNAPSHOT_MAX_REQUIRED_FEATURES,
    )?;
    validate_encode_count(components.len(), NATIVE_V2_SNAPSHOT_MAX_COMPONENTS)?;

    let mut previous_feature = 0;
    for feature in required_features {
        if *feature == 0 || *feature <= previous_feature {
            return Err(SnapshotV2EncodeError::InvalidRequiredFeatureInventory);
        }
        if !catalog_contains(required_feature_catalog, *feature, version.minor()) {
            return Err(SnapshotV2EncodeError::UnknownRequiredFeature);
        }
        previous_feature = *feature;
    }

    let mut previous_key = None;
    let mut payload_bytes = 0usize;
    for component in components {
        if component.key.kind == 0
            || previous_key.is_some_and(|previous| component.key <= previous)
            || component.payload.is_empty()
        {
            return Err(SnapshotV2EncodeError::InvalidComponentDirectory);
        }
        if component.disposition == SnapshotV2ComponentDisposition::Semantic
            && !catalog_contains(
                semantic_component_catalog,
                component.key.kind,
                version.minor(),
            )
        {
            return Err(SnapshotV2EncodeError::UnknownSemanticComponent);
        }
        payload_bytes = payload_bytes
            .checked_add(component.payload.len())
            .ok_or(SnapshotV2EncodeError::LengthOverflow)?;
        previous_key = Some(component.key);
    }

    let component_directory_offset = NATIVE_V2_SNAPSHOT_HEADER_BYTES
        .checked_add(
            required_features
                .len()
                .checked_mul(size_of::<u32>())
                .ok_or(SnapshotV2EncodeError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2EncodeError::LengthOverflow)?;
    let component_payload_offset = component_directory_offset
        .checked_add(
            components
                .len()
                .checked_mul(NATIVE_V2_COMPONENT_DIRECTORY_ENTRY_BYTES)
                .ok_or(SnapshotV2EncodeError::LengthOverflow)?,
        )
        .ok_or(SnapshotV2EncodeError::LengthOverflow)?;
    let total_length = component_payload_offset
        .checked_add(payload_bytes)
        .and_then(|length| length.checked_add(NATIVE_V2_SNAPSHOT_INTEGRITY_BYTES))
        .ok_or(SnapshotV2EncodeError::LengthOverflow)?;
    let total_length_u64 =
        u64::try_from(total_length).map_err(|_| SnapshotV2EncodeError::LengthOverflow)?;
    if total_length > NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES {
        return Err(SnapshotV2EncodeError::FileTooLarge {
            length: total_length_u64,
            maximum: NATIVE_V2_SNAPSHOT_MAX_FILE_BYTES,
        });
    }
    Ok(EncodeLayout {
        component_directory_offset,
        component_payload_offset,
        total_length,
    })
}

fn validate_encode_count(count: usize, maximum: usize) -> Result<(), SnapshotV2EncodeError> {
    if count > maximum {
        Err(SnapshotV2EncodeError::CountOutOfBounds { count, maximum })
    } else {
        Ok(())
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, SnapshotV2DecodeError> {
    Ok(u16::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, SnapshotV2DecodeError> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, SnapshotV2DecodeError> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

fn read_array<const LENGTH: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; LENGTH], SnapshotV2DecodeError> {
    let end = offset
        .checked_add(LENGTH)
        .ok_or(SnapshotV2DecodeError::LengthOverflow)?;
    let source = bytes
        .get(offset..end)
        .ok_or(SnapshotV2DecodeError::Truncated {
            minimum: end,
            actual: bytes.len(),
        })?;
    let mut result = [0; LENGTH];
    result.copy_from_slice(source);
    Ok(result)
}

fn read_u32_option(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(size_of::<u32>())?;
    let source = bytes.get(offset..end)?;
    let mut value = [0; size_of::<u32>()];
    value.copy_from_slice(source);
    Some(u32::from_le_bytes(value))
}

fn read_u64_option(bytes: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(size_of::<u64>())?;
    let source = bytes.get(offset..end)?;
    let mut value = [0; size_of::<u64>()];
    value.copy_from_slice(source);
    Some(u64::from_le_bytes(value))
}

#[cfg(test)]
mod tests;
