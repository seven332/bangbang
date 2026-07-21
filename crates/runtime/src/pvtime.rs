//! Backend-neutral arm64 stolen-time ABI and guest-memory placement.

use std::collections::TryReserveError;
use std::fmt;

use crate::memory::{GuestAddress, GuestMemoryError, GuestMemoryRange};

/// Standard arm64 PVTime shared-structure size in bytes.
pub const ARM64_PVTIME_STRUCTURE_SIZE: usize = 64;
/// Required guest-physical alignment of each arm64 PVTime structure.
pub const ARM64_PVTIME_STRUCTURE_ALIGNMENT: u64 = 64;
/// Current standard stolen-time structure revision.
pub const ARM64_PVTIME_REVISION: u32 = 0;
/// Current standard stolen-time structure attributes.
pub const ARM64_PVTIME_ATTRIBUTES: u32 = 0;

const ARM64_PVTIME_STOLEN_TIME_OFFSET: usize = 8;
const ARM64_PVTIME_PADDING_OFFSET: usize = 16;

/// Exact standard arm64 PVTime stolen-time structure contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Arm64PvTimeStAbi {
    revision: u32,
    attributes: u32,
    stolen_time_ns: u64,
}

impl Arm64PvTimeStAbi {
    /// Return the initial standard structure.
    pub const fn initial() -> Self {
        Self {
            revision: ARM64_PVTIME_REVISION,
            attributes: ARM64_PVTIME_ATTRIBUTES,
            stolen_time_ns: 0,
        }
    }

    /// Return the structure revision.
    pub const fn revision(self) -> u32 {
        self.revision
    }

    /// Return the structure attributes.
    pub const fn attributes(self) -> u32 {
        self.attributes
    }

    /// Return cumulative stolen time in nanoseconds.
    pub const fn stolen_time_ns(self) -> u64 {
        self.stolen_time_ns
    }

    /// Return the exact 64-byte little-endian guest representation.
    pub fn to_bytes(self) -> [u8; ARM64_PVTIME_STRUCTURE_SIZE] {
        let mut bytes = [0; ARM64_PVTIME_STRUCTURE_SIZE];
        bytes[0..4].copy_from_slice(&self.revision.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.attributes.to_le_bytes());
        bytes[ARM64_PVTIME_STOLEN_TIME_OFFSET..ARM64_PVTIME_PADDING_OFFSET]
            .copy_from_slice(&self.stolen_time_ns.to_le_bytes());
        bytes
    }

    /// Decode and validate one exact standard guest representation.
    pub fn from_bytes(
        bytes: [u8; ARM64_PVTIME_STRUCTURE_SIZE],
    ) -> Result<Self, Arm64PvTimeAbiError> {
        let revision = u32::from_le_bytes(
            bytes[0..4]
                .try_into()
                .map_err(|_| Arm64PvTimeAbiError::InvalidLength)?,
        );
        if revision != ARM64_PVTIME_REVISION {
            return Err(Arm64PvTimeAbiError::UnsupportedRevision);
        }
        let attributes = u32::from_le_bytes(
            bytes[4..8]
                .try_into()
                .map_err(|_| Arm64PvTimeAbiError::InvalidLength)?,
        );
        if attributes != ARM64_PVTIME_ATTRIBUTES {
            return Err(Arm64PvTimeAbiError::UnsupportedAttributes);
        }
        if bytes[ARM64_PVTIME_PADDING_OFFSET..]
            .iter()
            .any(|byte| *byte != 0)
        {
            return Err(Arm64PvTimeAbiError::NonzeroPadding);
        }
        let stolen_time_ns = u64::from_le_bytes(
            bytes[ARM64_PVTIME_STOLEN_TIME_OFFSET..ARM64_PVTIME_PADDING_OFFSET]
                .try_into()
                .map_err(|_| Arm64PvTimeAbiError::InvalidLength)?,
        );
        Ok(Self {
            revision,
            attributes,
            stolen_time_ns,
        })
    }
}

/// Invalid standard arm64 PVTime structure contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm64PvTimeAbiError {
    InvalidLength,
    UnsupportedRevision,
    UnsupportedAttributes,
    NonzeroPadding,
}

