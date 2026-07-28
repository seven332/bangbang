//! Canonical detached native-v2 2.8 entropy state profile.
//!
//! This module contains only inert entropy configuration, token-bucket
//! continuation, common virtio registers, and transport placement. Entropy
//! bytes, source handles, host clock identity, interrupt authority, metrics,
//! and scheduler ownership remain destination-local.

use std::fmt;
use std::time::{Duration, Instant};

use crate::entropy::{
    EntropyConfig, EntropyRateLimiterConfig, EntropyTokenBucketConfig, VIRTIO_RNG_DEVICE_ID,
    VIRTIO_RNG_QUEUE_SIZE, VIRTIO_RNG_QUEUE_SIZES, VirtioRngDevice, VirtioRngDeviceCaptureState,
    VirtioRngMmioCaptureState, VirtioRngMmioHandler, VirtioRngPciCaptureState, VirtioRngQueue,
    VirtioRngRateLimiter, VirtioRngRateLimiterRestoreState, VirtioRngRetryCaptureState,
    VirtioRngTokenBucketCaptureState, VirtioRngTokenBucketRestoreState,
};
use crate::interrupt::GuestInterruptLine;
use crate::memory::{GuestMemory, GuestMemoryRange};
use crate::mmio::MmioRegion;
use crate::pci::PciSbdf;
use crate::snapshot_device_v2::{
    SnapshotV2DeviceGraphCaptureError, SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    SnapshotV2VirtioState, capture_mmio_common_for_device_with_config_status_gate,
    capture_mmio_transport, capture_pci_common_for_device, capture_pci_transport_parts,
    range_is_wholly_contained, restore_mmio_transport_state_for_device_with_config_status_gate,
};
use crate::snapshot_device_v2_5::{
    queue_ranges, validate_mmio, validate_pci, validate_virtio_with_queue_size,
};
use crate::snapshot_format::SnapshotFormatVersion;
use crate::storage_capture::StorageDeviceOrigin;
use crate::virtio::VirtioDeviceType;
use crate::virtio_mmio::{
    VIRTIO_MMIO_VERSION_1_FEATURE, VirtioMmioQueueState, VirtioMmioTransportState,
};
use crate::virtio_pci::{VirtioPciIdentity, VirtioPciTransportState};

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
    /// Converts one checked MMIO live capture without retaining source
    /// ownership.
    pub fn try_from_mmio_capture(
        config: EntropyConfig,
        retry: VirtioRngRetryCaptureState,
        region: MmioRegion,
        interrupt_line: GuestInterruptLine,
        captured: &VirtioRngMmioCaptureState,
    ) -> Result<Self, SnapshotV2EntropyStateCaptureError> {
        let virtio = capture_mmio_common_for_device_with_config_status_gate(
            captured.transport(),
            VIRTIO_RNG_DEVICE_ID,
            VIRTIO_MMIO_VERSION_1_FEATURE,
            false,
        )
        .map_err(capture_common_error)?;
        let transport = capture_mmio_transport(region, interrupt_line, captured.transport())
            .map(SnapshotV2DeviceTransport::Mmio)
            .map_err(capture_common_error)?;
        capture_entropy_state(config, retry, captured.device(), virtio, transport)
    }

    /// Converts one checked startup-origin PCI live capture without retaining
    /// source ownership.
    pub fn try_from_pci_capture(
        config: EntropyConfig,
        retry: VirtioRngRetryCaptureState,
        sbdf: PciSbdf,
        bar_range: GuestMemoryRange,
        captured: &VirtioRngPciCaptureState,
    ) -> Result<Self, SnapshotV2EntropyStateCaptureError> {
        let virtio = capture_pci_common_for_device(
            captured.transport(),
            VIRTIO_RNG_DEVICE_ID,
            VIRTIO_MMIO_VERSION_1_FEATURE,
        )
        .map_err(capture_common_error)?;
        let transport = capture_pci_transport_parts(
            StorageDeviceOrigin::Startup,
            sbdf,
            bar_range,
            captured.transport(),
        )
        .map(SnapshotV2DeviceTransport::Pci)
        .map_err(capture_common_error)?;
        capture_entropy_state(config, retry, captured.device(), virtio, transport)
    }

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

