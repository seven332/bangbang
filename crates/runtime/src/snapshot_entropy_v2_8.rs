//! Canonical detached native-v2 2.8 entropy state profile.
//!
//! This module contains only inert entropy configuration, token-bucket
//! continuation, common virtio registers, and transport placement. Entropy
//! bytes, source handles, host clock identity, interrupt authority, metrics,
//! and scheduler ownership remain destination-local.

use std::fmt;

use crate::entropy::{
    EntropyConfig, EntropyRateLimiterConfig, EntropyTokenBucketConfig, VIRTIO_RNG_QUEUE_SIZE,
};
use crate::snapshot_device_v2::{SnapshotV2DeviceTransport, SnapshotV2VirtioState};
use crate::snapshot_device_v2_5::{
    queue_ranges, validate_mmio, validate_pci, validate_virtio_with_queue_size,
};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio_mmio::VIRTIO_MMIO_VERSION_1_FEATURE;

mod codec;

#[cfg(test)]
mod tests;

/// Exact compatibility context of the singleton entropy component.
pub const NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION: SnapshotFormatVersion =
    SnapshotFormatVersion::new(2, 8, 0);

/// Maximum complete exact-2.8 entropy component size.
pub const NATIVE_V2_ENTROPY_STATE_MAX_BYTES: usize = 64 * 1024;

/// Fixed exact-2.8 entropy component header size.
pub const NATIVE_V2_ENTROPY_STATE_HEADER_BYTES: usize = 64;

/// Fixed encoded size of one entropy section-directory entry.
pub const NATIVE_V2_ENTROPY_STATE_SECTION_ENTRY_BYTES: usize = 32;

/// Fixed encoded size of the entropy-local section.
pub const NATIVE_V2_ENTROPY_STATE_LOCAL_BYTES: usize = 128;

const REDACTED: &str = "<redacted>";

/// One checked active virtio-rng queue cursor pair.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2EntropyQueueState {
    next_available: u16,
    next_used: u16,
}

impl SnapshotV2EntropyQueueState {
    /// Constructs cursors whose outstanding distance fits the selected queue.
    pub fn try_new(
        next_available: u16,
        next_used: u16,
        queue_size: u16,
    ) -> Result<Self, SnapshotV2EntropyStateBuildError> {
        let state = Self {
            next_available,
            next_used,
        };
        if queue_size == 0 || state.outstanding() > queue_size {
            return Err(SnapshotV2EntropyStateBuildError::Queue);
        }
        Ok(state)
    }

    pub(crate) const fn from_parts(next_available: u16, next_used: u16) -> Self {
        Self {
            next_available,
            next_used,
        }
    }

    /// Returns the next device-local available-ring cursor.
    pub const fn next_available(self) -> u16 {
        self.next_available
    }

    /// Returns the next device-local used-ring cursor.
    pub const fn next_used(self) -> u16 {
        self.next_used
    }

    /// Returns the wrapping outstanding descriptor count.
    pub const fn outstanding(self) -> u16 {
        self.next_available.wrapping_sub(self.next_used)
    }
}

impl fmt::Debug for SnapshotV2EntropyQueueState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2EntropyQueueState")
            .field("cursors", &REDACTED)
            .finish()
    }
}

/// One enabled entropy token bucket's host-time-free continuation.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2EntropyBucketState {
    budget: u64,
    remaining_burst: u64,
    age_nanos: u64,
}

impl SnapshotV2EntropyBucketState {
    /// Constructs state checked against one enabled external configuration.
    pub fn try_new(
        config: EntropyTokenBucketConfig,
        budget: u64,
        remaining_burst: u64,
        age_nanos: u64,
    ) -> Result<Self, SnapshotV2EntropyStateBuildError> {
        let state = Self {
            budget,
            remaining_burst,
            age_nanos,
        };
        validate_bucket_relationship(Some(config), Some(state))?;
        Ok(state)
    }