impl fmt::Display for Arm64PvTimeAbiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidLength => "arm64 PVTime structure length is invalid",
            Self::UnsupportedRevision => "arm64 PVTime structure revision is unsupported",
            Self::UnsupportedAttributes => "arm64 PVTime structure attributes are unsupported",
            Self::NonzeroPadding => "arm64 PVTime structure padding is nonzero",
        };
        f.write_str(message)
    }
}

impl std::error::Error for Arm64PvTimeAbiError {}

/// Bounded, topology-ordered arm64 PVTime guest-memory placement.
#[derive(Clone, PartialEq, Eq)]
pub struct Arm64PvTimeLayout {
    records: Vec<GuestMemoryRange>,
}

impl Arm64PvTimeLayout {
    /// Plan one aligned structure per vCPU at the high end of `arena`.
    pub fn plan(vcpu_count: u8, arena: GuestMemoryRange) -> Result<Self, Arm64PvTimeLayoutError> {
        if vcpu_count == 0 {
            return Err(Arm64PvTimeLayoutError::EmptyTopology);
        }

        let record_size = ARM64_PVTIME_STRUCTURE_SIZE as u64;
        let total_size = record_size
            .checked_mul(u64::from(vcpu_count))
            .ok_or(Arm64PvTimeLayoutError::SizeOverflow)?;
        let aligned_end =
            arena.end_exclusive().raw_value() & !(ARM64_PVTIME_STRUCTURE_ALIGNMENT - 1);
        let Some(base) = aligned_end.checked_sub(total_size) else {
            return Err(Arm64PvTimeLayoutError::InsufficientSpace);
        };
        if base < arena.start().raw_value() {
            return Err(Arm64PvTimeLayoutError::InsufficientSpace);
        }

        let mut records = Vec::new();
        records
            .try_reserve_exact(usize::from(vcpu_count))
            .map_err(Arm64PvTimeLayoutError::MetadataAllocation)?;
        for index in 0..vcpu_count {
            let offset = record_size
                .checked_mul(u64::from(index))
                .ok_or(Arm64PvTimeLayoutError::SizeOverflow)?;
            let address = base
                .checked_add(offset)
                .ok_or(Arm64PvTimeLayoutError::AddressOverflow)?;
            let range = GuestMemoryRange::new(GuestAddress::new(address), record_size)
                .map_err(Arm64PvTimeLayoutError::InvalidRange)?;
            range
                .validate_alignment(ARM64_PVTIME_STRUCTURE_ALIGNMENT)
                .map_err(Arm64PvTimeLayoutError::InvalidRange)?;
            records.push(range);
        }

        Ok(Self { records })
    }

    /// Return structures in vCPU index order.
    pub fn records(&self) -> &[GuestMemoryRange] {
        &self.records
    }

    /// Return the structure for one vCPU index.
    pub fn record(&self, vcpu_index: usize) -> Option<GuestMemoryRange> {
        self.records.get(vcpu_index).copied()
    }

    /// Return the topology size represented by this placement.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Return whether the placement contains no records.
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

impl fmt::Debug for Arm64PvTimeLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arm64PvTimeLayout")
            .field("record_count", &self.records.len())
            .finish()
    }
}

/// Failure while planning arm64 PVTime guest-memory placement.
#[derive(Debug)]
pub enum Arm64PvTimeLayoutError {
    EmptyTopology,
    SizeOverflow,
    AddressOverflow,
    InsufficientSpace,
    MetadataAllocation(TryReserveError),
    InvalidRange(GuestMemoryError),
}

impl fmt::Display for Arm64PvTimeLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyTopology => "arm64 PVTime topology is empty",
            Self::SizeOverflow => "arm64 PVTime topology size overflows",
            Self::AddressOverflow => "arm64 PVTime record address overflows",
            Self::InsufficientSpace => "arm64 PVTime arena is too small",
            Self::MetadataAllocation(_) => "arm64 PVTime record metadata allocation failed",
            Self::InvalidRange(_) => "arm64 PVTime record range is invalid",
        };
        f.write_str(message)
    }
}

impl std::error::Error for Arm64PvTimeLayoutError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MetadataAllocation(source) => Some(source),
            Self::InvalidRange(source) => Some(source),
            Self::EmptyTopology
            | Self::SizeOverflow
            | Self::AddressOverflow
            | Self::InsufficientSpace => None,
        }
    }
}