/// Complete pathless entropy continuation validated against loaded memory.
///
/// The plan contains only detached runtime and transport values. It owns no
/// entropy source, metrics, scheduler, notifier, interrupt route, dispatcher,
/// PCI endpoint, platform VM, or publication authority.
pub struct SnapshotV2EntropyRestorePlan {
    config: EntropyConfig,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    retry: SnapshotV2EntropyRetryState,
    retry_deadline: Option<Instant>,
    transport: PreparedSnapshotV2EntropyTransport,
}

impl SnapshotV2EntropyRestorePlan {
    /// Validates and reconstructs one decoded entropy continuation at a
    /// destination-local monotonic-time sample.
    pub fn prepare(
        state: SnapshotV2EntropyState,
        memory: &GuestMemory,
        now: Instant,
    ) -> Result<Self, SnapshotV2EntropyRestorePlanError> {
        validate_entropy_state(&state)
            .map_err(|_| SnapshotV2EntropyRestorePlanError::InvalidState)?;
        let (config, active_queue, limiter, retry, pending, virtio, transport) = state.into_parts();
        let queue_state = *virtio
            .queues()
            .first()
            .ok_or(SnapshotV2EntropyRestorePlanError::InvalidState)?;
        let queue_ranges = queue_ranges(&queue_state)
            .map_err(|_| SnapshotV2EntropyRestorePlanError::InvalidState)?;
        if queue_ranges.is_some_and(|ranges| {
            ranges
                .into_iter()
                .any(|range| !range_is_wholly_contained(memory, range))
        }) {
            return Err(SnapshotV2EntropyRestorePlanError::QueueMemory);
        }

        let active_queue = active_queue
            .map(|cursor| {
                let queue = VirtioMmioQueueState::from_parts(
                    queue_state.max_size(),
                    queue_state.size(),
                    queue_state.ready(),
                    queue_state.descriptor_table(),
                    queue_state.driver_ring(),
                    queue_state.device_ring(),
                );
                let queue = VirtioRngQueue::from_snapshot_state(
                    &queue,
                    cursor.next_available(),
                    cursor.next_used(),
                    pending,
                )
                .map_err(|_| SnapshotV2EntropyRestorePlanError::QueueContinuation)?;
                queue
                    .validate_snapshot_state(memory, pending)
                    .map_err(|_| SnapshotV2EntropyRestorePlanError::QueueContinuation)?;
                Ok(queue)
            })
            .transpose()?;

        let limiter = restore_limiter_state(config.rate_limiter(), limiter)?;
        let rate_limiter =
            VirtioRngRateLimiter::from_persisted_state_at(config.rate_limiter(), limiter, now)
                .map_err(|_| SnapshotV2EntropyRestorePlanError::RateLimiter)?;
        let retry_deadline = match retry {
            SnapshotV2EntropyRetryState::None => None,
            SnapshotV2EntropyRetryState::Immediate => Some(now),
            SnapshotV2EntropyRetryState::After { remaining_nanos } => Some(
                now.checked_add(Duration::from_nanos(remaining_nanos))
                    .ok_or(SnapshotV2EntropyRestorePlanError::Retry)?,
            ),
        };
        let device = VirtioRngDevice::from_snapshot_parts(active_queue, rate_limiter, pending);

        let transport = match transport {
            SnapshotV2DeviceTransport::Mmio(mmio) => {
                let retained = restore_mmio_transport_state_for_device_with_config_status_gate(
                    VIRTIO_RNG_DEVICE_ID,
                    &virtio,
                    &mmio,
                    false,
                )
                .map_err(|_| SnapshotV2EntropyRestorePlanError::MmioTransport)?;
                PreparedSnapshotV2EntropyTransport::Mmio(Box::new(
                    PreparedSnapshotV2EntropyMmioTransport {
                        region: mmio.region(),
                        interrupt_line: mmio.interrupt_line(),
                        device,
                        retained,
                    },
                ))
            }
            SnapshotV2DeviceTransport::Pci(pci) => {
                let device_type = VirtioDeviceType::new(VIRTIO_RNG_DEVICE_ID)
                    .map_err(|_| SnapshotV2EntropyRestorePlanError::DeviceType)?;
                let identity = VirtioPciIdentity::new(device_type, virtio.available_features())
                    .with_config_generation(virtio.config_generation());
                let retained =
                    VirtioPciTransportState::from_snapshot_v2_parts(identity, &virtio, &pci, false)
                        .map_err(|_| SnapshotV2EntropyRestorePlanError::PciTransport)?;
                PreparedSnapshotV2EntropyTransport::Pci(Box::new(
                    PreparedSnapshotV2EntropyPciTransport {
                        origin: pci.origin(),
                        sbdf: pci.sbdf(),
                        bar_range: pci.bar_range(),
                        identity,
                        device,
                        retained,
                    },
                ))
            }
        };

        Ok(Self {
            config,
            queue_ranges,
            retry,
            retry_deadline,
            transport,
        })
    }