    pub(crate) const fn from_parts(budget: u64, remaining_burst: u64, age_nanos: u64) -> Self {
        Self {
            budget,
            remaining_burst,
            age_nanos,
        }
    }

    /// Returns the retained recurring-token budget.
    pub const fn budget(self) -> u64 {
        self.budget
    }

    /// Returns the retained one-time burst budget.
    pub const fn remaining_burst(self) -> u64 {
        self.remaining_burst
    }

    /// Returns logical nanoseconds elapsed at capture.
    pub const fn age_nanos(self) -> u64 {
        self.age_nanos
    }
}

impl fmt::Debug for SnapshotV2EntropyBucketState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2EntropyBucketState")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Detached bandwidth and operations token-bucket state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV2EntropyLimiterState {
    bandwidth: Option<SnapshotV2EntropyBucketState>,
    ops: Option<SnapshotV2EntropyBucketState>,
}

impl SnapshotV2EntropyLimiterState {
    /// Constructs limiter state checked against the exact external config.
    pub fn try_new(
        config: Option<EntropyRateLimiterConfig>,
        bandwidth: Option<SnapshotV2EntropyBucketState>,
        ops: Option<SnapshotV2EntropyBucketState>,
    ) -> Result<Self, SnapshotV2EntropyStateBuildError> {
        if config.is_some_and(|config| !config.is_configured()) {
            return Err(SnapshotV2EntropyStateBuildError::Configuration);
        }
        let state = Self { bandwidth, ops };
        validate_limiter_relationship(config, state)?;
        Ok(state)
    }

    pub(crate) const fn from_parts(
        bandwidth: Option<SnapshotV2EntropyBucketState>,
        ops: Option<SnapshotV2EntropyBucketState>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    /// Returns enabled bandwidth-bucket state.
    pub const fn bandwidth(self) -> Option<SnapshotV2EntropyBucketState> {
        self.bandwidth
    }

    /// Returns enabled operations-bucket state.
    pub const fn ops(self) -> Option<SnapshotV2EntropyBucketState> {
        self.ops
    }

    /// Returns whether at least one enabled bucket is retained.
    pub const fn is_enabled(self) -> bool {
        self.bandwidth.is_some() || self.ops.is_some()
    }
}

impl fmt::Debug for SnapshotV2EntropyLimiterState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2EntropyLimiterState")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Host-time-free entropy retry disposition.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2EntropyRetryState {
    /// No retained rate-limited work.
    None,
    /// Retry retained work as soon as the destination scheduler is active.
    Immediate,
    /// Retry after the retained relative duration.
    After {
        /// Remaining logical retry duration.
        remaining_nanos: u64,
    },
}

impl SnapshotV2EntropyRetryState {
    /// Constructs a nonzero delayed retry.
    pub fn try_after(remaining_nanos: u64) -> Result<Self, SnapshotV2EntropyStateBuildError> {
        if remaining_nanos == 0 {
            Err(SnapshotV2EntropyStateBuildError::Retry)
        } else {
            Ok(Self::After { remaining_nanos })
        }
    }

    /// Returns whether retained pending work requires a retry.
    pub const fn has_retry(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Returns the delayed duration, when present.
    pub const fn remaining_nanos(self) -> Option<u64> {
        match self {
            Self::None | Self::Immediate => None,
            Self::After { remaining_nanos } => Some(remaining_nanos),
        }
    }
}

impl fmt::Debug for SnapshotV2EntropyRetryState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let disposition = match self {
            Self::None => "none",
            Self::Immediate => "immediate",
            Self::After { .. } => "delayed",
        };
        formatter
            .debug_tuple("SnapshotV2EntropyRetryState")
            .field(&disposition)
            .finish()
    }
}

/// Complete bounded exact-2.8 entropy component value.
#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV2EntropyState {
    config: EntropyConfig,
    active_queue: Option<SnapshotV2EntropyQueueState>,
    limiter: SnapshotV2EntropyLimiterState,
    retry: SnapshotV2EntropyRetryState,
    pending: bool,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
}