/// Stage of one PVTime backing-record write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm64PvTimeInitializationWrite {
    Initialize,
    Rollback,
}

/// Redacted failure from multi-record PVTime backing initialization.
pub struct Arm64PvTimeInitializationError<E> {
    initialized_records: usize,
    rollback_failures: usize,
    source: E,
}

impl<E> Arm64PvTimeInitializationError<E> {
    /// Return how many records were initialized before the failing write.
    pub const fn initialized_records(&self) -> usize {
        self.initialized_records
    }

    /// Return how many committed-prefix records could not be zeroed during rollback.
    pub const fn rollback_failures(&self) -> usize {
        self.rollback_failures
    }

    /// Return the initial write failure.
    pub const fn source_error(&self) -> &E {
        &self.source
    }

    /// Consume this failure and return the initial write error.
    pub fn into_source(self) -> E {
        self.source
    }
}

impl<E> fmt::Debug for Arm64PvTimeInitializationError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Arm64PvTimeInitializationError")
            .field("initialized_records", &self.initialized_records)
            .field("rollback_failures", &self.rollback_failures)
            .field("source", &"<redacted>")
            .finish()
    }
}

impl<E: fmt::Display> fmt::Display for Arm64PvTimeInitializationError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "arm64 PVTime initialization failed after {} records with {} rollback failures: {}",
            self.initialized_records, self.rollback_failures, self.source
        )
    }
}