    /// Returns the exact public entropy configuration.
    pub const fn config(&self) -> EntropyConfig {
        self.config
    }

    /// Returns the loaded-memory ranges occupied by an active queue.
    pub const fn queue_ranges(&self) -> Option<[GuestMemoryRange; 3]> {
        self.queue_ranges
    }

    /// Returns the retained retry disposition.
    pub const fn retry(&self) -> SnapshotV2EntropyRetryState {
        self.retry
    }

    /// Returns the destination-local retry deadline without scheduling it.
    pub const fn retry_deadline(&self) -> Option<Instant> {
        self.retry_deadline
    }

    /// Returns the selected transport kind.
    pub const fn transport_kind(&self) -> SnapshotV2DeviceTransportKind {
        self.transport.kind()
    }

    /// Returns the checked detached transport.
    pub const fn transport(&self) -> &PreparedSnapshotV2EntropyTransport {
        &self.transport
    }

    /// Consumes the plan into common continuation and detached transport.
    pub fn into_parts(
        self,
    ) -> (
        EntropyConfig,
        Option<[GuestMemoryRange; 3]>,
        SnapshotV2EntropyRetryState,
        Option<Instant>,
        PreparedSnapshotV2EntropyTransport,
    ) {
        (
            self.config,
            self.queue_ranges,
            self.retry,
            self.retry_deadline,
            self.transport,
        )
    }

    /// Consumes a checked MMIO plan into one complete inert register handler.
    ///
    /// The returned value still owns no dispatcher registration, interrupt
    /// route, entropy source, metrics, scheduler, notifier, or VM authority.
    #[doc(hidden)]
    pub fn into_mmio_handler(
        self,
    ) -> Result<PreparedSnapshotV2EntropyMmioHandler, SnapshotV2EntropyMmioHandlerError> {
        let Self {
            config,
            queue_ranges,
            retry,
            retry_deadline,
            transport,
        } = self;
        let PreparedSnapshotV2EntropyTransport::Mmio(mmio) = transport else {
            return Err(SnapshotV2EntropyMmioHandlerError::WrongTransport);
        };
        let (region, interrupt_line, device, retained) = mmio.into_parts();
        let activation_is_active = device.is_activated();
        let mut handler = VirtioRngMmioHandler::with_activation(
            VIRTIO_RNG_DEVICE_ID,
            0,
            &VIRTIO_RNG_QUEUE_SIZES,
            device,
        )
        .map_err(|_| SnapshotV2EntropyMmioHandlerError::Handler)?;
        handler
            .restore_transport_state(&retained, activation_is_active)
            .map_err(|_| SnapshotV2EntropyMmioHandlerError::Transport)?;

        Ok(PreparedSnapshotV2EntropyMmioHandler {
            config,
            queue_ranges,
            retry,
            retry_deadline,
            region,
            interrupt_line,
            handler,
        })
    }
}