impl SnapshotV2EntropyState {
    /// Constructs one complete checked entropy continuation.
    pub fn try_new(
        config: EntropyConfig,
        active_queue: Option<SnapshotV2EntropyQueueState>,
        limiter: SnapshotV2EntropyLimiterState,
        retry: SnapshotV2EntropyRetryState,
        pending: bool,
        virtio: SnapshotV2VirtioState,
        transport: SnapshotV2DeviceTransport,
    ) -> Result<Self, SnapshotV2EntropyStateBuildError> {
        let state = Self {
            config,
            active_queue,
            limiter,
            retry,
            pending,
            virtio,
            transport,
        };
        validate_entropy_state(&state)?;
        Ok(state)
    }

    /// Returns the exact compatibility context of this value.
    pub const fn compatibility_version(&self) -> SnapshotFormatVersion {
        NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION
    }

    /// Returns the exact external entropy configuration.
    pub const fn config(&self) -> EntropyConfig {
        self.config
    }

    /// Returns active queue cursors when the device is activated.
    pub const fn active_queue(&self) -> Option<SnapshotV2EntropyQueueState> {
        self.active_queue
    }

    /// Returns enabled token-bucket continuation state.
    pub const fn limiter(&self) -> SnapshotV2EntropyLimiterState {
        self.limiter
    }

    /// Returns the host-time-free retry disposition.
    pub const fn retry(&self) -> SnapshotV2EntropyRetryState {
        self.retry
    }

    /// Returns whether one rate-limited descriptor is retained.
    pub const fn has_pending_work(&self) -> bool {
        self.pending
    }

    /// Returns common virtio continuation state.
    pub const fn virtio(&self) -> &SnapshotV2VirtioState {
        &self.virtio
    }

    /// Returns exact MMIO or PCI transport state.
    pub const fn transport(&self) -> &SnapshotV2DeviceTransport {
        &self.transport
    }

    /// Consumes this value into its inert parts.
    pub fn into_parts(
        self,
    ) -> (
        EntropyConfig,
        Option<SnapshotV2EntropyQueueState>,
        SnapshotV2EntropyLimiterState,
        SnapshotV2EntropyRetryState,
        bool,
        SnapshotV2VirtioState,
        SnapshotV2DeviceTransport,
    ) {
        (
            self.config,
            self.active_queue,
            self.limiter,
            self.retry,
            self.pending,
            self.virtio,
            self.transport,
        )
    }

    /// Encodes the canonical entropy payload for an exact outer context.
    pub fn encode(
        &self,
        outer_version: SnapshotFormatVersion,
    ) -> Result<Vec<u8>, SnapshotV2EntropyStateEncodeError> {
        codec::encode(outer_version, self)
    }

    /// Decodes and validates one canonical exact-2.8 entropy payload.
    pub fn decode(
        outer_version: SnapshotFormatVersion,
        bytes: &[u8],
    ) -> Result<Self, SnapshotV2EntropyStateDecodeError> {
        codec::decode(outer_version, bytes)
    }
}

impl fmt::Debug for SnapshotV2EntropyState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2EntropyState")
            .field("version", &self.compatibility_version())
            .field("state", &REDACTED)
            .finish()
    }
}

