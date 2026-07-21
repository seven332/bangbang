//! Firecracker-compatible VMClock ABI state and restore updates.

use std::fmt;
use std::sync::atomic::{Ordering, fence};

use crate::memory::{GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryRange};

pub const VMCLOCK_ABI_SIZE: usize = 112;
pub const VMCLOCK_PAGE_SIZE: u64 = 0x1000;
pub const VMCLOCK_MAGIC: u32 = 1_263_289_174;
pub const VMCLOCK_VERSION: u16 = 1;
pub const VMCLOCK_COUNTER_ARM_VCNT: u8 = 0;
pub const VMCLOCK_COUNTER_INVALID: u8 = 255;
pub const VMCLOCK_STATUS_UNKNOWN: u8 = 0;
pub const VMCLOCK_FLAG_VM_GEN_COUNTER_PRESENT: u64 = 1 << 8;
pub const VMCLOCK_FLAG_NOTIFICATION_PRESENT: u64 = 1 << 9;
pub const VMCLOCK_REQUIRED_FLAGS: u64 =
    VMCLOCK_FLAG_VM_GEN_COUNTER_PRESENT | VMCLOCK_FLAG_NOTIFICATION_PRESENT;

const VMCLOCK_KNOWN_FLAGS: u64 = (1 << 10) - 1;
const MAGIC_OFFSET: usize = 0;
const SIZE_OFFSET: usize = 4;
const VERSION_OFFSET: usize = 8;
const COUNTER_ID_OFFSET: usize = 10;
const TIME_TYPE_OFFSET: usize = 11;
const SEQUENCE_OFFSET: usize = 12;
const DISRUPTION_MARKER_OFFSET: usize = 16;
const FLAGS_OFFSET: usize = 24;
const PADDING_OFFSET: usize = 32;
const CLOCK_STATUS_OFFSET: usize = 34;
const SMEARING_HINT_OFFSET: usize = 35;
const TAI_OFFSET_OFFSET: usize = 36;
const LEAP_INDICATOR_OFFSET: usize = 38;
const COUNTER_PERIOD_SHIFT_OFFSET: usize = 39;
const COUNTER_VALUE_OFFSET: usize = 40;
const COUNTER_PERIOD_OFFSET: usize = 48;
const COUNTER_PERIOD_ESTERROR_OFFSET: usize = 56;
const COUNTER_PERIOD_MAXERROR_OFFSET: usize = 64;
const TIME_SECONDS_OFFSET: usize = 72;
const TIME_FRACTION_OFFSET: usize = 80;
const TIME_ESTERROR_OFFSET: usize = 88;
const TIME_MAXERROR_OFFSET: usize = 96;
const GENERATION_COUNTER_OFFSET: usize = 104;

/// Complete host-owned VMClock ABI payload in native numeric form.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VmClockAbi {
    magic: u32,
    size: u32,
    version: u16,
    counter_id: u8,
    time_type: u8,
    sequence: u32,
    disruption_marker: u64,
    flags: u64,
    padding: [u8; 2],
    clock_status: u8,
    smearing_hint: u8,
    tai_offset_seconds: u16,
    leap_indicator: u8,
    counter_period_shift: u8,
    counter_value: u64,
    counter_period_fraction_seconds: u64,
    counter_period_esterror_rate_fraction_seconds: u64,
    counter_period_maxerror_rate_fraction_seconds: u64,
    time_seconds: u64,
    time_fraction_seconds: u64,
    time_esterror_nanoseconds: u64,
    time_maxerror_nanoseconds: u64,
    generation_counter: u64,
}

impl VmClockAbi {
    /// Firecracker v1.16.0 startup value for the reserved VMClock page.
    pub const fn initial() -> Self {
        Self {
            magic: VMCLOCK_MAGIC,
            size: VMCLOCK_PAGE_SIZE as u32,
            version: VMCLOCK_VERSION,
            counter_id: VMCLOCK_COUNTER_INVALID,
            time_type: 0,
            sequence: 0,
            disruption_marker: 0,
            flags: VMCLOCK_REQUIRED_FLAGS,
            padding: [0; 2],
            clock_status: VMCLOCK_STATUS_UNKNOWN,
            smearing_hint: 0,
            tai_offset_seconds: 0,
            leap_indicator: 0,
            counter_period_shift: 0,
            counter_value: 0,
            counter_period_fraction_seconds: 0,
            counter_period_esterror_rate_fraction_seconds: 0,
            counter_period_maxerror_rate_fraction_seconds: 0,
            time_seconds: 0,
            time_fraction_seconds: 0,
            time_esterror_nanoseconds: 0,
            time_maxerror_nanoseconds: 0,
            generation_counter: 0,
        }
    }