impl fmt::Debug for SnapshotV2EntropyRestorePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SnapshotV2EntropyRestorePlan")
            .field("transport", &self.transport.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// One checked detached MMIO or PCI entropy transport.
pub enum PreparedSnapshotV2EntropyTransport {
    /// Value-only virtio-mmio continuation.
    Mmio(Box<PreparedSnapshotV2EntropyMmioTransport>),
    /// Value-only virtio-pci continuation.
    Pci(Box<PreparedSnapshotV2EntropyPciTransport>),
}

impl PreparedSnapshotV2EntropyTransport {
    /// Returns the selected transport kind.
    pub const fn kind(&self) -> SnapshotV2DeviceTransportKind {
        match self {
            Self::Mmio(_) => SnapshotV2DeviceTransportKind::Mmio,
            Self::Pci(_) => SnapshotV2DeviceTransportKind::Pci,
        }
    }
}

impl fmt::Debug for PreparedSnapshotV2EntropyTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2EntropyTransport")
            .field("kind", &self.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Checked value-only MMIO entropy continuation.
pub struct PreparedSnapshotV2EntropyMmioTransport {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    device: VirtioRngDevice,
    retained: VirtioMmioTransportState,
}

impl PreparedSnapshotV2EntropyMmioTransport {
    /// Returns the exact retained MMIO region.
    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    /// Returns the exact retained guest interrupt line.
    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    /// Returns the detached entropy device.
    pub const fn device(&self) -> &VirtioRngDevice {
        &self.device
    }

    /// Returns the detached MMIO register/queue/interrupt state.
    pub const fn retained(&self) -> &VirtioMmioTransportState {
        &self.retained
    }

    /// Consumes the value into placement, device, and retained transport.
    pub fn into_parts(
        self,
    ) -> (
        MmioRegion,
        GuestInterruptLine,
        VirtioRngDevice,
        VirtioMmioTransportState,
    ) {
        (self.region, self.interrupt_line, self.device, self.retained)
    }
}

impl fmt::Debug for PreparedSnapshotV2EntropyMmioTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2EntropyMmioTransport")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One checked, complete, and still unpublished MMIO entropy handler.
///
/// Destination-local source, metrics, scheduler, notifier, interrupt, VM, and
/// publication owners are intentionally absent.
#[doc(hidden)]
pub struct PreparedSnapshotV2EntropyMmioHandler {
    config: EntropyConfig,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
    retry: SnapshotV2EntropyRetryState,
    retry_deadline: Option<Instant>,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
    handler: VirtioRngMmioHandler,
}

impl PreparedSnapshotV2EntropyMmioHandler {
    /// Returns the exact public entropy configuration.
    pub const fn config(&self) -> EntropyConfig {
        self.config
    }

    /// Returns the loaded-memory ranges occupied by an active queue.
    pub const fn queue_ranges(&self) -> Option<[GuestMemoryRange; 3]> {
        self.queue_ranges
    }

    /// Returns the retained retry disposition.
    pub const fn retry(&self) -> SnapshotV2EntropyRetryState {
        self.retry
    }

    /// Returns the destination-local retry deadline without scheduling it.
    pub const fn retry_deadline(&self) -> Option<Instant> {
        self.retry_deadline
    }

    /// Returns the exact retained MMIO region.
    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    /// Returns the exact retained guest interrupt line.
    pub const fn interrupt_line(&self) -> GuestInterruptLine {
        self.interrupt_line
    }

    /// Returns the fully restored, still-unpublished handler.
    pub const fn handler(&self) -> &VirtioRngMmioHandler {
        &self.handler
    }

    /// Consumes the value into continuation, placement, and inert handler.
    pub fn into_parts(
        self,
    ) -> (
        EntropyConfig,
        Option<[GuestMemoryRange; 3]>,
        SnapshotV2EntropyRetryState,
        Option<Instant>,
        MmioRegion,
        GuestInterruptLine,
        VirtioRngMmioHandler,
    ) {
        (
            self.config,
            self.queue_ranges,
            self.retry,
            self.retry_deadline,
            self.region,
            self.interrupt_line,
            self.handler,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2EntropyMmioHandler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2EntropyMmioHandler")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Failure while materializing a checked entropy plan as an MMIO handler.
#[derive(Clone, Copy, PartialEq, Eq)]
#[doc(hidden)]
pub enum SnapshotV2EntropyMmioHandlerError {
    /// The checked plan selects PCI rather than MMIO.
    WrongTransport,
    /// The fixed virtio-rng handler could not be built.
    Handler,
    /// The retained common MMIO state could not be applied.
    Transport,
}

impl fmt::Debug for SnapshotV2EntropyMmioHandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2EntropyMmioHandlerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WrongTransport => "native-v2 entropy restore plan is not MMIO",
            Self::Handler => "native-v2 entropy MMIO handler construction failed",
            Self::Transport => "native-v2 entropy MMIO handler state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2EntropyMmioHandlerError {}

/// Checked value-only PCI entropy continuation.
pub struct PreparedSnapshotV2EntropyPciTransport {
    origin: StorageDeviceOrigin,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
    identity: VirtioPciIdentity,
    device: VirtioRngDevice,
    retained: VirtioPciTransportState,
}

impl PreparedSnapshotV2EntropyPciTransport {
    /// Returns the retained startup/runtime origin.
    pub const fn origin(&self) -> StorageDeviceOrigin {
        self.origin
    }

    /// Returns the exact retained PCI function.
    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }

    /// Returns the exact retained capability BAR.
    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.bar_range
    }

    /// Returns the fixed entropy PCI identity.
    pub const fn identity(&self) -> VirtioPciIdentity {
        self.identity
    }

    /// Returns the detached entropy device.
    pub const fn device(&self) -> &VirtioRngDevice {
        &self.device
    }

    /// Returns the detached PCI configuration/queue/MSI-X state.
    pub const fn retained(&self) -> &VirtioPciTransportState {
        &self.retained
    }

    /// Consumes the value into placement, identity, device, and retained state.
    pub fn into_parts(
        self,
    ) -> (
        StorageDeviceOrigin,
        PciSbdf,
        GuestMemoryRange,
        VirtioPciIdentity,
        VirtioRngDevice,
        VirtioPciTransportState,
    ) {
        (
            self.origin,
            self.sbdf,
            self.bar_range,
            self.identity,
            self.device,
            self.retained,
        )
    }
}

impl fmt::Debug for PreparedSnapshotV2EntropyPciTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedSnapshotV2EntropyPciTransport")
            .field("state", &REDACTED)
            .finish()
    }
}

fn restore_limiter_state(
    config: Option<EntropyRateLimiterConfig>,
    state: SnapshotV2EntropyLimiterState,
) -> Result<VirtioRngRateLimiterRestoreState, SnapshotV2EntropyRestorePlanError> {
    Ok(VirtioRngRateLimiterRestoreState::new(
        restore_bucket_state(
            config.and_then(EntropyRateLimiterConfig::bandwidth),
            state.bandwidth(),
        )?,
        restore_bucket_state(config.and_then(EntropyRateLimiterConfig::ops), state.ops())?,
    ))
}

fn restore_bucket_state(
    config: Option<EntropyTokenBucketConfig>,
    state: Option<SnapshotV2EntropyBucketState>,
) -> Result<Option<VirtioRngTokenBucketRestoreState>, SnapshotV2EntropyRestorePlanError> {
    match (config, state) {
        (Some(config), Some(state)) if config.is_enabled() => {
            Ok(Some(VirtioRngTokenBucketRestoreState::new(
                config,
                state.budget(),
                state.remaining_burst(),
                state.age_nanos(),
            )))
        }
        (Some(config), None) if !config.is_enabled() => Ok(None),
        (None, None) => Ok(None),
        _ => Err(SnapshotV2EntropyRestorePlanError::RateLimiter),
    }
}

/// Failure while proving decoded entropy state against destination memory.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2EntropyRestorePlanError {
    /// The decoded typed state no longer satisfies the exact profile.
    InvalidState,
    /// A queue range is not wholly contained by loaded guest memory.
    QueueMemory,
    /// Queue cursors, rings, or retained pending work are inconsistent.
    QueueContinuation,
    /// Token-bucket state cannot be restored at destination time.
    RateLimiter,
    /// Relative retry time cannot be represented at the destination sample.
    Retry,
    /// The fixed entropy virtio identity cannot be represented.
    DeviceType,
    /// Detached MMIO retained state reconstruction failed.
    MmioTransport,
    /// Detached PCI retained state reconstruction failed.
    PciTransport,
}