pub(crate) fn validate_entropy_state(
    state: &SnapshotV2EntropyState,
) -> Result<(), SnapshotV2EntropyStateBuildError> {
    let rate_limiter = state.config.rate_limiter();
    if rate_limiter.is_some_and(|config| !config.is_configured()) {
        return Err(SnapshotV2EntropyStateBuildError::Configuration);
    }
    validate_limiter_relationship(rate_limiter, state.limiter)?;

    if state.pending != state.retry.has_retry()
        || matches!(
            state.retry,
            SnapshotV2EntropyRetryState::After { remaining_nanos: 0 }
        )
    {
        return Err(SnapshotV2EntropyStateBuildError::Retry);
    }
    if state.pending && (state.active_queue.is_none() || !state.limiter.is_enabled()) {
        return Err(SnapshotV2EntropyStateBuildError::Retry);
    }

    validate_virtio_with_queue_size(
        &state.virtio,
        VIRTIO_MMIO_VERSION_1_FEATURE,
        VIRTIO_RNG_QUEUE_SIZE,
    )
    .map_err(|_| SnapshotV2EntropyStateBuildError::Virtio)?;
    if state.virtio.config_generation() != 0
        || state.active_queue.is_some() != state.virtio.is_activated()
    {
        return Err(SnapshotV2EntropyStateBuildError::Virtio);
    }
    let queue = state
        .virtio
        .queues()
        .first()
        .ok_or(SnapshotV2EntropyStateBuildError::Virtio)?;
    if state.active_queue.is_some_and(|cursor| {
        cursor.outstanding() > queue.size() || (state.pending && cursor.outstanding() == 0)
    }) {
        return Err(SnapshotV2EntropyStateBuildError::Queue);
    }

    let placement = match &state.transport {
        SnapshotV2DeviceTransport::Mmio(mmio) => {
            validate_mmio(mmio).map_err(|_| SnapshotV2EntropyStateBuildError::Transport)?;
            mmio.region().range()
        }
        SnapshotV2DeviceTransport::Pci(pci) => {
            validate_pci(pci).map_err(|_| SnapshotV2EntropyStateBuildError::Transport)?;
            if pci.origin() != StorageDeviceOrigin::Startup {
                return Err(SnapshotV2EntropyStateBuildError::Transport);
            }
            pci.bar_range()
        }
    };
    if queue_ranges(queue)
        .map_err(|_| SnapshotV2EntropyStateBuildError::Queue)?
        .is_some_and(|ranges| ranges.into_iter().any(|range| range.overlaps(placement)))
    {
        return Err(SnapshotV2EntropyStateBuildError::Placement);
    }
    Ok(())
}

fn validate_limiter_relationship(
    config: Option<EntropyRateLimiterConfig>,
    state: SnapshotV2EntropyLimiterState,
) -> Result<(), SnapshotV2EntropyStateBuildError> {
    validate_bucket_relationship(
        config.and_then(EntropyRateLimiterConfig::bandwidth),
        state.bandwidth,
    )?;
    validate_bucket_relationship(config.and_then(EntropyRateLimiterConfig::ops), state.ops)
}

fn validate_bucket_relationship(
    config: Option<EntropyTokenBucketConfig>,
    state: Option<SnapshotV2EntropyBucketState>,
) -> Result<(), SnapshotV2EntropyStateBuildError> {
    match (config, state) {
        (None, None) => Ok(()),
        (Some(config), None) if !config.is_enabled() => Ok(()),
        (Some(config), Some(state))
            if config.is_enabled()
                && state.budget <= config.size()
                && state.remaining_burst <= config.one_time_burst().unwrap_or(0) =>
        {
            Ok(())
        }
        _ => Err(SnapshotV2EntropyStateBuildError::Limiter),
    }
}

/// Failure while constructing trusted exact-2.8 entropy state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2EntropyStateBuildError {
    /// External configuration is empty or noncanonical.
    Configuration,
    /// Active queue cursors are inconsistent.
    Queue,
    /// Token-bucket configuration and state disagree.
    Limiter,
    /// Pending-work and retry state disagree.
    Retry,
    /// Common virtio state is not canonical for virtio-rng.
    Virtio,
    /// MMIO or PCI transport state is not canonical for virtio-rng.
    Transport,
    /// Queue ranges overlap the selected transport placement.
    Placement,
}