    pub fn from_bytes(bytes: [u8; VMCLOCK_ABI_SIZE]) -> Result<Self, VmClockAbiError> {
        let value = Self {
            magic: read_u32(&bytes, MAGIC_OFFSET)?,
            size: read_u32(&bytes, SIZE_OFFSET)?,
            version: read_u16(&bytes, VERSION_OFFSET)?,
            counter_id: bytes[COUNTER_ID_OFFSET],
            time_type: bytes[TIME_TYPE_OFFSET],
            sequence: read_u32(&bytes, SEQUENCE_OFFSET)?,
            disruption_marker: read_u64(&bytes, DISRUPTION_MARKER_OFFSET)?,
            flags: read_u64(&bytes, FLAGS_OFFSET)?,
            padding: [bytes[PADDING_OFFSET], bytes[PADDING_OFFSET + 1]],
            clock_status: bytes[CLOCK_STATUS_OFFSET],
            smearing_hint: bytes[SMEARING_HINT_OFFSET],
            tai_offset_seconds: read_u16(&bytes, TAI_OFFSET_OFFSET)?,
            leap_indicator: bytes[LEAP_INDICATOR_OFFSET],
            counter_period_shift: bytes[COUNTER_PERIOD_SHIFT_OFFSET],
            counter_value: read_u64(&bytes, COUNTER_VALUE_OFFSET)?,
            counter_period_fraction_seconds: read_u64(&bytes, COUNTER_PERIOD_OFFSET)?,
            counter_period_esterror_rate_fraction_seconds: read_u64(
                &bytes,
                COUNTER_PERIOD_ESTERROR_OFFSET,
            )?,
            counter_period_maxerror_rate_fraction_seconds: read_u64(
                &bytes,
                COUNTER_PERIOD_MAXERROR_OFFSET,
            )?,
            time_seconds: read_u64(&bytes, TIME_SECONDS_OFFSET)?,
            time_fraction_seconds: read_u64(&bytes, TIME_FRACTION_OFFSET)?,
            time_esterror_nanoseconds: read_u64(&bytes, TIME_ESTERROR_OFFSET)?,
            time_maxerror_nanoseconds: read_u64(&bytes, TIME_MAXERROR_OFFSET)?,
            generation_counter: read_u64(&bytes, GENERATION_COUNTER_OFFSET)?,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn to_bytes(self) -> [u8; VMCLOCK_ABI_SIZE] {
        let mut bytes = [0; VMCLOCK_ABI_SIZE];
        write_u32(&mut bytes, MAGIC_OFFSET, self.magic);
        write_u32(&mut bytes, SIZE_OFFSET, self.size);
        write_u16(&mut bytes, VERSION_OFFSET, self.version);
        bytes[COUNTER_ID_OFFSET] = self.counter_id;
        bytes[TIME_TYPE_OFFSET] = self.time_type;
        write_u32(&mut bytes, SEQUENCE_OFFSET, self.sequence);
        write_u64(&mut bytes, DISRUPTION_MARKER_OFFSET, self.disruption_marker);
        write_u64(&mut bytes, FLAGS_OFFSET, self.flags);
        bytes[PADDING_OFFSET..PADDING_OFFSET + 2].copy_from_slice(&self.padding);
        bytes[CLOCK_STATUS_OFFSET] = self.clock_status;
        bytes[SMEARING_HINT_OFFSET] = self.smearing_hint;
        write_u16(&mut bytes, TAI_OFFSET_OFFSET, self.tai_offset_seconds);
        bytes[LEAP_INDICATOR_OFFSET] = self.leap_indicator;
        bytes[COUNTER_PERIOD_SHIFT_OFFSET] = self.counter_period_shift;
        write_u64(&mut bytes, COUNTER_VALUE_OFFSET, self.counter_value);
        write_u64(
            &mut bytes,
            COUNTER_PERIOD_OFFSET,
            self.counter_period_fraction_seconds,
        );
        write_u64(
            &mut bytes,
            COUNTER_PERIOD_ESTERROR_OFFSET,
            self.counter_period_esterror_rate_fraction_seconds,
        );
        write_u64(
            &mut bytes,
            COUNTER_PERIOD_MAXERROR_OFFSET,
            self.counter_period_maxerror_rate_fraction_seconds,
        );
        write_u64(&mut bytes, TIME_SECONDS_OFFSET, self.time_seconds);
        write_u64(&mut bytes, TIME_FRACTION_OFFSET, self.time_fraction_seconds);
        write_u64(
            &mut bytes,
            TIME_ESTERROR_OFFSET,
            self.time_esterror_nanoseconds,
        );
        write_u64(
            &mut bytes,
            TIME_MAXERROR_OFFSET,
            self.time_maxerror_nanoseconds,
        );
        write_u64(
            &mut bytes,
            GENERATION_COUNTER_OFFSET,
            self.generation_counter,
        );
        bytes
    }

    pub const fn size(self) -> u32 {
        self.size
    }

    pub const fn version(self) -> u16 {
        self.version
    }

    pub const fn clock_status(self) -> u8 {
        self.clock_status
    }

    pub const fn sequence(self) -> u32 {
        self.sequence
    }

    pub const fn disruption_marker(self) -> u64 {
        self.disruption_marker
    }

    pub const fn generation_counter(self) -> u64 {
        self.generation_counter
    }

    fn validate(self) -> Result<(), VmClockAbiError> {
        if self.magic != VMCLOCK_MAGIC {
            return Err(VmClockAbiError::InvalidMagic);
        }
        if self.size != VMCLOCK_PAGE_SIZE as u32 {
            return Err(VmClockAbiError::InvalidSize);
        }
        if self.version != VMCLOCK_VERSION {
            return Err(VmClockAbiError::UnsupportedVersion);
        }
        if !matches!(
            self.counter_id,
            VMCLOCK_COUNTER_ARM_VCNT | VMCLOCK_COUNTER_INVALID
        ) {
            return Err(VmClockAbiError::InvalidCounterId);
        }
        if self.time_type > 4 {
            return Err(VmClockAbiError::InvalidTimeType);
        }
        if self.flags & !VMCLOCK_KNOWN_FLAGS != 0
            || self.flags & VMCLOCK_REQUIRED_FLAGS != VMCLOCK_REQUIRED_FLAGS
        {
            return Err(VmClockAbiError::InvalidFlags);
        }
        if self.padding != [0; 2] {
            return Err(VmClockAbiError::NonzeroPadding);
        }
        if self.clock_status > 4 {
            return Err(VmClockAbiError::InvalidClockStatus);
        }
        if self.smearing_hint > 2 {
            return Err(VmClockAbiError::InvalidSmearingHint);
        }
        if self.leap_indicator > 5 {
            return Err(VmClockAbiError::InvalidLeapIndicator);
        }
        if !self.sequence.is_multiple_of(2) {
            return Err(VmClockAbiError::OddSequence);
        }
        Ok(())
    }

    /// Update the guest page after restore, leaving an odd sequence on any
    /// failure after the first committed write.
    pub fn update_after_restore(
        &mut self,
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), VmClockRestoreUpdateError> {
        if range.size() != VMCLOCK_PAGE_SIZE {
            return Err(VmClockRestoreUpdateError::InvalidRange);
        }

        self.update_after_restore_with(|offset, bytes, stage, committed| {
            write_guest_field(memory, range.start(), offset, bytes, stage, committed)
        })
    }

    fn update_after_restore_with(
        &mut self,
        mut write: impl FnMut(
            usize,
            &[u8],
            VmClockRestoreUpdateStage,
            bool,
        ) -> Result<(), VmClockRestoreUpdateError>,
    ) -> Result<(), VmClockRestoreUpdateError> {
        let odd_sequence = self.sequence | 1;
        write(
            SEQUENCE_OFFSET,
            &odd_sequence.to_le_bytes(),
            VmClockRestoreUpdateStage::BeginSequence,
            false,
        )?;
        self.sequence = odd_sequence;

        // Matched by the guest read barrier after it observes the odd value.
        fence(Ordering::Release);

        let disruption_marker = self.disruption_marker.wrapping_add(1);
        write(
            DISRUPTION_MARKER_OFFSET,
            &disruption_marker.to_le_bytes(),
            VmClockRestoreUpdateStage::DisruptionMarker,
            true,
        )?;
        self.disruption_marker = disruption_marker;

        let generation_counter = self.generation_counter.wrapping_add(1);
        write(
            GENERATION_COUNTER_OFFSET,
            &generation_counter.to_le_bytes(),
            VmClockRestoreUpdateStage::GenerationCounter,
            true,
        )?;
        self.generation_counter = generation_counter;

        // Publish both counters before making the sequence stable again.
        fence(Ordering::Release);

        let even_sequence = odd_sequence.wrapping_add(1);
        write(
            SEQUENCE_OFFSET,
            &even_sequence.to_le_bytes(),
            VmClockRestoreUpdateStage::CompleteSequence,
            true,
        )?;
        self.sequence = even_sequence;
        Ok(())
    }
}

impl Default for VmClockAbi {
    fn default() -> Self {
        Self::initial()
    }
}

impl fmt::Debug for VmClockAbi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmClockAbi")
            .field("size", &self.size)
            .field("version", &self.version)
            .field("clock_status", &self.clock_status)
            .field("sequence", &self.sequence)
            .field("disruption_marker", &self.disruption_marker)
            .field("generation_counter", &self.generation_counter)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmClockAbiError {
    InvalidMagic,
    InvalidSize,
    UnsupportedVersion,
    InvalidCounterId,
    InvalidTimeType,
    InvalidFlags,
    NonzeroPadding,
    InvalidClockStatus,
    InvalidSmearingHint,
    InvalidLeapIndicator,
    OddSequence,
}

impl fmt::Display for VmClockAbiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidMagic => "VMClock ABI magic is invalid",
            Self::InvalidSize => "VMClock ABI page size is invalid",
            Self::UnsupportedVersion => "VMClock ABI version is unsupported",
            Self::InvalidCounterId => "VMClock ABI counter ID is invalid for arm64",
            Self::InvalidTimeType => "VMClock ABI time type is invalid",
            Self::InvalidFlags => "VMClock ABI flags are invalid",
            Self::NonzeroPadding => "VMClock ABI padding is nonzero",
            Self::InvalidClockStatus => "VMClock ABI clock status is invalid",
            Self::InvalidSmearingHint => "VMClock ABI smearing hint is invalid",
            Self::InvalidLeapIndicator => "VMClock ABI leap indicator is invalid",
            Self::OddSequence => "VMClock ABI sequence is unstable",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for VmClockAbiError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmClockRestoreUpdateStage {
    BeginSequence,
    DisruptionMarker,
    GenerationCounter,
    CompleteSequence,
}

impl fmt::Display for VmClockRestoreUpdateStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::BeginSequence => "odd sequence publication",
            Self::DisruptionMarker => "disruption marker publication",
            Self::GenerationCounter => "generation counter publication",
            Self::CompleteSequence => "even sequence publication",
        };
        formatter.write_str(name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmClockRestoreUpdateError {
    InvalidRange,
    GuestMemoryWrite {
        stage: VmClockRestoreUpdateStage,
        committed: bool,
        source: GuestMemoryAccessError,
    },
}

impl VmClockRestoreUpdateError {
    pub const fn is_committed(&self) -> bool {
        matches!(
            self,
            Self::GuestMemoryWrite {
                committed: true,
                ..
            }
        )
    }
}

impl fmt::Display for VmClockRestoreUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange => formatter.write_str("VMClock restore range is invalid"),
            Self::GuestMemoryWrite {
                stage, committed, ..
            } => write!(
                formatter,
                "VMClock restore failed during {stage}; committed={committed}"
            ),
        }
    }
}