impl fmt::Debug for SnapshotV2EntropyRestorePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2EntropyRestorePlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidState => "native-v2 entropy restore state is invalid",
            Self::QueueMemory => "native-v2 entropy restore queue memory is invalid",
            Self::QueueContinuation => "native-v2 entropy restore queue continuation is invalid",
            Self::RateLimiter => "native-v2 entropy restore limiter state is invalid",
            Self::Retry => "native-v2 entropy restore retry state is invalid",
            Self::DeviceType => "native-v2 entropy restore device identity is invalid",
            Self::MmioTransport => "native-v2 entropy MMIO retained state is invalid",
            Self::PciTransport => "native-v2 entropy PCI retained state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2EntropyRestorePlanError {}

fn capture_entropy_state(
    config: EntropyConfig,
    retry: VirtioRngRetryCaptureState,
    device: &VirtioRngDeviceCaptureState,
    virtio: SnapshotV2VirtioState,
    transport: SnapshotV2DeviceTransport,
) -> Result<SnapshotV2EntropyState, SnapshotV2EntropyStateCaptureError> {
    if device.config() != config
        || device.available_features() != virtio.available_features()
        || device.negotiated_features() != virtio.driver_features()
        || device.active_queue().is_some() != virtio.is_activated()
    {
        return Err(SnapshotV2EntropyStateCaptureError::Device);
    }

    let active_queue = device
        .active_queue()
        .map(|queue| {
            let next_available = queue
                .next_available()
                .wrapping_add(u16::from(device.has_pending_rate_limited_queue()));
            SnapshotV2EntropyQueueState::try_new(
                next_available,
                queue.next_used(),
                VIRTIO_RNG_QUEUE_SIZE,
            )
        })
        .transpose()
        .map_err(|_| SnapshotV2EntropyStateCaptureError::Queue)?;
    let rate_limiter = device.rate_limiter();
    let rate_limiter_config = config.rate_limiter();
    let limiter = SnapshotV2EntropyLimiterState::try_new(
        rate_limiter_config,
        capture_bucket(
            rate_limiter_config.and_then(EntropyRateLimiterConfig::bandwidth),
            rate_limiter.bandwidth(),
        )?,
        capture_bucket(
            rate_limiter_config.and_then(EntropyRateLimiterConfig::ops),
            rate_limiter.ops(),
        )?,
    )
    .map_err(|_| SnapshotV2EntropyStateCaptureError::Limiter)?;
    let retry = match retry {
        VirtioRngRetryCaptureState::None => SnapshotV2EntropyRetryState::None,
        VirtioRngRetryCaptureState::Immediate => SnapshotV2EntropyRetryState::Immediate,
        VirtioRngRetryCaptureState::After { remaining_nanos } => {
            SnapshotV2EntropyRetryState::try_after(remaining_nanos)
                .map_err(|_| SnapshotV2EntropyStateCaptureError::Retry)?
        }
    };

    SnapshotV2EntropyState::try_new(
        config,
        active_queue,
        limiter,
        retry,
        device.has_pending_rate_limited_queue(),
        virtio,
        transport,
    )
    .map_err(capture_build_error)
}