impl<E: std::error::Error + 'static> std::error::Error for Arm64PvTimeInitializationError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Initialize every record and zero the committed prefix if a later write fails.
pub fn initialize_arm64_pvtime_records_with<E>(
    layout: &Arm64PvTimeLayout,
    mut write: impl FnMut(
        Arm64PvTimeInitializationWrite,
        usize,
        GuestMemoryRange,
        &[u8; ARM64_PVTIME_STRUCTURE_SIZE],
    ) -> Result<(), E>,
) -> Result<(), Arm64PvTimeInitializationError<E>> {
    let initial = Arm64PvTimeStAbi::initial().to_bytes();
    for (index, range) in layout.records().iter().copied().enumerate() {
        if let Err(source) = write(
            Arm64PvTimeInitializationWrite::Initialize,
            index,
            range,
            &initial,
        ) {
            let mut rollback_failures = 0;
            for (rollback_index, rollback_range) in layout
                .records()
                .iter()
                .copied()
                .take(index)
                .enumerate()
                .rev()
            {
                if write(
                    Arm64PvTimeInitializationWrite::Rollback,
                    rollback_index,
                    rollback_range,
                    &[0; ARM64_PVTIME_STRUCTURE_SIZE],
                )
                .is_err()
                {
                    rollback_failures += 1;
                }
            }
            return Err(Arm64PvTimeInitializationError {
                initialized_records: index,
                rollback_failures,
                source,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ARM64_PVTIME_ATTRIBUTES, ARM64_PVTIME_REVISION, ARM64_PVTIME_STRUCTURE_ALIGNMENT,
        ARM64_PVTIME_STRUCTURE_SIZE, Arm64PvTimeAbiError, Arm64PvTimeInitializationWrite,
        Arm64PvTimeLayout, Arm64PvTimeLayoutError, Arm64PvTimeStAbi,
        initialize_arm64_pvtime_records_with,
    };
    use crate::memory::{GuestAddress, GuestMemoryRange};

    fn arena(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size).expect("arena should be valid")
    }

    #[test]
    fn abi_layout_is_exact_little_endian_and_zero_padded() {
        let bytes = Arm64PvTimeStAbi::initial().to_bytes();

        assert_eq!(bytes.len(), ARM64_PVTIME_STRUCTURE_SIZE);
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 0);
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 0);
        assert!(bytes[16..].iter().all(|byte| *byte == 0));
        assert_eq!(
            Arm64PvTimeStAbi::from_bytes(bytes).unwrap(),
            Arm64PvTimeStAbi::initial()
        );
        assert_eq!(ARM64_PVTIME_REVISION, 0);
        assert_eq!(ARM64_PVTIME_ATTRIBUTES, 0);
    }

    #[test]
    fn abi_rejects_future_revision_attributes_and_nonzero_padding() {
        let mut revision = Arm64PvTimeStAbi::initial().to_bytes();
        revision[0] = 1;
        assert_eq!(
            Arm64PvTimeStAbi::from_bytes(revision),
            Err(Arm64PvTimeAbiError::UnsupportedRevision)
        );

        let mut attributes = Arm64PvTimeStAbi::initial().to_bytes();
        attributes[4] = 1;
        assert_eq!(
            Arm64PvTimeStAbi::from_bytes(attributes),
            Err(Arm64PvTimeAbiError::UnsupportedAttributes)
        );

        let mut padding = Arm64PvTimeStAbi::initial().to_bytes();
        padding[63] = 1;
        assert_eq!(
            Arm64PvTimeStAbi::from_bytes(padding),
            Err(Arm64PvTimeAbiError::NonzeroPadding)
        );
    }

    #[test]
    fn layout_packs_aligned_records_at_high_end_in_vcpu_order() {
        let arena = arena(0x1003, 0x4fd);
        let layout = Arm64PvTimeLayout::plan(3, arena).expect("layout should fit");

        assert_eq!(layout.len(), 3);
        assert_eq!(layout.record(0).unwrap().start(), GuestAddress::new(0x1440));
        assert_eq!(layout.record(1).unwrap().start(), GuestAddress::new(0x1480));
        assert_eq!(layout.record(2).unwrap().start(), GuestAddress::new(0x14c0));
        for record in layout.records() {
            assert_eq!(record.size(), ARM64_PVTIME_STRUCTURE_SIZE as u64);
            assert!(
                record
                    .start()
                    .is_aligned(ARM64_PVTIME_STRUCTURE_ALIGNMENT)
                    .unwrap()
            );
            assert!(record.start().raw_value() >= arena.start().raw_value());
            assert!(record.end_exclusive().raw_value() <= arena.end_exclusive().raw_value());
        }
        assert!(
            layout
                .records()
                .windows(2)
                .all(|pair| !pair[0].overlaps(pair[1]))
        );
        assert!(format!("{layout:?}").contains("record_count: 3"));
        assert!(!format!("{layout:?}").contains("0x1440"));
    }

    #[test]
    fn layout_rejects_empty_topology_and_exhausted_arena() {
        assert!(matches!(
            Arm64PvTimeLayout::plan(0, arena(0x1000, 64)),
            Err(Arm64PvTimeLayoutError::EmptyTopology)
        ));
        assert!(matches!(
            Arm64PvTimeLayout::plan(2, arena(0x1001, 127)),
            Err(Arm64PvTimeLayoutError::InsufficientSpace)
        ));
    }

    #[test]
    fn initialization_rolls_back_exact_committed_prefix_in_reverse_order() {
        let layout = Arm64PvTimeLayout::plan(4, arena(0x1000, 0x100)).expect("layout should fit");
        let mut calls = Vec::new();
        let error = initialize_arm64_pvtime_records_with(
            &layout,
            |operation, index, _, bytes| -> Result<(), &'static str> {
                calls.push((operation, index, bytes.iter().all(|byte| *byte == 0)));
                if operation == Arm64PvTimeInitializationWrite::Initialize && index == 2 {
                    Err("injected write failure")
                } else if operation == Arm64PvTimeInitializationWrite::Rollback && index == 0 {
                    Err("injected rollback failure")
                } else {
                    Ok(())
                }
            },
        )
        .expect_err("third write should fail");

        assert_eq!(error.initialized_records(), 2);
        assert_eq!(error.rollback_failures(), 1);
        assert_eq!(error.source_error(), &"injected write failure");
        assert_eq!(
            calls
                .iter()
                .map(|(stage, index, _)| (*stage, *index))
                .collect::<Vec<_>>(),
            vec![
                (Arm64PvTimeInitializationWrite::Initialize, 0),
                (Arm64PvTimeInitializationWrite::Initialize, 1),
                (Arm64PvTimeInitializationWrite::Initialize, 2),
                (Arm64PvTimeInitializationWrite::Rollback, 1),
                (Arm64PvTimeInitializationWrite::Rollback, 0),
            ]
        );
        assert!(calls.iter().all(|(_, _, zero)| *zero));
        assert!(!format!("{error:?}").contains("injected write failure"));
    }
}