impl std::error::Error for VmClockRestoreUpdateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::GuestMemoryWrite { source, .. } => Some(source),
            Self::InvalidRange => None,
        }
    }
}

fn write_guest_field(
    memory: &mut GuestMemory,
    start: GuestAddress,
    offset: usize,
    bytes: &[u8],
    stage: VmClockRestoreUpdateStage,
    committed: bool,
) -> Result<(), VmClockRestoreUpdateError> {
    let address = start
        .checked_add(offset as u64)
        .ok_or(VmClockRestoreUpdateError::InvalidRange)?;
    memory.write_slice(bytes, address).map_err(|source| {
        VmClockRestoreUpdateError::GuestMemoryWrite {
            stage,
            committed,
            source,
        }
    })
}

fn read_u16(bytes: &[u8; VMCLOCK_ABI_SIZE], offset: usize) -> Result<u16, VmClockAbiError> {
    fixed_field(bytes, offset).map(u16::from_le_bytes)
}

fn read_u32(bytes: &[u8; VMCLOCK_ABI_SIZE], offset: usize) -> Result<u32, VmClockAbiError> {
    fixed_field(bytes, offset).map(u32::from_le_bytes)
}

fn read_u64(bytes: &[u8; VMCLOCK_ABI_SIZE], offset: usize) -> Result<u64, VmClockAbiError> {
    fixed_field(bytes, offset).map(u64::from_le_bytes)
}