impl fmt::Display for SnapshotV2EntropyStateBuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Configuration => "native-v2 entropy configuration is invalid",
            Self::Queue => "native-v2 entropy queue state is invalid",
            Self::Limiter => "native-v2 entropy limiter state is invalid",
            Self::Retry => "native-v2 entropy retry state is invalid",
            Self::Virtio => "native-v2 entropy virtio state is invalid",
            Self::Transport => "native-v2 entropy transport state is invalid",
            Self::Placement => "native-v2 entropy placement is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2EntropyStateBuildError {}

/// Failure while encoding trusted exact-2.8 entropy state.
#[derive(Debug)]
pub enum SnapshotV2EntropyStateEncodeError {
    /// The supplied outer semantic version is not exact 2.8.
    UnsupportedVersion,
    /// Trusted state no longer satisfies the canonical profile.
    InvalidState(SnapshotV2EntropyStateBuildError),
    /// Encoded length arithmetic overflowed.
    LengthOverflow,
    /// The encoded payload exceeds the fixed profile limit.
    TooLarge,
    /// The exact output buffer could not be reserved.
    Allocation,
}

impl fmt::Display for SnapshotV2EntropyStateEncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "native-v2 entropy encoding version is unsupported",
            Self::InvalidState(_) => "native-v2 entropy state is invalid",
            Self::LengthOverflow => "native-v2 entropy state length arithmetic overflowed",
            Self::TooLarge => "native-v2 entropy state exceeds its size limit",
            Self::Allocation => "native-v2 entropy output allocation failed",
        })
    }
}

impl std::error::Error for SnapshotV2EntropyStateEncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState(source) => Some(source),
            Self::UnsupportedVersion | Self::LengthOverflow | Self::TooLarge | Self::Allocation => {
                None
            }
        }
    }
}

/// Failure while decoding untrusted exact-2.8 entropy state.
#[derive(Debug)]
pub enum SnapshotV2EntropyStateDecodeError {
    /// The supplied outer semantic version is not exact 2.8.
    UnsupportedVersion,
    /// Input ends before a required bounded field.
    Truncated,
    /// The payload exceeds the fixed complete limit.
    TooLarge,
    /// Header magic is invalid.
    InvalidMagic,
    /// Header profile or transport tag is unsupported.
    UnsupportedProfile,
    /// Header or section layout is noncanonical.
    InvalidStructure,
    /// Flags, booleans, tags, or scalar relationships are invalid.
    InvalidValue,
    /// Reserved bytes or canonical padding are nonzero.
    NonzeroReserved,
    /// A bounded decoded collection could not be reserved.
    Allocation,
    /// Complete decoded semantics fail the final typed gate.
    InvalidState(SnapshotV2EntropyStateBuildError),
}

impl fmt::Display for SnapshotV2EntropyStateDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::UnsupportedVersion => "native-v2 entropy decoding version is unsupported",
            Self::Truncated => "native-v2 entropy state is truncated",
            Self::TooLarge => "native-v2 entropy state exceeds its bounds",
            Self::InvalidMagic => "native-v2 entropy state magic is invalid",
            Self::UnsupportedProfile => "native-v2 entropy state profile is unsupported",
            Self::InvalidStructure => "native-v2 entropy state structure is noncanonical",
            Self::InvalidValue => "native-v2 entropy state scalar value is invalid",
            Self::NonzeroReserved => "native-v2 entropy reserved bytes are nonzero",
            Self::Allocation => "native-v2 entropy state allocation failed",
            Self::InvalidState(_) => "native-v2 entropy state semantics are invalid",
        })
    }
}

impl std::error::Error for SnapshotV2EntropyStateDecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidState(source) => Some(source),
            Self::UnsupportedVersion
            | Self::Truncated
            | Self::TooLarge
            | Self::InvalidMagic
            | Self::UnsupportedProfile
            | Self::InvalidStructure
            | Self::InvalidValue
            | Self::NonzeroReserved
            | Self::Allocation => None,
        }
    }
}