fn capture_common_error(
    source: SnapshotV2DeviceGraphCaptureError,
) -> SnapshotV2EntropyStateCaptureError {
    if source == SnapshotV2DeviceGraphCaptureError::Allocation {
        SnapshotV2EntropyStateCaptureError::Allocation
    } else {
        SnapshotV2EntropyStateCaptureError::Common { source }
    }
}

fn capture_build_error(
    source: SnapshotV2EntropyStateBuildError,
) -> SnapshotV2EntropyStateCaptureError {
    match source {
        SnapshotV2EntropyStateBuildError::Queue => SnapshotV2EntropyStateCaptureError::Queue,
        SnapshotV2EntropyStateBuildError::Limiter
        | SnapshotV2EntropyStateBuildError::Configuration => {
            SnapshotV2EntropyStateCaptureError::Limiter
        }
        SnapshotV2EntropyStateBuildError::Retry => SnapshotV2EntropyStateCaptureError::Retry,
        source => SnapshotV2EntropyStateCaptureError::Build { source },
    }
}

fn capture_bucket(
    config: Option<EntropyTokenBucketConfig>,
    captured: Option<VirtioRngTokenBucketCaptureState>,
) -> Result<Option<SnapshotV2EntropyBucketState>, SnapshotV2EntropyStateCaptureError> {
    match (config, captured) {
        (Some(config), Some(captured)) if config.is_enabled() && captured.config() == config => {
            SnapshotV2EntropyBucketState::try_new(
                config,
                captured.budget(),
                captured.one_time_burst(),
                captured.age_nanos(),
            )
            .map(Some)
            .map_err(|_| SnapshotV2EntropyStateCaptureError::Limiter)
        }
        (Some(config), None) if !config.is_enabled() => Ok(None),
        (None, None) => Ok(None),
        _ => Err(SnapshotV2EntropyStateCaptureError::Limiter),
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

/// Failure while converting one trusted live capture into exact-2.8 state.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV2EntropyStateCaptureError {
    /// A bounded common-state collection could not be allocated.
    Allocation,
    /// Repeated device and transport state disagree.
    Device,
    /// Active queue cursors are inconsistent.
    Queue,
    /// Captured limiter configuration and state disagree.
    Limiter,
    /// Captured retry disposition is noncanonical.
    Retry,
    /// Common virtio or transport capture failed.
    Common {
        /// Redacted common capture category.
        source: SnapshotV2DeviceGraphCaptureError,
    },
    /// Complete converted state failed its final semantic gate.
    Build {
        /// Redacted exact-2.8 build category.
        source: SnapshotV2EntropyStateBuildError,
    },
}

impl fmt::Debug for SnapshotV2EntropyStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl fmt::Display for SnapshotV2EntropyStateCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Allocation => "native-v2 captured entropy state allocation failed",
            Self::Device => "native-v2 captured entropy device state is inconsistent",
            Self::Queue => "native-v2 captured entropy queue state is invalid",
            Self::Limiter => "native-v2 captured entropy limiter state is invalid",
            Self::Retry => "native-v2 captured entropy retry state is invalid",
            Self::Common { .. } => "native-v2 captured entropy transport state is invalid",
            Self::Build { .. } => "native-v2 captured entropy state is invalid",
        })
    }
}

impl std::error::Error for SnapshotV2EntropyStateCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Common { source } => Some(source),
            Self::Build { source } => Some(source),
            Self::Allocation | Self::Device | Self::Queue | Self::Limiter | Self::Retry => None,
        }
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