fn fixed_field<const SIZE: usize>(
    bytes: &[u8; VMCLOCK_ABI_SIZE],
    offset: usize,
) -> Result<[u8; SIZE], VmClockAbiError> {
    bytes
        .get(offset..offset.saturating_add(SIZE))
        .ok_or(VmClockAbiError::InvalidSize)?
        .try_into()
        .map_err(|_| VmClockAbiError::InvalidSize)
}

fn write_u16(bytes: &mut [u8; VMCLOCK_ABI_SIZE], offset: usize, value: u16) {
    write_field(bytes, offset, &value.to_le_bytes());
}

fn write_u32(bytes: &mut [u8; VMCLOCK_ABI_SIZE], offset: usize, value: u32) {
    write_field(bytes, offset, &value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8; VMCLOCK_ABI_SIZE], offset: usize, value: u64) {
    write_field(bytes, offset, &value.to_le_bytes());
}

fn write_field(bytes: &mut [u8; VMCLOCK_ABI_SIZE], offset: usize, value: &[u8]) {
    let destination = bytes.get_mut(offset..offset.saturating_add(value.len()));
    debug_assert!(destination.is_some(), "VMClock ABI field must fit");
    if let Some(destination) = destination {
        destination.copy_from_slice(value);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DISRUPTION_MARKER_OFFSET, GENERATION_COUNTER_OFFSET, SEQUENCE_OFFSET, VMCLOCK_ABI_SIZE,
        VMCLOCK_PAGE_SIZE, VmClockAbi, VmClockAbiError, VmClockRestoreUpdateError,
        VmClockRestoreUpdateStage,
    };
    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};

    fn memory() -> GuestMemory {
        let range = GuestMemoryRange::new(GuestAddress::new(0x4000), 0x4000)
            .expect("test range should be valid");
        GuestMemory::allocate(
            &GuestMemoryLayout::new(vec![range]).expect("test layout should be valid"),
        )
        .expect("test memory should allocate")
    }

    #[test]
    fn initial_payload_matches_pinned_layout() {
        let abi = VmClockAbi::initial();
        let bytes = abi.to_bytes();

        assert_eq!(bytes.len(), VMCLOCK_ABI_SIZE);
        assert_eq!(VmClockAbi::from_bytes(bytes), Ok(abi));
        assert_eq!(abi.size(), VMCLOCK_PAGE_SIZE as u32);
        assert_eq!(abi.version(), 1);
        assert_eq!(abi.clock_status(), 0);
        assert_eq!(abi.sequence(), 0);
        assert_eq!(abi.disruption_marker(), 0);
        assert_eq!(abi.generation_counter(), 0);
    }

    #[test]
    fn payload_validation_rejects_unstable_or_unsupported_values() {
        let initial = VmClockAbi::initial().to_bytes();
        for (offset, value, expected) in [
            (0, 0, VmClockAbiError::InvalidMagic),
            (5, 0, VmClockAbiError::InvalidSize),
            (8, 2, VmClockAbiError::UnsupportedVersion),
            (10, 1, VmClockAbiError::InvalidCounterId),
            (11, 5, VmClockAbiError::InvalidTimeType),
            (25, 1, VmClockAbiError::InvalidFlags),
            (25, 7, VmClockAbiError::InvalidFlags),
            (32, 1, VmClockAbiError::NonzeroPadding),
            (34, 5, VmClockAbiError::InvalidClockStatus),
            (35, 3, VmClockAbiError::InvalidSmearingHint),
            (38, 6, VmClockAbiError::InvalidLeapIndicator),
            (12, 1, VmClockAbiError::OddSequence),
        ] {
            let mut bytes = initial;
            bytes[offset] = value;
            assert_eq!(VmClockAbi::from_bytes(bytes), Err(expected));
        }
    }

    #[test]
    fn restore_update_publishes_even_wrapping_counters() {
        let mut memory = memory();
        let range = GuestMemoryRange::new(GuestAddress::new(0x4000), VMCLOCK_PAGE_SIZE)
            .expect("VMClock range should be valid");
        let mut bytes = VmClockAbi::initial().to_bytes();
        bytes[SEQUENCE_OFFSET..SEQUENCE_OFFSET + 4].copy_from_slice(&(u32::MAX - 1).to_le_bytes());
        bytes[DISRUPTION_MARKER_OFFSET..DISRUPTION_MARKER_OFFSET + 8]
            .copy_from_slice(&u64::MAX.to_le_bytes());
        bytes[GENERATION_COUNTER_OFFSET..GENERATION_COUNTER_OFFSET + 8]
            .copy_from_slice(&u64::MAX.to_le_bytes());
        let mut abi = VmClockAbi::from_bytes(bytes).expect("test ABI should validate");
        memory
            .write_slice(&abi.to_bytes(), range.start())
            .expect("test ABI should write");

        abi.update_after_restore(&mut memory, range)
            .expect("restore update should succeed");

        let mut observed = [0; VMCLOCK_ABI_SIZE];
        memory
            .read_slice(&mut observed, range.start())
            .expect("updated ABI should read");
        assert_eq!(VmClockAbi::from_bytes(observed), Ok(abi));
        assert_eq!(abi.sequence(), 0);
        assert_eq!(abi.disruption_marker(), 0);
        assert_eq!(abi.generation_counter(), 0);
    }

    #[test]
    fn restore_update_classifies_precommit_and_partial_writes() {
        let unavailable_range = GuestMemoryRange::new(GuestAddress::new(0x8000), VMCLOCK_PAGE_SIZE)
            .expect("unavailable VMClock range should be valid");
        let mut unavailable = memory();
        let mut precommit = VmClockAbi::initial();
        let error = precommit
            .update_after_restore(&mut unavailable, unavailable_range)
            .expect_err("first write outside memory should fail");
        assert!(matches!(
            error,
            VmClockRestoreUpdateError::GuestMemoryWrite {
                stage: VmClockRestoreUpdateStage::BeginSequence,
                committed: false,
                ..
            }
        ));
        assert!(!error.is_committed());

        let sequence_only_range =
            GuestMemoryRange::new(GuestAddress::new(0x7ff0), VMCLOCK_PAGE_SIZE)
                .expect("partial VMClock range should be valid");
        let mut sequence_only = memory();
        let mut partial = VmClockAbi::initial();
        let error = partial
            .update_after_restore(&mut sequence_only, sequence_only_range)
            .expect_err("counter write outside memory should fail after odd sequence");
        assert!(matches!(
            error,
            VmClockRestoreUpdateError::GuestMemoryWrite {
                stage: VmClockRestoreUpdateStage::DisruptionMarker,
                committed: true,
                ..
            }
        ));
        assert!(error.is_committed());
        assert_eq!(partial.sequence(), 1);
    }

    #[test]
    fn restore_update_reports_every_write_stage_and_committed_prefix() {
        let range = GuestMemoryRange::new(GuestAddress::new(0x4000), VMCLOCK_PAGE_SIZE)
            .expect("VMClock test range should be valid");

        for (failed_stage, expected_sequence, expected_disruption, expected_generation) in [
            (VmClockRestoreUpdateStage::BeginSequence, 0, 0, 0),
            (VmClockRestoreUpdateStage::DisruptionMarker, 1, 0, 0),
            (VmClockRestoreUpdateStage::GenerationCounter, 1, 1, 0),
            (VmClockRestoreUpdateStage::CompleteSequence, 1, 1, 1),
        ] {
            let mut abi = VmClockAbi::initial();
            let error = abi
                .update_after_restore_with(|_offset, _bytes, stage, committed| {
                    if stage == failed_stage {
                        Err(VmClockRestoreUpdateError::GuestMemoryWrite {
                            stage,
                            committed,
                            source: crate::memory::GuestMemoryAccessError::UnmappedRange { range },
                        })
                    } else {
                        Ok(())
                    }
                })
                .expect_err("injected VMClock stage should fail");

            assert!(matches!(
                error,
                VmClockRestoreUpdateError::GuestMemoryWrite { stage, committed, .. }
                    if stage == failed_stage
                        && committed == (failed_stage != VmClockRestoreUpdateStage::BeginSequence)
            ));
            assert_eq!(
                abi.sequence(),
                expected_sequence,
                "sequence after {failed_stage:?}"
            );
            assert_eq!(
                abi.disruption_marker(),
                expected_disruption,
                "disruption marker after {failed_stage:?}"
            );
            assert_eq!(
                abi.generation_counter(),
                expected_generation,
                "generation counter after {failed_stage:?}"
            );
        }
    }
}
