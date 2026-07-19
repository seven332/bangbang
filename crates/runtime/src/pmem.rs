//! Backend-neutral pmem configuration model.

use std::collections::{BTreeMap, TryReserveError};
use std::ffi::c_void;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::ptr::{self, NonNull};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryLayout,
    GuestMemoryRange, aarch64,
};
use crate::mmio::{
    MmioAccessBytes, MmioAccessBytesError, MmioBusError, MmioDispatchError, MmioDispatcher,
    MmioHandlerError, MmioRegion, MmioRegionId,
};
use crate::token_bucket::{TokenBucket, TokenBucketConfig};
use crate::virtio::VirtioInterruptIntent;
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError,
    VirtioMmioDeviceActivationHandler, VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError,
    VirtioMmioDeviceConfigHandler, VirtioMmioQueueRegisterError, VirtioMmioQueueState,
    VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
};
use crate::virtio_pci::{VirtioPciDeviceOperationError, VirtioPciEndpoint, VirtioPciEndpointError};
use crate::virtio_queue::{
    VirtqueueAvailableRing, VirtqueueAvailableRingError, VirtqueueDescriptor,
    VirtqueueDescriptorChain, VirtqueueNotificationSuppression, VirtqueueUsedRing,
    VirtqueueUsedRingError, VirtqueueUsedRingPublication,
};

pub const VIRTIO_PMEM_DEVICE_ID: u32 = 27;
pub const VIRTIO_PMEM_QUEUE_COUNT: usize = 1;
pub const VIRTIO_PMEM_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_PMEM_QUEUE_SIZES: [u16; VIRTIO_PMEM_QUEUE_COUNT] = [VIRTIO_PMEM_QUEUE_SIZE];
pub const VIRTIO_PMEM_CONFIG_SPACE_SIZE: usize = 16;
pub const VIRTIO_PMEM_ALIGNMENT: u64 = 2 * 1024 * 1024;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;
pub const VIRTIO_PMEM_REQUEST_SIZE: u32 = 4;
pub const VIRTIO_PMEM_STATUS_SIZE: u32 = 4;
pub const VIRTIO_PMEM_REQUEST_TYPE_FLUSH: u32 = 0;
pub const VIRTIO_PMEM_STATUS_SUCCESS: i32 = 0;
pub const VIRTIO_PMEM_STATUS_FAILURE: i32 = -1;

pub type VirtioPmemMmioHandler = VirtioMmioRegisterHandler<VirtioPmemConfigSpace, VirtioPmemDevice>;
pub type VirtioPmemPciEndpoint = VirtioPciEndpoint<VirtioPmemConfigSpace, VirtioPmemDevice>;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioPmemConfigSpace {
    start: u64,
    size: u64,
}

impl VirtioPmemConfigSpace {
    pub const fn new(start: u64, size: u64) -> Self {
        Self { start, size }
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn available_features(self) -> u64 {
        virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
    }

    pub const fn from_le_bytes(bytes: [u8; VIRTIO_PMEM_CONFIG_SPACE_SIZE]) -> Self {
        let [
            start0,
            start1,
            start2,
            start3,
            start4,
            start5,
            start6,
            start7,
            size0,
            size1,
            size2,
            size3,
            size4,
            size5,
            size6,
            size7,
        ] = bytes;
        Self {
            start: u64::from_le_bytes([
                start0, start1, start2, start3, start4, start5, start6, start7,
            ]),
            size: u64::from_le_bytes([size0, size1, size2, size3, size4, size5, size6, size7]),
        }
    }

    pub fn to_le_bytes(self) -> [u8; VIRTIO_PMEM_CONFIG_SPACE_SIZE] {
        let [
            start0,
            start1,
            start2,
            start3,
            start4,
            start5,
            start6,
            start7,
        ] = self.start.to_le_bytes();
        let [size0, size1, size2, size3, size4, size5, size6, size7] = self.size.to_le_bytes();

        [
            start0, start1, start2, start3, start4, start5, start6, start7, size0, size1, size2,
            size3, size4, size5, size6, size7,
        ]
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioPmemConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let bytes = self.to_le_bytes();
        let bytes = read_virtio_pmem_config_bytes(&bytes, access)?;
        MmioAccessBytes::new(bytes).map_err(config_bytes_error)
    }

    fn write_device_config(
        &mut self,
        access: VirtioMmioDeviceConfigAccess,
        _data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        Err(VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioPmemFlushStatus {
    Success,
    Failure,
}

impl VirtioPmemFlushStatus {
    pub const fn from_result(is_success: bool) -> Self {
        if is_success {
            Self::Success
        } else {
            Self::Failure
        }
    }

    pub const fn status_code(self) -> i32 {
        match self {
            Self::Success => VIRTIO_PMEM_STATUS_SUCCESS,
            Self::Failure => VIRTIO_PMEM_STATUS_FAILURE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioPmemStatusDescriptor {
    index: u16,
    address: GuestAddress,
}

impl VirtioPmemStatusDescriptor {
    pub const fn index(self) -> u16 {
        self.index
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioPmemRequest {
    descriptor_head: u16,
    status: VirtioPmemStatusDescriptor,
}

impl VirtioPmemRequest {
    pub fn parse(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioPmemRequestError> {
        let request = descriptor_at(chain, 0, 1)?;
        validate_request_descriptor(request)?;
        let request_type = read_virtio_pmem_request_type(memory, request)?;
        if request_type != VIRTIO_PMEM_REQUEST_TYPE_FLUSH {
            return Err(VirtioPmemRequestError::UnsupportedRequestType { request_type });
        }

        let status = validate_status_descriptor(descriptor_at(chain, 1, 2)?)?;

        Ok(Self {
            descriptor_head: chain.head_index(),
            status,
        })
    }

    pub const fn descriptor_head(&self) -> u16 {
        self.descriptor_head
    }

    pub const fn status(&self) -> VirtioPmemStatusDescriptor {
        self.status
    }

    pub fn execute(
        &self,
        memory: &mut GuestMemory,
        flush_status: VirtioPmemFlushStatus,
    ) -> VirtioPmemRequestExecution {
        let status_code = flush_status.status_code();
        match memory.write_slice(&status_code.to_le_bytes(), self.status.address()) {
            Ok(()) => VirtioPmemRequestExecution::new(
                VirtioPmemRequestCompletion::new(self.descriptor_head, VIRTIO_PMEM_STATUS_SIZE),
                status_code,
                match flush_status {
                    VirtioPmemFlushStatus::Success => VirtioPmemRequestExecutionOutcome::Ok,
                    VirtioPmemFlushStatus::Failure => {
                        VirtioPmemRequestExecutionOutcome::FlushFailed
                    }
                },
            ),
            Err(source) => VirtioPmemRequestExecution::new(
                VirtioPmemRequestCompletion::new(self.descriptor_head, 0),
                status_code,
                VirtioPmemRequestExecutionOutcome::StatusWriteFailed {
                    address: self.status.address(),
                    source,
                },
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioPmemRequestCompletion {
    descriptor_head: u16,
    bytes_written_to_guest: u32,
}

impl VirtioPmemRequestCompletion {
    pub const fn new(descriptor_head: u16, bytes_written_to_guest: u32) -> Self {
        Self {
            descriptor_head,
            bytes_written_to_guest,
        }
    }

    pub const fn descriptor_head(self) -> u16 {
        self.descriptor_head
    }

    pub const fn bytes_written_to_guest(self) -> u32 {
        self.bytes_written_to_guest
    }
}

#[derive(Debug)]
pub struct VirtioPmemRequestExecution {
    completion: VirtioPmemRequestCompletion,
    status_code: i32,
    outcome: VirtioPmemRequestExecutionOutcome,
}

impl VirtioPmemRequestExecution {
    pub const fn new(
        completion: VirtioPmemRequestCompletion,
        status_code: i32,
        outcome: VirtioPmemRequestExecutionOutcome,
    ) -> Self {
        Self {
            completion,
            status_code,
            outcome,
        }
    }

    pub const fn completion(&self) -> VirtioPmemRequestCompletion {
        self.completion
    }

    pub const fn status_code(&self) -> i32 {
        self.status_code
    }

    pub const fn outcome(&self) -> &VirtioPmemRequestExecutionOutcome {
        &self.outcome
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioPmemRequestExecutionOutcome {
    Ok,
    FlushFailed,
    StatusWriteFailed {
        address: GuestAddress,
        source: GuestMemoryAccessError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioPmemRequestError {
    DescriptorChainTooShort {
        expected: usize,
        actual: usize,
    },
    RequestDescriptorWriteOnly {
        index: u16,
    },
    RequestDescriptorInvalidLength {
        index: u16,
        len: u32,
        expected: u32,
    },
    ReadRequest {
        address: GuestAddress,
        source: GuestMemoryAccessError,
    },
    UnsupportedRequestType {
        request_type: u32,
    },
    StatusDescriptorReadOnly {
        index: u16,
    },
    StatusDescriptorInvalidLength {
        index: u16,
        len: u32,
        expected: u32,
    },
}

impl fmt::Display for VirtioPmemRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DescriptorChainTooShort { expected, actual } => write!(
                f,
                "virtio-pmem descriptor chain has {actual} descriptor(s), expected at least {expected}"
            ),
            Self::RequestDescriptorWriteOnly { index } => {
                write!(f, "virtio-pmem request descriptor {index} is write-only")
            }
            Self::RequestDescriptorInvalidLength {
                index,
                len,
                expected,
            } => write!(
                f,
                "virtio-pmem request descriptor {index} has length {len}, expected {expected}"
            ),
            Self::ReadRequest { address, source } => {
                write!(
                    f,
                    "failed to read virtio-pmem request at {address}: {source}"
                )
            }
            Self::UnsupportedRequestType { request_type } => {
                write!(f, "unsupported virtio-pmem request type {request_type}")
            }
            Self::StatusDescriptorReadOnly { index } => {
                write!(f, "virtio-pmem status descriptor {index} is not writable")
            }
            Self::StatusDescriptorInvalidLength {
                index,
                len,
                expected,
            } => write!(
                f,
                "virtio-pmem status descriptor {index} has length {len}, expected {expected}"
            ),
        }
    }
}

impl std::error::Error for VirtioPmemRequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadRequest { source, .. } => Some(source),
            Self::DescriptorChainTooShort { .. }
            | Self::RequestDescriptorWriteOnly { .. }
            | Self::RequestDescriptorInvalidLength { .. }
            | Self::UnsupportedRequestType { .. }
            | Self::StatusDescriptorReadOnly { .. }
            | Self::StatusDescriptorInvalidLength { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioPmemQueue {
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
}

impl VirtioPmemQueue {
    pub const fn new(available: VirtqueueAvailableRing, used: VirtqueueUsedRing) -> Self {
        Self { available, used }
    }

    pub fn from_mmio_queue_state(
        queue: &VirtioMmioQueueState,
    ) -> Result<Self, VirtioPmemQueueBuildError> {
        if !queue.ready() {
            return Err(VirtioPmemQueueBuildError::QueueNotReady);
        }

        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioPmemQueueBuildError::AvailableRing { source })?;
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioPmemQueueBuildError::UsedRing { source })?;

        Ok(Self { available, used })
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
    }

    fn has_available_work(
        &self,
        memory: &GuestMemory,
    ) -> Result<bool, VirtqueueAvailableRingError> {
        let available_index = self.available.available_index(memory)?;
        let available_len = available_index.wrapping_sub(self.available.next_avail());
        if available_len > self.available.queue_size() {
            return Err(VirtqueueAvailableRingError::AvailableRingLengthTooLarge {
                queue_size: self.available.queue_size(),
                available_len,
            });
        }
        Ok(available_len != 0)
    }

    pub fn dispatch(
        &mut self,
        memory: &mut GuestMemory,
        flush: &mut impl FnMut() -> VirtioPmemFlushStatus,
    ) -> Result<VirtioPmemQueueDispatch, VirtioPmemQueueDispatchError> {
        let mut dispatch = VirtioPmemQueueDispatch::default();
        let mut cached_flush_status = None;
        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioPmemQueueDispatchError::AvailableRing {
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioPmemQueueDispatchError::EmptyDescriptorChain {
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;
            let (completion, outcome) = match VirtioPmemRequest::parse(memory, &chain) {
                Ok(request) => {
                    let flush_status = match cached_flush_status {
                        Some(status) => status,
                        None => {
                            let status = flush();
                            cached_flush_status = Some(status);
                            status
                        }
                    };
                    let execution = request.execute(memory, flush_status);
                    (
                        execution.completion(),
                        VirtioPmemQueueDispatchOutcome::from_execution(&execution),
                    )
                }
                Err(source) => (
                    VirtioPmemRequestCompletion::new(descriptor_head, 0),
                    VirtioPmemQueueDispatchOutcome::ParseError(source),
                ),
            };

            let publication = self
                .used
                .publish_used_element_with_notification(
                    memory,
                    completion.descriptor_head(),
                    completion.bytes_written_to_guest(),
                    VirtqueueNotificationSuppression::Disabled,
                )
                .map_err(|source| VirtioPmemQueueDispatchError::UsedRing {
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head: completion.descriptor_head(),
                    bytes_written_to_guest: completion.bytes_written_to_guest(),
                    source,
                })?;
            dispatch.record(outcome, publication);
        }

        Ok(dispatch)
    }
}

#[derive(Debug)]
pub enum VirtioPmemQueueBuildError {
    QueueNotReady,
    AvailableRing { source: VirtqueueAvailableRingError },
    UsedRing { source: VirtqueueUsedRingError },
}

impl fmt::Display for VirtioPmemQueueBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("virtio-pmem queue is not ready"),
            Self::AvailableRing { source } => {
                write!(f, "failed to build virtio-pmem available ring: {source}")
            }
            Self::UsedRing { source } => {
                write!(f, "failed to build virtio-pmem used ring: {source}")
            }
        }
    }
}

impl std::error::Error for VirtioPmemQueueBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source } => Some(source),
            Self::QueueNotReady => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtioPmemQueueDispatch {
    processed_requests: usize,
    successful_flushes: usize,
    failed_flushes: usize,
    parse_failures: usize,
    status_write_failures: usize,
    first_parse_failure: Option<VirtioPmemRequestError>,
    needs_queue_interrupt: bool,
    rate_limiter_throttled_events: usize,
    rate_limiter_events: usize,
    rate_limiter_retry_after: Option<Duration>,
}

impl VirtioPmemQueueDispatch {
    pub const fn processed_requests(&self) -> usize {
        self.processed_requests
    }

    pub const fn successful_flushes(&self) -> usize {
        self.successful_flushes
    }

    pub const fn failed_flushes(&self) -> usize {
        self.failed_flushes
    }

    pub const fn parse_failures(&self) -> usize {
        self.parse_failures
    }

    pub const fn first_parse_failure(&self) -> Option<&VirtioPmemRequestError> {
        self.first_parse_failure.as_ref()
    }

    pub const fn status_write_failures(&self) -> usize {
        self.status_write_failures
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.needs_queue_interrupt
    }

    pub const fn rate_limiter_throttled_events(&self) -> usize {
        self.rate_limiter_throttled_events
    }

    pub const fn rate_limiter_events(&self) -> usize {
        self.rate_limiter_events
    }

    pub const fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
    }

    fn record_rate_limiter_throttle(&mut self, retry_after: Duration) {
        self.rate_limiter_throttled_events += 1;
        self.rate_limiter_retry_after = Some(match self.rate_limiter_retry_after {
            Some(current) => current.min(retry_after),
            None => retry_after,
        });
    }

    fn record_rate_limiter_event(&mut self) {
        self.rate_limiter_events += 1;
    }

    fn record(
        &mut self,
        outcome: VirtioPmemQueueDispatchOutcome,
        publication: VirtqueueUsedRingPublication,
    ) {
        self.processed_requests += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        match outcome {
            VirtioPmemQueueDispatchOutcome::Ok => {
                self.successful_flushes += 1;
            }
            VirtioPmemQueueDispatchOutcome::FlushFailed => {
                self.failed_flushes += 1;
            }
            VirtioPmemQueueDispatchOutcome::ParseError(source) => {
                self.parse_failures += 1;
                if self.first_parse_failure.is_none() {
                    self.first_parse_failure = Some(source);
                }
            }
            VirtioPmemQueueDispatchOutcome::StatusWriteFailed => {
                self.status_write_failures += 1;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VirtioPmemQueueDispatchOutcome {
    Ok,
    FlushFailed,
    ParseError(VirtioPmemRequestError),
    StatusWriteFailed,
}

impl VirtioPmemQueueDispatchOutcome {
    const fn from_execution(execution: &VirtioPmemRequestExecution) -> Self {
        match execution.outcome() {
            VirtioPmemRequestExecutionOutcome::Ok => Self::Ok,
            VirtioPmemRequestExecutionOutcome::FlushFailed => Self::FlushFailed,
            VirtioPmemRequestExecutionOutcome::StatusWriteFailed { .. } => Self::StatusWriteFailed,
        }
    }
}

#[derive(Debug)]
pub enum VirtioPmemQueueDispatchError {
    AvailableRing {
        completed_dispatch: Box<VirtioPmemQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain {
        completed_dispatch: Box<VirtioPmemQueueDispatch>,
    },
    UsedRing {
        completed_dispatch: Box<VirtioPmemQueueDispatch>,
        descriptor_head: u16,
        bytes_written_to_guest: u32,
        source: VirtqueueUsedRingError,
    },
}

impl fmt::Display for VirtioPmemQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AvailableRing { source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-pmem available descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain { .. } => {
                f.write_str("virtio-pmem queue produced an empty descriptor chain")
            }
            Self::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to publish virtio-pmem used descriptor head {descriptor_head} with length {bytes_written_to_guest}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioPmemQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::EmptyDescriptorChain { .. } => None,
        }
    }
}

impl VirtioPmemQueueDispatchError {
    pub const fn completed_dispatch(&self) -> &VirtioPmemQueueDispatch {
        match self {
            Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            } => completed_dispatch,
        }
    }

    fn completed_dispatch_mut(&mut self) -> &mut VirtioPmemQueueDispatch {
        match self {
            Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            } => completed_dispatch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VirtioPmemRateLimiter {
    bandwidth: Option<TokenBucket>,
    ops: Option<TokenBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioPmemRateLimiterReduction {
    Allowed,
    Throttled { retry_after: Duration },
}

impl VirtioPmemRateLimiter {
    fn new_at(config: PmemRateLimiterConfig, now: Instant) -> Option<Self> {
        let bandwidth = config
            .bandwidth()
            .and_then(|bucket| TokenBucket::new_at(bucket.token_bucket_config(), now));
        let ops = config
            .ops()
            .and_then(|bucket| TokenBucket::new_at(bucket.token_bucket_config(), now));
        if bandwidth.is_none() && ops.is_none() {
            None
        } else {
            Some(Self { bandwidth, ops })
        }
    }

    fn updated_at(
        existing: Option<&Self>,
        update: PmemRateLimiterConfig,
        now: Instant,
    ) -> Option<Self> {
        let bandwidth = match update.bandwidth() {
            Some(config) => TokenBucket::new_at(config.token_bucket_config(), now),
            None => existing.and_then(|limiter| limiter.bandwidth.clone()),
        };
        let ops = match update.ops() {
            Some(config) => TokenBucket::new_at(config.token_bucket_config(), now),
            None => existing.and_then(|limiter| limiter.ops.clone()),
        };
        if bandwidth.is_none() && ops.is_none() {
            None
        } else {
            Some(Self { bandwidth, ops })
        }
    }

    fn reduce_at(&mut self, bytes: u64, now: Instant) -> VirtioPmemRateLimiterReduction {
        let ops_snapshot = self.ops.as_ref().map(TokenBucket::snapshot);

        if let Some(ops) = self.ops.as_mut()
            && let Some(retry_after) = ops.reduce_with_retry_at(1, now).retry_after()
        {
            return VirtioPmemRateLimiterReduction::Throttled { retry_after };
        }
        if let Some(bandwidth) = self.bandwidth.as_mut()
            && let Some(retry_after) = bandwidth
                .reduce_allow_overconsumption_with_retry_at(bytes, now)
                .retry_after()
        {
            if let (Some(ops), Some(snapshot)) = (self.ops.as_mut(), ops_snapshot) {
                ops.restore(snapshot);
            }
            return VirtioPmemRateLimiterReduction::Throttled { retry_after };
        }

        VirtioPmemRateLimiterReduction::Allowed
    }
}

#[derive(Debug, Default)]
pub struct VirtioPmemDevice {
    active_queue: Option<VirtioPmemQueue>,
    file_len: u64,
    rate_limiter: Option<VirtioPmemRateLimiter>,
    pending_rate_limited_queue: bool,
}

impl VirtioPmemDevice {
    pub const fn new() -> Self {
        Self {
            active_queue: None,
            file_len: 0,
            rate_limiter: None,
            pending_rate_limited_queue: false,
        }
    }

    pub fn with_rate_limiter(file_len: u64, rate_limiter: Option<PmemRateLimiterConfig>) -> Self {
        Self::with_rate_limiter_at(file_len, rate_limiter, Instant::now())
    }

    fn with_rate_limiter_at(
        file_len: u64,
        rate_limiter: Option<PmemRateLimiterConfig>,
        now: Instant,
    ) -> Self {
        Self {
            active_queue: None,
            file_len,
            rate_limiter: rate_limiter
                .and_then(|rate_limiter| VirtioPmemRateLimiter::new_at(rate_limiter, now)),
            pending_rate_limited_queue: false,
        }
    }

    pub fn is_activated(&self) -> bool {
        self.active_queue.is_some()
    }

    pub const fn active_queue(&self) -> Option<&VirtioPmemQueue> {
        self.active_queue.as_ref()
    }

    pub fn active_queue_mut(&mut self) -> Option<&mut VirtioPmemQueue> {
        self.active_queue.as_mut()
    }

    pub const fn has_pending_rate_limited_queue(&self) -> bool {
        self.pending_rate_limited_queue
    }

    fn update_rate_limiter_at(&mut self, update: &PmemUpdate, now: Instant) -> bool {
        if let Some(config) = update.rate_limiter() {
            self.rate_limiter =
                VirtioPmemRateLimiter::updated_at(self.rate_limiter.as_ref(), config, now);
        }
        self.pending_rate_limited_queue
    }

    fn dispatch_drained_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
        flush: &mut impl FnMut() -> VirtioPmemFlushStatus,
    ) -> Result<VirtioPmemDeviceNotificationDispatch, VirtioPmemDeviceNotificationError> {
        self.dispatch_drained_queue_notifications_at(
            memory,
            drained_notifications,
            flush,
            Instant::now(),
        )
    }

    fn dispatch_drained_queue_notifications_at(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
        flush: &mut impl FnMut() -> VirtioPmemFlushStatus,
        now: Instant,
    ) -> Result<VirtioPmemDeviceNotificationDispatch, VirtioPmemDeviceNotificationError> {
        if drained_notifications.is_empty() && !self.pending_rate_limited_queue {
            return Ok(VirtioPmemDeviceNotificationDispatch::new(
                drained_notifications,
                None,
            ));
        }

        if let Some(queue_index) = drained_notifications
            .iter()
            .copied()
            .find(|queue_index| *queue_index != 0)
        {
            return Err(VirtioPmemDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        let Some(queue) = self.active_queue.as_mut() else {
            return Err(VirtioPmemDeviceNotificationError::Inactive {
                drained_notifications,
            });
        };

        let had_pending_rate_limited_queue = self.pending_rate_limited_queue;
        let mut admission_dispatch = VirtioPmemQueueDispatch::default();
        if had_pending_rate_limited_queue {
            admission_dispatch.record_rate_limiter_event();
        }
        let has_available_work = match queue.has_available_work(memory) {
            Ok(has_available_work) => has_available_work,
            Err(source) => {
                self.pending_rate_limited_queue = false;
                return Err(VirtioPmemDeviceNotificationError::QueueDispatch {
                    drained_notifications,
                    source: VirtioPmemQueueDispatchError::AvailableRing {
                        completed_dispatch: Box::new(admission_dispatch),
                        source,
                    },
                });
            }
        };

        if !has_available_work {
            self.pending_rate_limited_queue = false;
            return Ok(VirtioPmemDeviceNotificationDispatch::new(
                drained_notifications,
                Some(admission_dispatch),
            ));
        }

        if let Some(rate_limiter) = self.rate_limiter.as_mut()
            && let VirtioPmemRateLimiterReduction::Throttled { retry_after } =
                rate_limiter.reduce_at(self.file_len, now)
        {
            admission_dispatch.record_rate_limiter_throttle(retry_after);
            self.pending_rate_limited_queue = true;
            return Ok(VirtioPmemDeviceNotificationDispatch::new(
                drained_notifications,
                Some(admission_dispatch),
            ));
        }

        match queue.dispatch(memory, flush) {
            Ok(mut dispatch) => {
                self.pending_rate_limited_queue = false;
                if had_pending_rate_limited_queue {
                    dispatch.record_rate_limiter_event();
                }
                Ok(VirtioPmemDeviceNotificationDispatch::new(
                    drained_notifications,
                    Some(dispatch),
                ))
            }
            Err(mut source) => {
                self.pending_rate_limited_queue = false;
                if had_pending_rate_limited_queue {
                    source.completed_dispatch_mut().record_rate_limiter_event();
                }
                Err(VirtioPmemDeviceNotificationError::QueueDispatch {
                    drained_notifications,
                    source,
                })
            }
        }
    }

    pub fn activate_pmem(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioPmemDeviceActivationError> {
        if self.active_queue.is_some() {
            return Err(VirtioPmemDeviceActivationError::AlreadyActive);
        }

        let queue_index = 0;
        let queue = activation
            .queue(queue_index)
            .map_err(|source| VirtioPmemDeviceActivationError::QueueMetadata {
                queue_index,
                source,
            })
            .and_then(|queue| {
                VirtioPmemQueue::from_mmio_queue_state(queue).map_err(|source| {
                    VirtioPmemDeviceActivationError::QueueBuild {
                        queue_index,
                        source,
                    }
                })
            })?;
        self.active_queue = Some(queue);

        Ok(())
    }

    pub fn reset(&mut self) {
        self.active_queue = None;
        self.pending_rate_limited_queue = false;
    }
}

impl<C: VirtioMmioDeviceConfigHandler> VirtioMmioRegisterHandler<C, VirtioPmemDevice> {
    pub fn update_pmem_rate_limiter(&mut self, update: &PmemUpdate) -> bool {
        self.activation_handler_mut()
            .update_rate_limiter_at(update, Instant::now())
    }

    pub fn has_pending_pmem_queue_work(&self) -> bool {
        self.has_pending_queue_notifications()
            || self.activation_handler().has_pending_rate_limited_queue()
    }

    pub fn dispatch_pmem_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        flush: &mut impl FnMut() -> VirtioPmemFlushStatus,
    ) -> Result<VirtioPmemDeviceNotificationDispatch, VirtioPmemDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        let dispatch = self
            .activation_handler_mut()
            .dispatch_drained_queue_notifications(memory, drained_notifications, flush);
        let needs_queue_interrupt = match &dispatch {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) => error
                .completed_dispatch()
                .is_some_and(VirtioPmemQueueDispatch::needs_queue_interrupt),
        };
        if needs_queue_interrupt {
            self.mark_queue_interrupt_pending(0);
        }

        dispatch
    }
}

impl VirtioPciEndpoint<VirtioPmemConfigSpace, VirtioPmemDevice> {
    pub fn update_pmem_rate_limiter(
        &self,
        update: &PmemUpdate,
    ) -> Result<bool, VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| {
            core.activation
                .update_rate_limiter_at(update, Instant::now())
        })
    }

    pub fn has_pending_pmem_queue_work(&self) -> Result<bool, VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| {
            !core
                .queue_notifications
                .pending_queue_notifications()
                .is_empty()
                || core.activation.has_pending_rate_limited_queue()
        })
    }

    pub fn dispatch_pmem_queue_notifications(
        &self,
        memory: &mut GuestMemory,
        flush: &mut impl FnMut() -> VirtioPmemFlushStatus,
    ) -> Result<
        VirtioPmemDeviceNotificationDispatch,
        VirtioPciDeviceOperationError<
            VirtioPmemDeviceNotificationError,
            VirtioPmemDeviceNotificationDispatch,
        >,
    > {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let dispatch = work
            .with_core_mut(|core| {
                let drained_notifications =
                    core.queue_notifications.take_pending_queue_notifications();
                let dispatch = core.activation.dispatch_drained_queue_notifications(
                    memory,
                    drained_notifications,
                    flush,
                );
                let needs_queue_interrupt = match &dispatch {
                    Ok(dispatch) => dispatch.needs_queue_interrupt(),
                    Err(error) => error
                        .completed_dispatch()
                        .is_some_and(VirtioPmemQueueDispatch::needs_queue_interrupt),
                };
                if needs_queue_interrupt {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index: 0 });
                }
                dispatch
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let endpoint = work.drain_interrupt_intents();
        VirtioPciDeviceOperationError::combine(dispatch, endpoint)
    }
}

impl VirtioMmioDeviceActivationHandler for VirtioPmemDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        self.activate_pmem(activation).map_err(Into::into)
    }

    fn reset(&mut self) {
        VirtioPmemDevice::reset(self);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioPmemDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
    queue_dispatch: Option<VirtioPmemQueueDispatch>,
}

impl VirtioPmemDeviceNotificationDispatch {
    const fn new(
        drained_notifications: Vec<usize>,
        queue_dispatch: Option<VirtioPmemQueueDispatch>,
    ) -> Self {
        Self {
            drained_notifications,
            queue_dispatch,
        }
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub const fn queue_dispatch(&self) -> Option<&VirtioPmemQueueDispatch> {
        self.queue_dispatch.as_ref()
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.queue_dispatch
            .as_ref()
            .is_some_and(VirtioPmemQueueDispatch::needs_queue_interrupt)
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.queue_dispatch
            .as_ref()
            .and_then(VirtioPmemQueueDispatch::rate_limiter_retry_after)
    }
}

#[derive(Debug)]
pub enum VirtioPmemDeviceNotificationError {
    Inactive {
        drained_notifications: Vec<usize>,
    },
    UnsupportedQueue {
        drained_notifications: Vec<usize>,
        queue_index: usize,
    },
    QueueDispatch {
        drained_notifications: Vec<usize>,
        source: VirtioPmemQueueDispatchError,
    },
}

impl VirtioPmemDeviceNotificationError {
    pub fn drained_notifications(&self) -> &[usize] {
        match self {
            Self::Inactive {
                drained_notifications,
            }
            | Self::UnsupportedQueue {
                drained_notifications,
                ..
            }
            | Self::QueueDispatch {
                drained_notifications,
                ..
            } => drained_notifications,
        }
    }

    pub const fn completed_dispatch(&self) -> Option<&VirtioPmemQueueDispatch> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source.completed_dispatch()),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.completed_dispatch()
            .and_then(VirtioPmemQueueDispatch::rate_limiter_retry_after)
    }
}

impl fmt::Display for VirtioPmemDeviceNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive { .. } => {
                f.write_str("virtio-pmem queue notification cannot be dispatched before activation")
            }
            Self::UnsupportedQueue { queue_index, .. } => {
                write!(
                    f,
                    "virtio-pmem queue notification for unsupported queue {queue_index}"
                )
            }
            Self::QueueDispatch { source, .. } => {
                write!(
                    f,
                    "virtio-pmem queue notification dispatch failed: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioPmemDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioPmemDeviceActivationError {
    AlreadyActive,
    QueueMetadata {
        queue_index: u32,
        source: VirtioMmioQueueRegisterError,
    },
    QueueBuild {
        queue_index: u32,
        source: VirtioPmemQueueBuildError,
    },
}

impl fmt::Display for VirtioPmemDeviceActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("virtio-pmem device is already active"),
            Self::QueueMetadata {
                queue_index,
                source,
            } => write!(
                f,
                "failed to read virtio-pmem queue {queue_index} metadata during activation: {source}"
            ),
            Self::QueueBuild {
                queue_index,
                source,
            } => write!(
                f,
                "failed to activate virtio-pmem queue {queue_index}: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioPmemDeviceActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueMetadata { source, .. } => Some(source),
            Self::QueueBuild { source, .. } => Some(source),
            Self::AlreadyActive => None,
        }
    }
}

impl From<VirtioPmemDeviceActivationError> for VirtioMmioDeviceActivationError {
    fn from(source: VirtioPmemDeviceActivationError) -> Self {
        Self::from(MmioHandlerError::new(source.to_string()))
    }
}

fn descriptor_chain_head(chain: &VirtqueueDescriptorChain) -> Option<u16> {
    if chain.is_empty() {
        None
    } else {
        Some(chain.head_index())
    }
}

fn descriptor_at(
    chain: &VirtqueueDescriptorChain,
    index: usize,
    expected: usize,
) -> Result<&VirtqueueDescriptor, VirtioPmemRequestError> {
    chain
        .descriptors()
        .get(index)
        .ok_or(VirtioPmemRequestError::DescriptorChainTooShort {
            expected,
            actual: chain.len(),
        })
}

fn validate_request_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<(), VirtioPmemRequestError> {
    if descriptor.is_write_only() {
        return Err(VirtioPmemRequestError::RequestDescriptorWriteOnly {
            index: descriptor.index(),
        });
    }

    if descriptor.len() != VIRTIO_PMEM_REQUEST_SIZE {
        return Err(VirtioPmemRequestError::RequestDescriptorInvalidLength {
            index: descriptor.index(),
            len: descriptor.len(),
            expected: VIRTIO_PMEM_REQUEST_SIZE,
        });
    }

    Ok(())
}

fn read_virtio_pmem_request_type(
    memory: &GuestMemory,
    descriptor: &VirtqueueDescriptor,
) -> Result<u32, VirtioPmemRequestError> {
    let mut bytes = [0; VIRTIO_PMEM_REQUEST_SIZE as usize];
    memory
        .read_slice(&mut bytes, descriptor.address())
        .map_err(|source| VirtioPmemRequestError::ReadRequest {
            address: descriptor.address(),
            source,
        })?;

    Ok(u32::from_le_bytes(bytes))
}

fn validate_status_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<VirtioPmemStatusDescriptor, VirtioPmemRequestError> {
    if !descriptor.is_write_only() {
        return Err(VirtioPmemRequestError::StatusDescriptorReadOnly {
            index: descriptor.index(),
        });
    }

    if descriptor.len() != VIRTIO_PMEM_STATUS_SIZE {
        return Err(VirtioPmemRequestError::StatusDescriptorInvalidLength {
            index: descriptor.index(),
            len: descriptor.len(),
            expected: VIRTIO_PMEM_STATUS_SIZE,
        });
    }

    Ok(VirtioPmemStatusDescriptor {
        index: descriptor.index(),
        address: descriptor.address(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmemRateLimiterConfig {
    bandwidth: Option<PmemTokenBucketConfig>,
    ops: Option<PmemTokenBucketConfig>,
}

impl PmemRateLimiterConfig {
    pub const fn new(
        bandwidth: Option<PmemTokenBucketConfig>,
        ops: Option<PmemTokenBucketConfig>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    pub const fn bandwidth(self) -> Option<PmemTokenBucketConfig> {
        self.bandwidth
    }

    pub const fn ops(self) -> Option<PmemTokenBucketConfig> {
        self.ops
    }

    pub const fn is_configured(self) -> bool {
        self.bandwidth.is_some() || self.ops.is_some()
    }

    const fn normalized(self) -> Option<Self> {
        let bandwidth = enabled_pmem_token_bucket(self.bandwidth);
        let ops = enabled_pmem_token_bucket(self.ops);
        if bandwidth.is_none() && ops.is_none() {
            None
        } else {
            Some(Self { bandwidth, ops })
        }
    }

    fn applied_to(self, existing: Option<Self>) -> Option<Self> {
        let bandwidth =
            updated_pmem_token_bucket(existing.and_then(Self::bandwidth), self.bandwidth);
        let ops = updated_pmem_token_bucket(existing.and_then(Self::ops), self.ops);
        if bandwidth.is_none() && ops.is_none() {
            None
        } else {
            Some(Self { bandwidth, ops })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmemTokenBucketConfig {
    size: u64,
    one_time_burst: Option<u64>,
    refill_time: u64,
}

impl PmemTokenBucketConfig {
    pub const fn new(size: u64, one_time_burst: Option<u64>, refill_time: u64) -> Self {
        Self {
            size,
            one_time_burst,
            refill_time,
        }
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn one_time_burst(self) -> Option<u64> {
        self.one_time_burst
    }

    pub const fn refill_time(self) -> u64 {
        self.refill_time
    }

    const fn token_bucket_config(self) -> TokenBucketConfig {
        TokenBucketConfig::new(self.size, self.one_time_burst, self.refill_time)
    }

    const fn is_enabled(self) -> bool {
        self.token_bucket_config().is_enabled()
    }
}

const fn enabled_pmem_token_bucket(
    bucket: Option<PmemTokenBucketConfig>,
) -> Option<PmemTokenBucketConfig> {
    match bucket {
        Some(bucket) if bucket.is_enabled() => Some(bucket),
        Some(_) | None => None,
    }
}

const fn updated_pmem_token_bucket(
    existing: Option<PmemTokenBucketConfig>,
    update: Option<PmemTokenBucketConfig>,
) -> Option<PmemTokenBucketConfig> {
    match update {
        Some(bucket) if bucket.is_enabled() => Some(bucket),
        Some(_) => None,
        None => existing,
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PmemConfigInput {
    id: String,
    path_on_host: String,
    root_device: bool,
    read_only: bool,
    rate_limiter: Option<PmemRateLimiterConfig>,
}

impl fmt::Debug for PmemConfigInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PmemConfigInput")
            .field("id", &self.id)
            .field("path_on_host", &"<redacted>")
            .field("root_device", &self.root_device)
            .field("read_only", &self.read_only)
            .field("rate_limiter", &self.rate_limiter)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemUpdateInput {
    path_pmem_id: String,
    body_pmem_id: String,
    rate_limiter: Option<PmemRateLimiterConfig>,
}

impl PmemConfigInput {
    pub fn new(id: impl Into<String>, path_on_host: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            path_on_host: path_on_host.into(),
            root_device: false,
            read_only: false,
            rate_limiter: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path_on_host(&self) -> &str {
        &self.path_on_host
    }

    pub const fn root_device(&self) -> bool {
        self.root_device
    }

    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    pub const fn rate_limiter(&self) -> Option<PmemRateLimiterConfig> {
        self.rate_limiter
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter.is_some()
    }

    pub const fn with_root_device(mut self, root_device: bool) -> Self {
        self.root_device = root_device;
        self
    }

    pub const fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub const fn with_rate_limiter(mut self, rate_limiter: PmemRateLimiterConfig) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }
}

impl PmemUpdateInput {
    pub fn new(path_pmem_id: impl Into<String>, body_pmem_id: impl Into<String>) -> Self {
        Self {
            path_pmem_id: path_pmem_id.into(),
            body_pmem_id: body_pmem_id.into(),
            rate_limiter: None,
        }
    }

    pub fn path_pmem_id(&self) -> &str {
        &self.path_pmem_id
    }

    pub fn body_pmem_id(&self) -> &str {
        &self.body_pmem_id
    }

    pub const fn rate_limiter(&self) -> Option<PmemRateLimiterConfig> {
        self.rate_limiter
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter.is_some()
    }

    pub const fn with_rate_limiter(mut self, rate_limiter: PmemRateLimiterConfig) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    pub fn validate(self) -> Result<PmemUpdate, PmemUpdateError> {
        PmemUpdate::try_from(self)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PmemConfig {
    id: String,
    path_on_host: String,
    root_device: bool,
    read_only: bool,
    rate_limiter: Option<PmemRateLimiterConfig>,
}

impl fmt::Debug for PmemConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PmemConfig")
            .field("id", &self.id)
            .field("path_on_host", &"<redacted>")
            .field("root_device", &self.root_device)
            .field("read_only", &self.read_only)
            .field("rate_limiter", &self.rate_limiter)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemUpdate {
    id: String,
    rate_limiter: Option<PmemRateLimiterConfig>,
}

impl PmemConfig {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path_on_host(&self) -> &str {
        &self.path_on_host
    }

    pub const fn root_device(&self) -> bool {
        self.root_device
    }

    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    pub const fn rate_limiter(&self) -> Option<PmemRateLimiterConfig> {
        self.rate_limiter
    }

    fn updated(&self, update: &PmemUpdate) -> Self {
        let rate_limiter = match update.rate_limiter() {
            Some(rate_limiter) => rate_limiter.applied_to(self.rate_limiter),
            None => self.rate_limiter,
        };
        Self {
            id: self.id.clone(),
            path_on_host: self.path_on_host.clone(),
            root_device: self.root_device,
            read_only: self.read_only,
            rate_limiter,
        }
    }
}

impl PmemUpdate {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn rate_limiter(&self) -> Option<PmemRateLimiterConfig> {
        self.rate_limiter
    }

    pub const fn is_noop(&self) -> bool {
        self.rate_limiter.is_none()
    }
}

impl TryFrom<PmemConfigInput> for PmemConfig {
    type Error = PmemConfigError;

    fn try_from(input: PmemConfigInput) -> Result<Self, Self::Error> {
        validate_pmem_id(&input.id)?;

        if input.path_on_host.is_empty() {
            return Err(PmemConfigError::EmptyPathOnHost);
        }

        if input.root_device {
            return Err(PmemConfigError::UnsupportedRootDevice);
        }

        Ok(Self {
            id: input.id,
            path_on_host: input.path_on_host,
            root_device: input.root_device,
            read_only: input.read_only,
            rate_limiter: input
                .rate_limiter
                .and_then(PmemRateLimiterConfig::normalized),
        })
    }
}

impl TryFrom<PmemUpdateInput> for PmemUpdate {
    type Error = PmemUpdateError;

    fn try_from(input: PmemUpdateInput) -> Result<Self, Self::Error> {
        validate_pmem_update_id(PmemIdSource::Path, &input.path_pmem_id)?;
        validate_pmem_update_id(PmemIdSource::Body, &input.body_pmem_id)?;
        if input.path_pmem_id != input.body_pmem_id {
            return Err(PmemUpdateError::MismatchedPmemId);
        }

        Ok(Self {
            id: input.path_pmem_id,
            rate_limiter: input.rate_limiter,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PmemConfigs {
    configs: Vec<PmemConfig>,
}

impl PmemConfigs {
    pub const fn new() -> Self {
        Self {
            configs: Vec::new(),
        }
    }

    pub fn as_slice(&self) -> &[PmemConfig] {
        &self.configs
    }

    pub fn upsert(&mut self, config: PmemConfig) {
        if let Some(existing) = self
            .configs
            .iter_mut()
            .find(|existing| existing.id == config.id)
        {
            *existing = config;
            return;
        }

        self.configs.push(config);
    }

    pub fn validate_update(&self, input: PmemUpdateInput) -> Result<PmemUpdate, PmemUpdateError> {
        let update = input.validate()?;
        if !self.configs.iter().any(|config| config.id() == update.id()) {
            return Err(PmemUpdateError::UnknownPmem);
        }

        Ok(update)
    }

    pub fn prepare_update(
        &self,
        input: PmemUpdateInput,
    ) -> Result<(PmemUpdate, PmemConfig), PmemUpdateError> {
        let update = self.validate_update(input)?;
        let Some(existing) = self
            .configs
            .iter()
            .find(|config| config.id() == update.id())
        else {
            return Err(PmemUpdateError::UnknownPmem);
        };
        Ok((update.clone(), existing.updated(&update)))
    }

    pub fn commit_update(&mut self, config: PmemConfig) -> Result<(), PmemUpdateError> {
        let Some(existing) = self
            .configs
            .iter_mut()
            .find(|existing| existing.id() == config.id())
        else {
            return Err(PmemUpdateError::UnknownPmem);
        };
        *existing = config;
        Ok(())
    }
}

pub struct PmemFileBacking {
    file: File,
    len: u64,
    read_only: bool,
}

impl fmt::Debug for PmemFileBacking {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PmemFileBacking")
            .field("file", &"<owned>")
            .field("len", &self.len)
            .field("read_only", &self.read_only)
            .finish()
    }
}

impl PmemFileBacking {
    pub fn open(config: &PmemConfig) -> Result<Self, PmemFileBackingError> {
        let file = open_pmem_file(config.path_on_host(), config.read_only())?;
        Self::from_file(file, config.read_only())
    }

    /// Adopts an already-opened pmem backing without resolving its configured path.
    pub fn from_file(file: File, read_only: bool) -> Result<Self, PmemFileBackingError> {
        let metadata = file
            .metadata()
            .map_err(|source| PmemFileBackingError::ReadMetadata { source })?;

        if !metadata.file_type().is_file() {
            return Err(PmemFileBackingError::NonRegularFile);
        }

        if metadata.len() == 0 {
            return Err(PmemFileBackingError::ZeroSizedFile);
        }

        Ok(Self {
            file,
            len: metadata.len(),
            read_only,
        })
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub const fn len(&self) -> u64 {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

fn open_pmem_file(path: &str, read_only: bool) -> Result<File, PmemFileBackingError> {
    let mut options = OpenOptions::new();
    options.read(true).write(!read_only);

    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NONBLOCK);
    }

    options
        .open(path)
        .map_err(|source| PmemFileBackingError::OpenFile { source })
}

#[derive(Debug)]
pub enum PmemFileBackingError {
    OpenFile { source: io::Error },
    ReadMetadata { source: io::Error },
    NonRegularFile,
    ZeroSizedFile,
}

impl fmt::Display for PmemFileBackingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenFile { source } => write!(f, "failed to open pmem backing file: {source}"),
            Self::ReadMetadata { source } => {
                write!(f, "failed to read pmem backing file metadata: {source}")
            }
            Self::NonRegularFile => {
                f.write_str("pmem backing path does not reference a regular file")
            }
            Self::ZeroSizedFile => f.write_str("pmem backing file is zero-sized"),
        }
    }
}

impl std::error::Error for PmemFileBackingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OpenFile { source } | Self::ReadMetadata { source } => Some(source),
            Self::NonRegularFile | Self::ZeroSizedFile => None,
        }
    }
}

pub struct PmemBackingMapping {
    address: NonNull<c_void>,
    file_len: u64,
    mapped_len: u64,
    host_size: usize,
    read_only: bool,
    kind: PmemBackingMappingKind,
}

// SAFETY: `PmemBackingMapping` owns a process-local mmap region. Moving the
// owner to another thread does not invalidate the mapping, and `munmap` may run
// from any thread when ownership is dropped.
unsafe impl Send for PmemBackingMapping {}

// SAFETY: Shared references expose only copyable metadata and a raw pointer.
// Safe Rust cannot mutate mapped bytes through this type, and unsafe users must
// uphold normal raw-pointer aliasing and lifetime requirements.
unsafe impl Sync for PmemBackingMapping {}

impl PmemBackingMapping {
    pub fn map(backing: &PmemFileBacking) -> Result<Self, PmemBackingMappingError> {
        let mapped_len = align_pmem_mapping_len(backing.len())?;
        let file_len = usize::try_from(backing.len())
            .map_err(|_| PmemBackingMappingError::FileLengthTooLarge { len: backing.len() })?;
        let host_size = usize::try_from(mapped_len).map_err(|_| {
            PmemBackingMappingError::MappedLengthTooLarge {
                len: backing.len(),
                mapped_len,
            }
        })?;
        let prot = pmem_mapping_protection(backing.is_read_only());
        let address = map_pmem_file(backing.file(), prot, file_len, host_size)?;

        Ok(Self {
            address,
            file_len: backing.len(),
            mapped_len,
            host_size,
            read_only: backing.is_read_only(),
            kind: PmemBackingMappingKind::System,
        })
    }

    #[cfg(test)]
    fn test_mapping(
        file_len: u64,
        mapped_len: u64,
        read_only: bool,
        drop_count: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            address: NonNull::<u8>::dangling().cast(),
            file_len,
            mapped_len,
            host_size: usize::try_from(mapped_len).expect("test mapped length should fit in usize"),
            read_only,
            kind: PmemBackingMappingKind::Test { drop_count },
        }
    }

    pub const fn host_address(&self) -> NonNull<c_void> {
        self.address
    }

    pub const fn file_len(&self) -> u64 {
        self.file_len
    }

    pub const fn mapped_len(&self) -> u64 {
        self.mapped_len
    }

    pub const fn host_size(&self) -> usize {
        self.host_size
    }

    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

impl fmt::Debug for PmemBackingMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PmemBackingMapping")
            .field("file_len", &self.file_len)
            .field("mapped_len", &self.mapped_len)
            .field("host_size", &self.host_size)
            .field("read_only", &self.read_only)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
enum PmemBackingMappingKind {
    System,
    #[cfg(test)]
    Test {
        drop_count: Arc<AtomicUsize>,
    },
}

impl Drop for PmemBackingMapping {
    fn drop(&mut self) {
        match &self.kind {
            PmemBackingMappingKind::System => {
                // SAFETY: system mappings are constructed only after `mmap`
                // succeeds and each `PmemBackingMapping` owns one mapping.
                unsafe {
                    let _ = libc::munmap(self.address.as_ptr(), self.host_size);
                }
            }
            #[cfg(test)]
            PmemBackingMappingKind::Test { drop_count } => {
                drop_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Debug)]
pub enum PmemBackingMappingError {
    MappedLengthOverflow {
        len: u64,
        alignment: u64,
    },
    FileLengthTooLarge {
        len: u64,
    },
    MappedLengthTooLarge {
        len: u64,
        mapped_len: u64,
    },
    MapFile {
        len: usize,
        source: io::Error,
    },
    ReserveAlignedMapping {
        len: usize,
        source: io::Error,
    },
    MapFileOverReservation {
        file_len: usize,
        mapped_len: usize,
        source: io::Error,
        cleanup_source: Option<io::Error>,
    },
    FileMappingReturnedNull {
        file_len: usize,
        mapped_len: usize,
        cleanup_source: Option<io::Error>,
    },
    MmapReturnedNull {
        len: usize,
    },
    FixedMappingMoved {
        cleanup_source: Option<io::Error>,
    },
}

impl fmt::Display for PmemBackingMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MappedLengthOverflow { len, alignment } => write!(
                f,
                "pmem backing length {len} cannot be aligned to {alignment} bytes without overflow"
            ),
            Self::FileLengthTooLarge { len } => {
                write!(f, "pmem backing length {len} does not fit this host")
            }
            Self::MappedLengthTooLarge { len, mapped_len } => write!(
                f,
                "pmem backing length {len} maps to {mapped_len} bytes, which does not fit this host"
            ),
            Self::MapFile { len, source } => {
                write!(
                    f,
                    "failed to mmap pmem backing file with length {len}: {source}"
                )
            }
            Self::ReserveAlignedMapping { len, source } => write!(
                f,
                "failed to reserve aligned pmem mapping with length {len}: {source}"
            ),
            Self::MapFileOverReservation {
                file_len,
                mapped_len,
                source,
                cleanup_source,
            } => {
                if let Some(cleanup_source) = cleanup_source {
                    write!(
                        f,
                        "failed to mmap pmem backing file with length {file_len} over reserved length {mapped_len}: {source}; also failed to clean up the reserved mapping: {cleanup_source}"
                    )
                } else {
                    write!(
                        f,
                        "failed to mmap pmem backing file with length {file_len} over reserved length {mapped_len}: {source}"
                    )
                }
            }
            Self::FileMappingReturnedNull {
                file_len,
                mapped_len,
                cleanup_source,
            } => {
                if let Some(cleanup_source) = cleanup_source {
                    write!(
                        f,
                        "pmem backing file mapping with length {file_len} over reserved length {mapped_len} returned a null address; also failed to clean up the reserved mapping: {cleanup_source}"
                    )
                } else {
                    write!(
                        f,
                        "pmem backing file mapping with length {file_len} over reserved length {mapped_len} returned a null address"
                    )
                }
            }
            Self::MmapReturnedNull { len } => {
                write!(f, "pmem mapping with length {len} returned a null address")
            }
            Self::FixedMappingMoved { cleanup_source, .. } => {
                if let Some(cleanup_source) = cleanup_source {
                    write!(
                        f,
                        "fixed pmem file mapping did not reuse the reserved address; also failed to clean up the reserved mapping: {cleanup_source}"
                    )
                } else {
                    f.write_str("fixed pmem file mapping did not reuse the reserved address")
                }
            }
        }
    }
}

impl std::error::Error for PmemBackingMappingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapFile { source, .. } | Self::ReserveAlignedMapping { source, .. } => {
                Some(source)
            }
            Self::MapFileOverReservation { source, .. } => Some(source),
            Self::MappedLengthOverflow { .. }
            | Self::FileLengthTooLarge { .. }
            | Self::MappedLengthTooLarge { .. }
            | Self::FileMappingReturnedNull { .. }
            | Self::MmapReturnedNull { .. }
            | Self::FixedMappingMoved { .. } => None,
        }
    }
}

fn align_pmem_mapping_len(len: u64) -> Result<u64, PmemBackingMappingError> {
    let remainder = len % VIRTIO_PMEM_ALIGNMENT;
    if remainder == 0 {
        return Ok(len);
    }

    let padding = VIRTIO_PMEM_ALIGNMENT - remainder;
    len.checked_add(padding)
        .ok_or(PmemBackingMappingError::MappedLengthOverflow {
            len,
            alignment: VIRTIO_PMEM_ALIGNMENT,
        })
}

const fn pmem_mapping_protection(read_only: bool) -> libc::c_int {
    let mut prot = libc::PROT_READ;
    if !read_only {
        prot |= libc::PROT_WRITE;
    }
    prot
}

fn map_pmem_file(
    file: &File,
    prot: libc::c_int,
    file_len: usize,
    host_size: usize,
) -> Result<NonNull<c_void>, PmemBackingMappingError> {
    if file_len == host_size {
        return map_pmem_file_exact(file, prot, file_len);
    }

    map_pmem_file_with_aligned_reservation(file, prot, file_len, host_size)
}

fn map_pmem_file_exact(
    file: &File,
    prot: libc::c_int,
    file_len: usize,
) -> Result<NonNull<c_void>, PmemBackingMappingError> {
    // SAFETY: The call requests a new shared mapping for the already-open
    // regular backing file. Lengths are non-zero, fit usize, and the result is
    // checked before ownership is created.
    let address = unsafe {
        libc::mmap(
            ptr::null_mut(),
            file_len,
            prot,
            libc::MAP_SHARED | libc::MAP_NORESERVE,
            file.as_raw_fd(),
            0,
        )
    };

    if address == libc::MAP_FAILED {
        return Err(PmemBackingMappingError::MapFile {
            len: file_len,
            source: io::Error::last_os_error(),
        });
    }

    non_null_mapping(address, file_len)
}

fn map_pmem_file_with_aligned_reservation(
    file: &File,
    prot: libc::c_int,
    file_len: usize,
    host_size: usize,
) -> Result<NonNull<c_void>, PmemBackingMappingError> {
    // SAFETY: The call reserves a private anonymous region with the final
    // aligned length. No Rust references are created from the raw mapping.
    let reserved = unsafe {
        libc::mmap(
            ptr::null_mut(),
            host_size,
            prot,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };

    if reserved == libc::MAP_FAILED {
        return Err(PmemBackingMappingError::ReserveAlignedMapping {
            len: host_size,
            source: io::Error::last_os_error(),
        });
    }

    let reserved = non_null_mapping(reserved, host_size)?;

    // SAFETY: `reserved` owns a live mapping of `host_size` bytes. This maps
    // the file over the prefix at the same address, matching Firecracker's
    // aligned pmem mapping shape.
    let file_address = unsafe {
        libc::mmap(
            reserved.as_ptr(),
            file_len,
            prot,
            libc::MAP_SHARED | libc::MAP_FIXED | libc::MAP_NORESERVE,
            file.as_raw_fd(),
            0,
        )
    };

    if file_address == libc::MAP_FAILED {
        let source = io::Error::last_os_error();
        let cleanup_source = unmap_pmem_region(reserved, host_size).err();
        return Err(PmemBackingMappingError::MapFileOverReservation {
            file_len,
            mapped_len: host_size,
            source,
            cleanup_source,
        });
    }

    let file_address = match NonNull::new(file_address) {
        Some(file_address) => file_address,
        None => {
            let cleanup_source = unmap_pmem_region(reserved, host_size).err();
            return Err(PmemBackingMappingError::FileMappingReturnedNull {
                file_len,
                mapped_len: host_size,
                cleanup_source,
            });
        }
    };

    if file_address != reserved {
        let cleanup_source = unmap_pmem_region(reserved, host_size).err();
        return Err(PmemBackingMappingError::FixedMappingMoved { cleanup_source });
    }

    Ok(reserved)
}

fn non_null_mapping(
    address: *mut c_void,
    len: usize,
) -> Result<NonNull<c_void>, PmemBackingMappingError> {
    let Some(address) = NonNull::new(address) else {
        // SAFETY: `mmap` reported success, so the returned address and length
        // describe a live mapping even if the address is null.
        unsafe {
            let _ = libc::munmap(address, len);
        }

        return Err(PmemBackingMappingError::MmapReturnedNull { len });
    };

    Ok(address)
}

fn unmap_pmem_region(address: NonNull<c_void>, len: usize) -> Result<(), io::Error> {
    // SAFETY: Callers pass only addresses returned by successful `mmap` calls
    // and the same length used to create the mapping.
    let result = unsafe { libc::munmap(address.as_ptr(), len) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

trait PmemBackingMapper {
    fn map(
        &mut self,
        backing: &PmemFileBacking,
    ) -> Result<PmemBackingMapping, PmemBackingMappingError>;
}

#[derive(Debug, Default)]
struct SystemPmemBackingMapper;

impl PmemBackingMapper for SystemPmemBackingMapper {
    fn map(
        &mut self,
        backing: &PmemFileBacking,
    ) -> Result<PmemBackingMapping, PmemBackingMappingError> {
        PmemBackingMapping::map(backing)
    }
}

#[derive(Debug)]
pub struct PreparedPmemDevice {
    id: String,
    backing: PmemFileBacking,
    mapping: PmemBackingMapping,
    guest_range: GuestMemoryRange,
    config_space: VirtioPmemConfigSpace,
    rate_limiter: Option<PmemRateLimiterConfig>,
}

impl PreparedPmemDevice {
    fn from_config_with_backing_mapper_and_allocator(
        config: &PmemConfig,
        backing: Option<PmemFileBacking>,
        mapper: &mut impl PmemBackingMapper,
        allocator: &mut PmemGuestRangeAllocator<'_>,
    ) -> Result<Self, PreparedPmemDeviceError> {
        let backing = match backing {
            Some(backing) => {
                if backing.is_read_only() != config.read_only() {
                    return Err(PreparedPmemDeviceError::BackingModeMismatch {
                        pmem_id: config.id().to_string(),
                    });
                }
                backing
            }
            None => PmemFileBacking::open(config).map_err(|source| {
                PreparedPmemDeviceError::OpenBacking {
                    pmem_id: config.id().to_string(),
                    source,
                }
            })?,
        };
        let mapping =
            mapper
                .map(&backing)
                .map_err(|source| PreparedPmemDeviceError::MapBacking {
                    pmem_id: config.id().to_string(),
                    source,
                })?;
        let guest_range = allocator.allocate(mapping.mapped_len()).map_err(|source| {
            PreparedPmemDeviceError::AllocateGuestRange {
                pmem_id: config.id().to_string(),
                source,
            }
        })?;
        let config_space =
            VirtioPmemConfigSpace::new(guest_range.start().raw_value(), guest_range.size());

        Ok(Self {
            id: config.id().to_string(),
            backing,
            mapping,
            guest_range,
            config_space,
            rate_limiter: config.rate_limiter(),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn backing(&self) -> &PmemFileBacking {
        &self.backing
    }

    pub const fn mapping(&self) -> &PmemBackingMapping {
        &self.mapping
    }

    pub const fn guest_range(&self) -> GuestMemoryRange {
        self.guest_range
    }

    pub const fn config_space(&self) -> VirtioPmemConfigSpace {
        self.config_space
    }

    pub const fn rate_limiter(&self) -> Option<PmemRateLimiterConfig> {
        self.rate_limiter
    }

    pub fn into_parts(
        self,
    ) -> (
        String,
        PmemFileBacking,
        PmemBackingMapping,
        GuestMemoryRange,
        VirtioPmemConfigSpace,
        Option<PmemRateLimiterConfig>,
    ) {
        (
            self.id,
            self.backing,
            self.mapping,
            self.guest_range,
            self.config_space,
            self.rate_limiter,
        )
    }
}

#[derive(Debug, Default)]
pub struct PreparedPmemDevices {
    devices: Vec<PreparedPmemDevice>,
}

impl PreparedPmemDevices {
    pub fn from_configs(
        configs: &PmemConfigs,
        layout: &GuestMemoryLayout,
    ) -> Result<Self, PreparedPmemDeviceError> {
        Self::from_config_slice_with_layout(configs.as_slice(), layout)
    }

    pub(crate) fn from_config_slice_with_layout(
        configs: &[PmemConfig],
        layout: &GuestMemoryLayout,
    ) -> Result<Self, PreparedPmemDeviceError> {
        Self::from_config_slice_with_layout_and_backings(configs, layout, BTreeMap::new())
    }

    pub(crate) fn from_config_slice_with_layout_and_backings(
        configs: &[PmemConfig],
        layout: &GuestMemoryLayout,
        backings: BTreeMap<String, PmemFileBacking>,
    ) -> Result<Self, PreparedPmemDeviceError> {
        let mut mapper = SystemPmemBackingMapper;
        let mut allocator = PmemGuestRangeAllocator::for_layout(layout);
        Self::from_config_slice_with_backings_mapper_and_allocator(
            configs,
            backings,
            &mut mapper,
            &mut allocator,
        )
    }

    #[cfg(test)]
    fn from_config_slice(configs: &[PmemConfig]) -> Result<Self, PreparedPmemDeviceError> {
        let mut mapper = SystemPmemBackingMapper;
        Self::from_config_slice_with_mapper(configs, &mut mapper)
    }

    #[cfg(test)]
    fn from_config_slice_with_mapper(
        configs: &[PmemConfig],
        mapper: &mut impl PmemBackingMapper,
    ) -> Result<Self, PreparedPmemDeviceError> {
        let mut allocator = PmemGuestRangeAllocator::without_reserved_ranges();
        Self::from_config_slice_with_mapper_and_allocator(configs, mapper, &mut allocator)
    }

    #[cfg(test)]
    fn from_config_slice_with_mapper_and_allocator(
        configs: &[PmemConfig],
        mapper: &mut impl PmemBackingMapper,
        allocator: &mut PmemGuestRangeAllocator<'_>,
    ) -> Result<Self, PreparedPmemDeviceError> {
        Self::from_config_slice_with_backings_mapper_and_allocator(
            configs,
            BTreeMap::new(),
            mapper,
            allocator,
        )
    }

    fn from_config_slice_with_backings_mapper_and_allocator(
        configs: &[PmemConfig],
        mut backings: BTreeMap<String, PmemFileBacking>,
        mapper: &mut impl PmemBackingMapper,
        allocator: &mut PmemGuestRangeAllocator<'_>,
    ) -> Result<Self, PreparedPmemDeviceError> {
        if backings
            .keys()
            .any(|pmem_id| !configs.iter().any(|config| config.id() == pmem_id))
        {
            return Err(PreparedPmemDeviceError::UnexpectedBacking);
        }

        let mut devices = Vec::new();
        devices
            .try_reserve_exact(configs.len())
            .map_err(|source| PreparedPmemDeviceError::AllocateDevices { source })?;

        for config in configs {
            let backing = backings.remove(config.id());
            devices.push(
                PreparedPmemDevice::from_config_with_backing_mapper_and_allocator(
                    config, backing, mapper, allocator,
                )?,
            );
        }
        debug_assert!(backings.is_empty());

        Ok(Self { devices })
    }

    pub fn as_slice(&self) -> &[PreparedPmemDevice] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn into_vec(self) -> Vec<PreparedPmemDevice> {
        self.devices
    }

    pub fn register_mmio(
        self,
        layout: PmemMmioLayout,
    ) -> Result<PmemMmioDevices, PmemMmioRegistrationError> {
        PmemMmioDevices::from_prepared(self, layout)
    }

    pub fn register_mmio_with_dispatcher(
        self,
        layout: PmemMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<PmemMmioDevices, PmemMmioRegistrationError> {
        PmemMmioDevices::from_prepared_with_dispatcher(self, layout, dispatcher)
    }
}

#[derive(Debug)]
pub enum PreparedPmemDeviceError {
    AllocateDevices {
        source: TryReserveError,
    },
    OpenBacking {
        pmem_id: String,
        source: PmemFileBackingError,
    },
    MapBacking {
        pmem_id: String,
        source: PmemBackingMappingError,
    },
    AllocateGuestRange {
        pmem_id: String,
        source: PmemGuestRangeAllocationError,
    },
    BackingModeMismatch {
        pmem_id: String,
    },
    UnexpectedBacking,
}

impl fmt::Display for PreparedPmemDeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocateDevices { source } => {
                write!(f, "failed to allocate prepared pmem devices: {source}")
            }
            Self::OpenBacking { pmem_id, source } => {
                write!(f, "failed to prepare pmem device {pmem_id}: {source}")
            }
            Self::MapBacking { pmem_id, source } => {
                write!(f, "failed to map pmem device {pmem_id}: {source}")
            }
            Self::AllocateGuestRange { pmem_id, source } => {
                write!(
                    f,
                    "failed to allocate guest range for pmem device {pmem_id}: {source}"
                )
            }
            Self::BackingModeMismatch { pmem_id } => {
                write!(
                    f,
                    "provided pmem backing mode does not match device {pmem_id}"
                )
            }
            Self::UnexpectedBacking => {
                f.write_str("provided pmem backing does not match a configured device")
            }
        }
    }
}

impl std::error::Error for PreparedPmemDeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateDevices { source } => Some(source),
            Self::OpenBacking { source, .. } => Some(source),
            Self::MapBacking { source, .. } => Some(source),
            Self::AllocateGuestRange { source, .. } => Some(source),
            Self::BackingModeMismatch { .. } | Self::UnexpectedBacking => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmemMmioLayout {
    base_address: GuestAddress,
    base_region_id: MmioRegionId,
    address_stride: u64,
    region_id_stride: u64,
}

impl PmemMmioLayout {
    pub const fn new(base_address: GuestAddress, base_region_id: MmioRegionId) -> Self {
        Self {
            base_address,
            base_region_id,
            address_stride: VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            region_id_stride: 1,
        }
    }

    pub const fn base_address(self) -> GuestAddress {
        self.base_address
    }

    pub const fn base_region_id(self) -> MmioRegionId {
        self.base_region_id
    }

    pub const fn address_stride(self) -> u64 {
        self.address_stride
    }

    pub const fn region_id_stride(self) -> u64 {
        self.region_id_stride
    }

    pub const fn with_address_stride(mut self, address_stride: u64) -> Self {
        self.address_stride = address_stride;
        self
    }

    pub const fn with_region_id_stride(mut self, region_id_stride: u64) -> Self {
        self.region_id_stride = region_id_stride;
        self
    }

    fn validate(self) -> Result<(), PmemMmioRegistrationError> {
        if self.address_stride < VIRTIO_MMIO_DEVICE_WINDOW_SIZE {
            return Err(PmemMmioRegistrationError::AddressStrideTooSmall {
                stride: self.address_stride,
                minimum: VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            });
        }

        if self.region_id_stride == 0 {
            return Err(PmemMmioRegistrationError::DuplicateRegionIdStride {
                region_id: self.base_region_id,
            });
        }

        Ok(())
    }

    fn placement(self, index: usize) -> Result<PmemMmioDevicePlacement, PmemMmioRegistrationError> {
        let device_index = u64::try_from(index)
            .map_err(|_| PmemMmioRegistrationError::DeviceIndexTooLarge { index })?;
        let address_offset = device_index.checked_mul(self.address_stride).ok_or(
            PmemMmioRegistrationError::AddressOffsetOverflow {
                device_index,
                stride: self.address_stride,
            },
        )?;
        let address = self.base_address.checked_add(address_offset).ok_or(
            PmemMmioRegistrationError::AddressOverflow {
                base_address: self.base_address,
                offset: address_offset,
            },
        )?;
        let region_id_offset = device_index.checked_mul(self.region_id_stride).ok_or(
            PmemMmioRegistrationError::RegionIdOffsetOverflow {
                device_index,
                stride: self.region_id_stride,
            },
        )?;
        let region_id = self
            .base_region_id
            .raw_value()
            .checked_add(region_id_offset)
            .map(MmioRegionId::new)
            .ok_or(PmemMmioRegistrationError::RegionIdOverflow {
                base_region_id: self.base_region_id,
                offset: region_id_offset,
            })?;
        let region = MmioRegion::new(region_id, address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE).map_err(
            |source| PmemMmioRegistrationError::InvalidRegion {
                region_id,
                address,
                source,
            },
        )?;

        Ok(PmemMmioDevicePlacement {
            index,
            address,
            region_id,
            region,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PmemMmioDevicePlacement {
    index: usize,
    address: GuestAddress,
    region_id: MmioRegionId,
    region: MmioRegion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemMmioDeviceRegistration {
    index: usize,
    pmem_id: String,
    region: MmioRegion,
    guest_range: GuestMemoryRange,
    file_len: u64,
    config_space: VirtioPmemConfigSpace,
}

impl PmemMmioDeviceRegistration {
    pub const fn index(&self) -> usize {
        self.index
    }

    pub fn pmem_id(&self) -> &str {
        &self.pmem_id
    }

    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    pub const fn guest_range(&self) -> GuestMemoryRange {
        self.guest_range
    }

    pub const fn file_len(&self) -> u64 {
        self.file_len
    }

    pub const fn region_id(&self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn address(&self) -> GuestAddress {
        self.region.range().start()
    }

    pub const fn config_space(&self) -> VirtioPmemConfigSpace {
        self.config_space
    }
}

#[derive(Debug)]
pub struct PmemMmioDevices {
    dispatcher: MmioDispatcher,
    registrations: Vec<PmemMmioDeviceRegistration>,
    pmem_devices: Vec<PreparedPmemDevice>,
}

impl PmemMmioDevices {
    pub fn from_prepared(
        prepared: PreparedPmemDevices,
        layout: PmemMmioLayout,
    ) -> Result<Self, PmemMmioRegistrationError> {
        Self::from_prepared_with_dispatcher(prepared, layout, MmioDispatcher::new())
    }

    pub fn from_prepared_with_dispatcher(
        prepared: PreparedPmemDevices,
        layout: PmemMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<Self, PmemMmioRegistrationError> {
        layout.validate()?;

        let mut registrations = Vec::new();
        registrations
            .try_reserve_exact(prepared.len())
            .map_err(|source| PmemMmioRegistrationError::AllocateRegistrations { source })?;
        let mut placements = Vec::new();
        placements
            .try_reserve_exact(prepared.len())
            .map_err(|source| PmemMmioRegistrationError::AllocatePlacements { source })?;
        for index in 0..prepared.len() {
            placements.push(layout.placement(index)?);
        }

        let mut dispatcher = dispatcher;
        for (prepared_device, placement) in prepared.as_slice().iter().zip(placements) {
            let pmem_id = prepared_device.id().to_string();
            let guest_range = prepared_device.guest_range();
            let file_len = prepared_device.mapping().file_len();
            let config_space = prepared_device.config_space();
            let rate_limiter = prepared_device.rate_limiter();
            let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
                VIRTIO_PMEM_DEVICE_ID,
                config_space.available_features(),
                &VIRTIO_PMEM_QUEUE_SIZES,
                config_space,
                VirtioPmemDevice::with_rate_limiter(file_len, rate_limiter),
            )
            .map_err(|source| PmemMmioRegistrationError::BuildHandler {
                pmem_id: pmem_id.clone(),
                region_id: placement.region_id,
                source,
            })?;
            let region = dispatcher
                .insert_region(
                    placement.region_id,
                    placement.address,
                    VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
                )
                .map_err(|source| PmemMmioRegistrationError::InsertRegion {
                    pmem_id: pmem_id.clone(),
                    region_id: placement.region_id,
                    address: placement.address,
                    source,
                })?;
            dispatcher
                .register_handler(placement.region_id, handler)
                .map_err(|source| PmemMmioRegistrationError::RegisterHandler {
                    pmem_id: pmem_id.clone(),
                    region_id: placement.region_id,
                    source,
                })?;
            debug_assert_eq!(region, placement.region);
            registrations.push(PmemMmioDeviceRegistration {
                index: placement.index,
                pmem_id,
                region,
                guest_range,
                file_len,
                config_space,
            });
        }

        Ok(Self {
            dispatcher,
            registrations,
            pmem_devices: prepared.into_vec(),
        })
    }

    pub fn dispatcher(&self) -> &MmioDispatcher {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut MmioDispatcher {
        &mut self.dispatcher
    }

    pub fn registrations(&self) -> &[PmemMmioDeviceRegistration] {
        &self.registrations
    }

    pub fn pmem_devices(&self) -> &[PreparedPmemDevice] {
        &self.pmem_devices
    }

    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    pub fn into_parts(
        self,
    ) -> (
        MmioDispatcher,
        Vec<PmemMmioDeviceRegistration>,
        Vec<PreparedPmemDevice>,
    ) {
        (self.dispatcher, self.registrations, self.pmem_devices)
    }
}

#[derive(Debug)]
pub enum PmemMmioRegistrationError {
    AddressStrideTooSmall {
        stride: u64,
        minimum: u64,
    },
    DuplicateRegionIdStride {
        region_id: MmioRegionId,
    },
    DeviceIndexTooLarge {
        index: usize,
    },
    AddressOffsetOverflow {
        device_index: u64,
        stride: u64,
    },
    AddressOverflow {
        base_address: GuestAddress,
        offset: u64,
    },
    RegionIdOffsetOverflow {
        device_index: u64,
        stride: u64,
    },
    RegionIdOverflow {
        base_region_id: MmioRegionId,
        offset: u64,
    },
    AllocateRegistrations {
        source: TryReserveError,
    },
    AllocatePlacements {
        source: TryReserveError,
    },
    InvalidRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: GuestMemoryError,
    },
    BuildHandler {
        pmem_id: String,
        region_id: MmioRegionId,
        source: VirtioMmioRegisterHandlerError,
    },
    InsertRegion {
        pmem_id: String,
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        pmem_id: String,
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for PmemMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AddressStrideTooSmall { stride, minimum } => {
                write!(
                    f,
                    "pmem MMIO address stride {stride} is smaller than the required device window size {minimum}"
                )
            }
            Self::DuplicateRegionIdStride { region_id } => {
                write!(
                    f,
                    "pmem MMIO region id stride cannot be 0 because it would duplicate region id={region_id}"
                )
            }
            Self::DeviceIndexTooLarge { index } => {
                write!(f, "pmem MMIO device index {index} does not fit in u64")
            }
            Self::AddressOffsetOverflow {
                device_index,
                stride,
            } => {
                write!(
                    f,
                    "pmem MMIO address offset overflows for device index {device_index} with stride {stride}"
                )
            }
            Self::AddressOverflow {
                base_address,
                offset,
            } => {
                write!(
                    f,
                    "pmem MMIO address overflows from base {base_address} with offset {offset}"
                )
            }
            Self::RegionIdOffsetOverflow {
                device_index,
                stride,
            } => {
                write!(
                    f,
                    "pmem MMIO region id offset overflows for device index {device_index} with stride {stride}"
                )
            }
            Self::RegionIdOverflow {
                base_region_id,
                offset,
            } => {
                write!(
                    f,
                    "pmem MMIO region id overflows from base id={base_region_id} with offset {offset}"
                )
            }
            Self::AllocateRegistrations { source } => {
                write!(f, "failed to allocate pmem MMIO registrations: {source}")
            }
            Self::AllocatePlacements { source } => {
                write!(f, "failed to allocate pmem MMIO placements: {source}")
            }
            Self::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "invalid pmem MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::BuildHandler {
                pmem_id,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to build pmem MMIO handler for pmem device {pmem_id} region id={region_id}: {source}"
                )
            }
            Self::InsertRegion {
                pmem_id,
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "failed to insert pmem MMIO region for pmem device {pmem_id} region id={region_id} at {address}: {source}"
                )
            }
            Self::RegisterHandler {
                pmem_id,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to register pmem MMIO handler for pmem device {pmem_id} region id={region_id}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for PmemMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateRegistrations { source } => Some(source),
            Self::AllocatePlacements { source } => Some(source),
            Self::InvalidRegion { source, .. } => Some(source),
            Self::BuildHandler { source, .. } => Some(source),
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
            Self::AddressStrideTooSmall { .. }
            | Self::DuplicateRegionIdStride { .. }
            | Self::DeviceIndexTooLarge { .. }
            | Self::AddressOffsetOverflow { .. }
            | Self::AddressOverflow { .. }
            | Self::RegionIdOffsetOverflow { .. }
            | Self::RegionIdOverflow { .. } => None,
        }
    }
}

#[derive(Debug)]
struct PmemGuestRangeAllocator<'a> {
    next_start: u64,
    reserved_ranges: &'a [GuestMemoryRange],
}

impl<'a> PmemGuestRangeAllocator<'a> {
    fn for_layout(layout: &'a GuestMemoryLayout) -> Self {
        Self {
            next_start: aarch64::FIRST_ADDR_PAST_64BITS_MMIO,
            reserved_ranges: layout.ranges(),
        }
    }

    #[cfg(test)]
    fn without_reserved_ranges() -> Self {
        Self {
            next_start: aarch64::FIRST_ADDR_PAST_64BITS_MMIO,
            reserved_ranges: &[],
        }
    }

    #[cfg(test)]
    fn with_start_for_test(next_start: u64, reserved_ranges: &'a [GuestMemoryRange]) -> Self {
        Self {
            next_start,
            reserved_ranges,
        }
    }

    fn allocate(&mut self, size: u64) -> Result<GuestMemoryRange, PmemGuestRangeAllocationError> {
        if !size.is_multiple_of(VIRTIO_PMEM_ALIGNMENT) {
            return Err(PmemGuestRangeAllocationError::UnalignedSize {
                size,
                alignment: VIRTIO_PMEM_ALIGNMENT,
            });
        }

        let mut candidate = align_pmem_guest_address(self.next_start)?;
        loop {
            let range = build_pmem_guest_range(candidate, size)?;
            let Some(overlap) = self
                .reserved_ranges
                .iter()
                .copied()
                .find(|reserved| range.overlaps(*reserved))
            else {
                self.next_start = range.end_exclusive().raw_value();
                return Ok(range);
            };

            candidate = align_pmem_guest_address(overlap.end_exclusive().raw_value())?;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmemGuestRangeAllocationError {
    UnalignedSize {
        size: u64,
        alignment: u64,
    },
    AddressAlignmentOverflow {
        address: u64,
        alignment: u64,
    },
    InvalidRange {
        start: GuestAddress,
        size: u64,
        source: GuestMemoryError,
    },
}

impl fmt::Display for PmemGuestRangeAllocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnalignedSize { size, alignment } => write!(
                f,
                "pmem guest range size {size} is not aligned to {alignment} bytes"
            ),
            Self::AddressAlignmentOverflow { address, alignment } => write!(
                f,
                "pmem guest address 0x{address:x} cannot be aligned to {alignment} bytes without overflow"
            ),
            Self::InvalidRange {
                start,
                size,
                source,
            } => write!(
                f,
                "invalid pmem guest range at {start} with size {size}: {source}"
            ),
        }
    }
}

impl std::error::Error for PmemGuestRangeAllocationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRange { source, .. } => Some(source),
            Self::UnalignedSize { .. } | Self::AddressAlignmentOverflow { .. } => None,
        }
    }
}

fn align_pmem_guest_address(address: u64) -> Result<u64, PmemGuestRangeAllocationError> {
    let remainder = address % VIRTIO_PMEM_ALIGNMENT;
    if remainder == 0 {
        return Ok(address);
    }

    let padding = VIRTIO_PMEM_ALIGNMENT - remainder;
    address
        .checked_add(padding)
        .ok_or(PmemGuestRangeAllocationError::AddressAlignmentOverflow {
            address,
            alignment: VIRTIO_PMEM_ALIGNMENT,
        })
}

fn build_pmem_guest_range(
    start: u64,
    size: u64,
) -> Result<GuestMemoryRange, PmemGuestRangeAllocationError> {
    let start = GuestAddress::new(start);
    GuestMemoryRange::new(start, size).map_err(|source| {
        PmemGuestRangeAllocationError::InvalidRange {
            start,
            size,
            source,
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmemConfigError {
    EmptyPmemId,
    InvalidPmemId,
    EmptyPathOnHost,
    UnsupportedRootDevice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmemIdSource {
    Path,
    Body,
}

impl fmt::Display for PmemIdSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path => f.write_str("path pmem id"),
            Self::Body => f.write_str("body id"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PmemUpdateError {
    EmptyPmemId {
        source: PmemIdSource,
    },
    InvalidPmemId {
        source: PmemIdSource,
    },
    MismatchedPmemId,
    UnknownPmem,
    HandlerLookup {
        pmem_id: String,
        region_id: MmioRegionId,
        message: String,
    },
    ActiveSessionCommand {
        message: String,
    },
    ActiveSessionUnavailable,
    MmioDispatcherUnavailable,
}

impl fmt::Display for PmemConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPmemId => f.write_str("pmem id must not be empty"),
            Self::InvalidPmemId => {
                f.write_str("pmem id must contain only alphanumeric characters or '_'")
            }
            Self::EmptyPathOnHost => f.write_str("pmem path_on_host must not be empty"),
            Self::UnsupportedRootDevice => f.write_str("pmem root_device is not supported"),
        }
    }
}

impl std::error::Error for PmemConfigError {}

impl fmt::Display for PmemUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPmemId { source } => write!(f, "{source} must not be empty"),
            Self::InvalidPmemId { source, .. } => {
                write!(
                    f,
                    "{source} must contain only alphanumeric characters or '_'"
                )
            }
            Self::MismatchedPmemId => f.write_str("path pmem id must match body id"),
            Self::UnknownPmem => f.write_str("pmem device is not configured"),
            Self::HandlerLookup {
                region_id, message, ..
            } => write!(
                f,
                "failed to resolve pmem device handler for region id={region_id}: {message}"
            ),
            Self::ActiveSessionCommand { message } => {
                write!(
                    f,
                    "failed to deliver pmem update to active session: {message}"
                )
            }
            Self::ActiveSessionUnavailable => f.write_str("active pmem session is unavailable"),
            Self::MmioDispatcherUnavailable => f.write_str("pmem MMIO dispatcher is unavailable"),
        }
    }
}

impl std::error::Error for PmemUpdateError {}

fn validate_pmem_id(id: &str) -> Result<(), PmemConfigError> {
    if id.is_empty() {
        return Err(PmemConfigError::EmptyPmemId);
    }

    if !id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(PmemConfigError::InvalidPmemId);
    }

    Ok(())
}

fn validate_pmem_update_id(source: PmemIdSource, id: &str) -> Result<(), PmemUpdateError> {
    if id.is_empty() {
        return Err(PmemUpdateError::EmptyPmemId { source });
    }

    if !id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(PmemUpdateError::InvalidPmemId { source });
    }

    Ok(())
}

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

fn read_virtio_pmem_config_bytes(
    bytes: &[u8; VIRTIO_PMEM_CONFIG_SPACE_SIZE],
    access: VirtioMmioDeviceConfigAccess,
) -> Result<&[u8], VirtioMmioDeviceConfigError> {
    let offset = usize::try_from(access.offset()).map_err(|_| {
        VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        }
    })?;
    let Some(end) = offset.checked_add(access.len()) else {
        return Err(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        });
    };

    bytes
        .get(offset..end)
        .ok_or(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        })
}

fn config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: MmioHandlerError::new(format!("virtio-pmem config access bytes failed: {source}")),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeMap;
    use std::error::Error as _;
    use std::ffi::CString;
    use std::fs;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    use crate::memory::{GuestAddress, GuestMemoryLayout, GuestMemoryRange, aarch64};
    use crate::metrics::{
        PmemDeviceMetrics, PmemDeviceMetricsByDevice, SharedPmemDeviceMetrics,
        SharedPmemDeviceMetricsRegistry,
    };
    use crate::mmio::{MmioAccess, MmioAccessBytes, MmioBus, MmioOperation, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_MAGIC_VALUE,
        VirtioMmioAccess, VirtioMmioDeviceConfigError, VirtioMmioRegister,
        decode_virtio_mmio_access,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
    };

    const TEST_PMEM_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
    const TEST_PMEM_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(9000);
    const TEST_PMEM_START: u64 = aarch64::FIRST_ADDR_PAST_64BITS_MMIO;
    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x1000);
    const TEST_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x5000);
    const TEST_USED_RING: GuestAddress = GuestAddress::new(0x6000);
    const TEST_QUEUE_SIZE: u16 = 8;
    const TEST_MEMORY_SIZE: u64 = 0x10_000;
    const TEST_AVAILABLE_RING_IDX_OFFSET: u64 = 2;
    const TEST_AVAILABLE_RING_RING_OFFSET: u64 = 4;
    const TEST_AVAILABLE_RING_ENTRY_SIZE: u64 = 2;
    const TEST_USED_RING_IDX_OFFSET: u64 = 2;
    const TEST_USED_RING_RING_OFFSET: u64 = 4;
    const TEST_USED_RING_ELEMENT_SIZE: u64 = 8;
    const TEST_PMEM_REQUEST_ADDR: GuestAddress = GuestAddress::new(0x2000);
    const TEST_PMEM_STATUS_ADDR: GuestAddress = GuestAddress::new(0x3000);
    static NEXT_TEMP_PATH_ID: AtomicU64 = AtomicU64::new(0);

    #[derive(Debug)]
    struct TempPath {
        path: PathBuf,
    }

    impl TempPath {
        fn as_path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            match fs::symlink_metadata(&self.path) {
                Ok(metadata) if metadata.is_dir() => {
                    let _ = fs::remove_dir_all(&self.path);
                }
                Ok(_) => {
                    let _ = fs::remove_file(&self.path);
                }
                Err(_) => {}
            }
        }
    }

    fn pmem_config(input: PmemConfigInput) -> PmemConfig {
        input.try_into().expect("pmem input should validate")
    }

    fn temp_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_PATH_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-pmem-test-{}-{id}-{name}",
            std::process::id(),
        ))
    }

    fn temp_file(name: &str, bytes: &[u8]) -> TempPath {
        let temp = TempPath {
            path: temp_path(name),
        };
        fs::write(temp.as_path(), bytes).expect("test file should be written");
        temp
    }

    fn temp_sized_file(name: &str, len: u64) -> TempPath {
        let temp = TempPath {
            path: temp_path(name),
        };
        let file = fs::File::create(temp.as_path()).expect("test file should be created");
        file.set_len(len).expect("test file size should be set");
        temp
    }

    fn temp_dir(name: &str) -> TempPath {
        let temp = TempPath {
            path: temp_path(name),
        };
        fs::create_dir(temp.as_path()).expect("test directory should be created");
        temp
    }

    fn temp_fifo(name: &str) -> TempPath {
        let temp = TempPath {
            path: temp_path(name),
        };
        let c_path = CString::new(temp.as_path().as_os_str().as_bytes())
            .expect("test FIFO path should not contain NUL");

        // SAFETY: `c_path` is a NUL-terminated path built from the test path
        // and lives for the duration of the call.
        let result = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        if result != 0 {
            panic!(
                "test FIFO should be created: {}",
                io::Error::last_os_error()
            );
        }

        temp
    }

    fn temp_socket(name: &str) -> (TempPath, UnixListener) {
        let temp = TempPath {
            path: short_temp_path(name),
        };
        let listener =
            UnixListener::bind(temp.as_path()).expect("test Unix socket should be created");
        (temp, listener)
    }

    fn short_temp_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_PATH_ID.fetch_add(1, Ordering::Relaxed);
        let base = Path::new("/tmp");
        let dir = if base.is_dir() {
            base.to_path_buf()
        } else {
            std::env::temp_dir()
        };
        dir.join(format!("bb-pmem-{}-{id}-{name}", std::process::id()))
    }

    fn missing_path(name: &str) -> PathBuf {
        temp_path(name)
    }

    fn config_for_path(path: &Path, read_only: bool) -> PmemConfig {
        pmem_config(
            PmemConfigInput::new("pmem0", path.to_string_lossy().into_owned())
                .with_read_only(read_only),
        )
    }

    fn open_backing(path: &Path, read_only: bool) -> Result<PmemFileBacking, PmemFileBackingError> {
        PmemFileBacking::open(&config_for_path(path, read_only))
    }

    fn map_backing(path: &Path, read_only: bool) -> PmemBackingMapping {
        let backing = open_backing(path, read_only).expect("pmem backing should open");
        PmemBackingMapping::map(&backing).expect("pmem backing should map")
    }

    fn guest_range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size)
            .expect("test guest range should be valid")
    }

    fn guest_layout(ranges: Vec<GuestMemoryRange>) -> GuestMemoryLayout {
        GuestMemoryLayout::new(ranges).expect("test guest layout should be valid")
    }

    #[derive(Debug)]
    struct ScriptedPmemBackingMapper {
        drop_count: Arc<AtomicUsize>,
        calls: usize,
        fail_on_call: usize,
    }

    impl ScriptedPmemBackingMapper {
        fn new(drop_count: Arc<AtomicUsize>, fail_on_call: usize) -> Self {
            Self {
                drop_count,
                calls: 0,
                fail_on_call,
            }
        }
    }

    impl PmemBackingMapper for ScriptedPmemBackingMapper {
        fn map(
            &mut self,
            backing: &PmemFileBacking,
        ) -> Result<PmemBackingMapping, PmemBackingMappingError> {
            self.calls += 1;
            if self.calls == self.fail_on_call {
                return Err(PmemBackingMappingError::MapFile {
                    len: usize::try_from(backing.len())
                        .expect("test backing length should fit usize"),
                    source: io::Error::other("scripted pmem map failure"),
                });
            }

            Ok(PmemBackingMapping::test_mapping(
                backing.len(),
                align_pmem_mapping_len(backing.len()).expect("test mapping length should align"),
                backing.is_read_only(),
                Arc::clone(&self.drop_count),
            ))
        }
    }

    fn device_config_read_access(offset: u64, len: u64) -> VirtioMmioDeviceConfigAccess {
        let operation =
            MmioOperation::read(mmio_access(offset, len)).expect("read operation should build");
        decode_device_config_access(operation)
    }

    fn device_config_write_access(
        offset: u64,
        data: MmioAccessBytes,
    ) -> VirtioMmioDeviceConfigAccess {
        let len = u64::try_from(data.len()).expect("test write length should fit u64");
        let operation = MmioOperation::write(mmio_access(offset, len), data)
            .expect("write operation should build");
        decode_device_config_access(operation)
    }

    fn mmio_access(offset: u64, len: u64) -> MmioAccess {
        let mut bus = MmioBus::new();
        bus.insert(
            TEST_PMEM_MMIO_REGION_ID,
            TEST_PMEM_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("test MMIO region should insert");
        let start = TEST_PMEM_MMIO_BASE
            .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset)
            .expect("test MMIO address should not overflow");
        bus.lookup(start, len)
            .expect("test MMIO access should look up")
    }

    fn decode_device_config_access(operation: MmioOperation) -> VirtioMmioDeviceConfigAccess {
        match decode_virtio_mmio_access(&operation).expect("access should decode") {
            VirtioMmioAccess::DeviceConfig(access) => access,
            _ => panic!("test access should target device config"),
        }
    }

    fn read_pmem_config(
        config: &VirtioPmemConfigSpace,
        offset: u64,
        len: u64,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        config.read_device_config(device_config_read_access(offset, len))
    }

    fn write_pmem_config(
        config: &mut VirtioPmemConfigSpace,
        offset: u64,
        data: &[u8],
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        let data = MmioAccessBytes::new(data).expect("write bytes should be valid");
        config.write_device_config(device_config_write_access(offset, data), data)
    }

    fn dispatch_pmem_mmio_read(
        devices: &mut PmemMmioDevices,
        index: usize,
        offset: u64,
        len: u64,
    ) -> MmioAccessBytes {
        let registration = devices.registrations()[index].clone();
        let address = registration
            .address()
            .checked_add(offset)
            .expect("test MMIO address should not overflow");
        let access = devices
            .dispatcher()
            .lookup(address, len)
            .expect("pmem MMIO access should resolve");
        devices
            .dispatcher_mut()
            .handler_mut::<VirtioPmemMmioHandler>(registration.region_id())
            .expect("pmem MMIO handler should be registered")
            .read_access(access)
            .expect("pmem MMIO read should dispatch")
    }

    fn dispatch_pmem_mmio_read_u32(
        devices: &mut PmemMmioDevices,
        index: usize,
        offset: u64,
    ) -> u32 {
        let data = dispatch_pmem_mmio_read(devices, index, offset, 4);
        u32::from_le_bytes(
            data.as_slice()
                .try_into()
                .expect("u32 MMIO read should return 4 bytes"),
        )
    }

    #[derive(Debug, Clone, Copy)]
    struct TestDescriptor {
        address: GuestAddress,
        len: u32,
        flags: u16,
        next: u16,
    }

    impl TestDescriptor {
        const fn readable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_NEXT, index),
                None => (0, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }

        const fn writable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_WRITE | VIRTQUEUE_DESC_F_NEXT, index),
                None => (VIRTQUEUE_DESC_F_WRITE, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }
    }

    fn request_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test range should be valid"),
        ])
        .expect("test layout should be valid");
        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    fn write_request_type(memory: &mut GuestMemory, request_type: u32) {
        write_request_type_at(memory, TEST_PMEM_REQUEST_ADDR, request_type);
    }

    fn write_request_type_at(memory: &mut GuestMemory, address: GuestAddress, request_type: u32) {
        memory
            .write_slice(&request_type.to_le_bytes(), address)
            .expect("pmem request type should write");
    }

    fn write_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size fits u64"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("descriptor should write");
    }

    fn write_guest_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 field should write");
    }

    fn read_guest_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("u16 field should read");
        u16::from_le_bytes(bytes)
    }

    fn read_guest_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("u32 field should read");
        u32::from_le_bytes(bytes)
    }

    fn read_guest_i32(memory: &GuestMemory, address: GuestAddress) -> i32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("i32 field should read");
        i32::from_le_bytes(bytes)
    }

    fn available_ring_idx_address() -> GuestAddress {
        TEST_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("available idx address should not overflow")
    }

    fn available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("available entry address should not overflow")
    }

    fn used_ring_idx_address() -> GuestAddress {
        TEST_USED_RING
            .checked_add(TEST_USED_RING_IDX_OFFSET)
            .expect("used idx address should not overflow")
    }

    fn used_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_USED_RING
            .checked_add(
                TEST_USED_RING_RING_OFFSET + u64::from(ring_index) * TEST_USED_RING_ELEMENT_SIZE,
            )
            .expect("used entry address should not overflow")
    }

    fn write_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                available_ring_entry_address(
                    u16::try_from(ring_index).expect("ring index should fit u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            available_ring_idx_address(),
            u16::try_from(heads.len()).expect("head count should fit u16"),
        );
    }

    fn read_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, used_ring_idx_address())
    }

    fn read_used_element(memory: &GuestMemory, ring_index: u16) -> (u32, u32) {
        let entry = used_ring_entry_address(ring_index);
        let len_address = entry
            .checked_add(4)
            .expect("used element len address should not overflow");
        (
            read_guest_u32(memory, entry),
            read_guest_u32(memory, len_address),
        )
    }

    fn pmem_queue() -> VirtioPmemQueue {
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            TEST_AVAILABLE_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let used = VirtqueueUsedRing::new(TEST_USED_RING, TEST_QUEUE_SIZE)
            .expect("used ring should build");
        VirtioPmemQueue::new(available, used)
    }

    fn write_pmem_flush_chain(memory: &mut GuestMemory) {
        write_pmem_flush_chain_at(memory, 0, 1, TEST_PMEM_REQUEST_ADDR, TEST_PMEM_STATUS_ADDR);
        write_available_heads(memory, &[0]);
    }

    fn write_pmem_flush_chain_at(
        memory: &mut GuestMemory,
        request_index: u16,
        status_index: u16,
        request_address: GuestAddress,
        status_address: GuestAddress,
    ) {
        write_request_type_at(memory, request_address, VIRTIO_PMEM_REQUEST_TYPE_FLUSH);
        write_descriptor(
            memory,
            request_index,
            TestDescriptor::readable(
                request_address,
                VIRTIO_PMEM_REQUEST_SIZE,
                Some(status_index),
            ),
        );
        write_descriptor(
            memory,
            status_index,
            TestDescriptor::writable(status_address, VIRTIO_PMEM_STATUS_SIZE, None),
        );
    }

    #[test]
    fn virtio_pmem_constants_match_firecracker_shape() {
        assert_eq!(VIRTIO_PMEM_DEVICE_ID, 27);
        assert_eq!(VIRTIO_PMEM_QUEUE_COUNT, 1);
        assert_eq!(VIRTIO_PMEM_QUEUE_SIZE, 256);
        assert_eq!(VIRTIO_PMEM_QUEUE_SIZES, [VIRTIO_PMEM_QUEUE_SIZE]);
        assert_eq!(VIRTIO_PMEM_CONFIG_SPACE_SIZE, 16);
        assert_eq!(VIRTIO_PMEM_ALIGNMENT, 2 * 1024 * 1024);
    }

    #[test]
    fn virtio_pmem_config_space_tracks_start_and_size() {
        let config = VirtioPmemConfigSpace::new(0x1000_0000, 0x0200_0000);

        assert_eq!(config.start(), 0x1000_0000);
        assert_eq!(config.size(), 0x0200_0000);
    }

    #[test]
    fn virtio_pmem_config_space_uses_firecracker_little_endian_layout() {
        let config = VirtioPmemConfigSpace::new(0x0102_0304_0506_0708, 0x1112_1314_1516_1718);
        let bytes = [
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13,
            0x12, 0x11,
        ];

        assert_eq!(config.to_le_bytes(), bytes);
        assert_eq!(VirtioPmemConfigSpace::from_le_bytes(bytes), config);
    }

    #[test]
    fn virtio_pmem_config_space_preserves_u64_boundaries() {
        let config = VirtioPmemConfigSpace::new(u64::MAX, u64::MAX);

        assert_eq!(config.to_le_bytes(), [0xff; VIRTIO_PMEM_CONFIG_SPACE_SIZE]);
        assert_eq!(
            VirtioPmemConfigSpace::from_le_bytes([0xff; VIRTIO_PMEM_CONFIG_SPACE_SIZE]),
            config
        );
    }

    #[test]
    fn virtio_pmem_config_space_advertises_modern_virtio_feature() {
        let config = VirtioPmemConfigSpace::new(0, 0);

        assert_eq!(
            config.available_features(),
            1_u64 << VIRTIO_FEATURE_VERSION_1
        );
    }

    #[test]
    fn virtio_pmem_config_space_reads_within_layout() {
        let config = VirtioPmemConfigSpace::new(0x0102_0304_0506_0708, 0x1112_1314_1516_1718);

        assert_eq!(
            read_pmem_config(&config, 0, 8)
                .expect("start read should succeed")
                .as_slice(),
            &0x0102_0304_0506_0708_u64.to_le_bytes()
        );
        assert_eq!(
            read_pmem_config(&config, 8, 8)
                .expect("size read should succeed")
                .as_slice(),
            &0x1112_1314_1516_1718_u64.to_le_bytes()
        );
        assert_eq!(
            read_pmem_config(&config, 4, 4)
                .expect("partial read should succeed")
                .as_slice(),
            &[0x04, 0x03, 0x02, 0x01]
        );
        assert_eq!(
            read_pmem_config(&config, 15, 1)
                .expect("last byte read should succeed")
                .as_slice(),
            &[0x11]
        );
    }

    #[test]
    fn virtio_pmem_config_space_rejects_out_of_bounds_reads() {
        let config = VirtioPmemConfigSpace::new(0, 0);

        assert_eq!(
            read_pmem_config(&config, 16, 1),
            Err(VirtioMmioDeviceConfigError::UnsupportedRead { offset: 16, len: 1 })
        );
        assert_eq!(
            read_pmem_config(&config, 15, 2),
            Err(VirtioMmioDeviceConfigError::UnsupportedRead { offset: 15, len: 2 })
        );
    }

    #[test]
    fn virtio_pmem_config_space_rejects_guest_writes() {
        let mut config = VirtioPmemConfigSpace::new(0, 0);

        assert_eq!(
            write_pmem_config(&mut config, 0, &[1, 2, 3, 4]),
            Err(VirtioMmioDeviceConfigError::UnsupportedWrite { offset: 0, len: 4 })
        );
        assert_eq!(config, VirtioPmemConfigSpace::new(0, 0));
    }

    #[test]
    fn pmem_queue_dispatch_completes_successful_flush() {
        let mut memory = request_memory();
        write_pmem_flush_chain(&mut memory);
        let mut queue = pmem_queue();
        let mut flush_calls = 0;
        let mut flush = || {
            flush_calls += 1;
            VirtioPmemFlushStatus::Success
        };

        let dispatch = queue
            .dispatch(&mut memory, &mut flush)
            .expect("pmem flush queue should dispatch");

        assert_eq!(flush_calls, 1);
        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_flushes(), 1);
        assert_eq!(dispatch.failed_flushes(), 0);
        assert_eq!(dispatch.parse_failures(), 0);
        assert_eq!(dispatch.status_write_failures(), 0);
        assert!(dispatch.needs_queue_interrupt());
        let metrics = SharedPmemDeviceMetrics::default();
        metrics.record_queue_dispatch(&dispatch);
        assert_eq!(metrics.snapshot(), PmemDeviceMetrics::default());
        assert_eq!(
            read_guest_i32(&memory, TEST_PMEM_STATUS_ADDR),
            VIRTIO_PMEM_STATUS_SUCCESS
        );
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_PMEM_STATUS_SIZE));
    }

    #[test]
    fn pmem_queue_dispatch_reports_failed_flush_to_guest() {
        let mut memory = request_memory();
        write_pmem_flush_chain(&mut memory);
        let mut queue = pmem_queue();
        let mut flush_calls = 0;
        let mut flush = || {
            flush_calls += 1;
            VirtioPmemFlushStatus::Failure
        };

        let dispatch = queue
            .dispatch(&mut memory, &mut flush)
            .expect("pmem flush queue should dispatch");

        assert_eq!(flush_calls, 1);
        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_flushes(), 0);
        assert_eq!(dispatch.failed_flushes(), 1);
        assert_eq!(dispatch.parse_failures(), 0);
        assert_eq!(dispatch.status_write_failures(), 0);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            read_guest_i32(&memory, TEST_PMEM_STATUS_ADDR),
            VIRTIO_PMEM_STATUS_FAILURE
        );
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_PMEM_STATUS_SIZE));
    }

    #[test]
    fn pmem_queue_dispatch_skips_flush_for_empty_queue() {
        let mut memory = request_memory();
        let mut queue = pmem_queue();
        let flush_calls = Cell::new(0);
        let mut flush = || {
            flush_calls.set(flush_calls.get() + 1);
            VirtioPmemFlushStatus::Success
        };

        let dispatch = queue
            .dispatch(&mut memory, &mut flush)
            .expect("empty pmem queue should dispatch");

        assert_eq!(flush_calls.get(), 0);
        assert_eq!(dispatch.processed_requests(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(read_used_index(&memory), 0);
    }

    #[test]
    fn pmem_queue_dispatch_flushes_once_after_first_valid_chain() {
        let mut memory = request_memory();
        write_request_type(&mut memory, VIRTIO_PMEM_REQUEST_TYPE_FLUSH);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(TEST_PMEM_REQUEST_ADDR, VIRTIO_PMEM_REQUEST_SIZE, Some(1)),
        );
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(TEST_PMEM_STATUS_ADDR, VIRTIO_PMEM_STATUS_SIZE, None),
        );
        let first_request = GuestAddress::new(0x2010);
        let first_status = GuestAddress::new(0x3010);
        let second_request = GuestAddress::new(0x2020);
        let second_status = GuestAddress::new(0x3020);
        write_pmem_flush_chain_at(&mut memory, 2, 3, first_request, first_status);
        write_pmem_flush_chain_at(&mut memory, 4, 5, second_request, second_status);
        memory
            .write_slice(&99_i32.to_le_bytes(), TEST_PMEM_STATUS_ADDR)
            .expect("invalid-chain status sentinel should write");
        write_available_heads(&mut memory, &[0, 2, 4]);
        let mut queue = pmem_queue();
        let mut flush_calls = 0;
        let mut flush = || {
            flush_calls += 1;
            VirtioPmemFlushStatus::Success
        };

        let dispatch = queue
            .dispatch(&mut memory, &mut flush)
            .expect("mixed pmem chains should dispatch");

        assert_eq!(flush_calls, 1);
        assert_eq!(dispatch.processed_requests(), 3);
        assert_eq!(dispatch.successful_flushes(), 2);
        assert_eq!(dispatch.parse_failures(), 1);
        assert_eq!(read_guest_i32(&memory, TEST_PMEM_STATUS_ADDR), 99);
        assert_eq!(
            read_guest_i32(&memory, first_status),
            VIRTIO_PMEM_STATUS_SUCCESS
        );
        assert_eq!(
            read_guest_i32(&memory, second_status),
            VIRTIO_PMEM_STATUS_SUCCESS
        );
        assert_eq!(read_used_index(&memory), 3);
        assert_eq!(read_used_element(&memory, 0), (0, 0));
        assert_eq!(read_used_element(&memory, 1), (2, VIRTIO_PMEM_STATUS_SIZE));
        assert_eq!(read_used_element(&memory, 2), (4, VIRTIO_PMEM_STATUS_SIZE));
    }

    #[test]
    fn pmem_queue_dispatch_caches_failed_flush_for_all_valid_chains() {
        let mut memory = request_memory();
        let first_request = GuestAddress::new(0x2010);
        let first_status = GuestAddress::new(0x3010);
        let second_request = GuestAddress::new(0x2020);
        let second_status = GuestAddress::new(0x3020);
        write_pmem_flush_chain_at(&mut memory, 0, 1, first_request, first_status);
        write_pmem_flush_chain_at(&mut memory, 2, 3, second_request, second_status);
        write_available_heads(&mut memory, &[0, 2]);
        let mut queue = pmem_queue();
        let mut flush_calls = 0;
        let mut flush = || {
            flush_calls += 1;
            VirtioPmemFlushStatus::Failure
        };

        let dispatch = queue
            .dispatch(&mut memory, &mut flush)
            .expect("valid pmem chains should share failed flush result");

        assert_eq!(flush_calls, 1);
        assert_eq!(dispatch.processed_requests(), 2);
        assert_eq!(dispatch.failed_flushes(), 2);
        assert_eq!(
            read_guest_i32(&memory, first_status),
            VIRTIO_PMEM_STATUS_FAILURE
        );
        assert_eq!(
            read_guest_i32(&memory, second_status),
            VIRTIO_PMEM_STATUS_FAILURE
        );
    }

    #[test]
    fn pmem_queue_dispatch_publishes_zero_length_used_element_for_parse_failure() {
        let mut memory = request_memory();
        write_request_type(&mut memory, VIRTIO_PMEM_REQUEST_TYPE_FLUSH);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(TEST_PMEM_REQUEST_ADDR, VIRTIO_PMEM_REQUEST_SIZE, Some(1)),
        );
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(TEST_PMEM_STATUS_ADDR, VIRTIO_PMEM_STATUS_SIZE, None),
        );
        memory
            .write_slice(&99_i32.to_le_bytes(), TEST_PMEM_STATUS_ADDR)
            .expect("sentinel status should write");
        write_available_heads(&mut memory, &[0]);
        let mut queue = pmem_queue();
        let flush_calls = Cell::new(0);
        let mut flush = || {
            flush_calls.set(flush_calls.get() + 1);
            VirtioPmemFlushStatus::Success
        };

        let dispatch = queue
            .dispatch(&mut memory, &mut flush)
            .expect("invalid pmem request should still publish a completion");

        assert_eq!(flush_calls.get(), 0);
        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_flushes(), 0);
        assert_eq!(dispatch.failed_flushes(), 0);
        assert_eq!(dispatch.parse_failures(), 1);
        assert_eq!(dispatch.status_write_failures(), 0);
        assert!(matches!(
            dispatch.first_parse_failure(),
            Some(VirtioPmemRequestError::RequestDescriptorWriteOnly { index: 0 })
        ));
        assert!(dispatch.needs_queue_interrupt());
        let metrics = SharedPmemDeviceMetrics::default();
        metrics.record_queue_dispatch(&dispatch);
        assert_eq!(
            metrics.snapshot(),
            PmemDeviceMetrics::default().with_event_fails(1)
        );
        assert_eq!(read_guest_i32(&memory, TEST_PMEM_STATUS_ADDR), 99);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn pmem_queue_dispatch_publishes_zero_length_used_element_for_status_write_failure() {
        let mut memory = request_memory();
        write_request_type(&mut memory, VIRTIO_PMEM_REQUEST_TYPE_FLUSH);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PMEM_REQUEST_ADDR, VIRTIO_PMEM_REQUEST_SIZE, Some(1)),
        );
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(
                GuestAddress::new(TEST_MEMORY_SIZE),
                VIRTIO_PMEM_STATUS_SIZE,
                None,
            ),
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = pmem_queue();
        let mut flush_calls = 0;
        let mut flush = || {
            flush_calls += 1;
            VirtioPmemFlushStatus::Success
        };

        let dispatch = queue
            .dispatch(&mut memory, &mut flush)
            .expect("status write failure should still publish a completion");

        assert_eq!(flush_calls, 1);
        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_flushes(), 0);
        assert_eq!(dispatch.failed_flushes(), 0);
        assert_eq!(dispatch.parse_failures(), 0);
        assert_eq!(dispatch.status_write_failures(), 1);
        assert!(dispatch.needs_queue_interrupt());
        let metrics = SharedPmemDeviceMetrics::default();
        metrics.record_queue_dispatch(&dispatch);
        assert_eq!(
            metrics.snapshot(),
            PmemDeviceMetrics::default().with_event_fails(1)
        );
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn pmem_queue_dispatch_does_not_repeat_flush_after_used_ring_failure() {
        let mut memory = request_memory();
        write_pmem_flush_chain(&mut memory);
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            TEST_AVAILABLE_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let used = VirtqueueUsedRing::new(GuestAddress::new(TEST_MEMORY_SIZE), TEST_QUEUE_SIZE)
            .expect("used ring should build");
        let mut queue = VirtioPmemQueue::new(available, used);
        let mut flush_calls = 0;
        let mut flush = || {
            flush_calls += 1;
            VirtioPmemFlushStatus::Success
        };

        let error = queue
            .dispatch(&mut memory, &mut flush)
            .expect_err("unmapped used ring should fail after one targeted flush");

        assert_eq!(flush_calls, 1);
        assert!(matches!(
            error,
            VirtioPmemQueueDispatchError::UsedRing { .. }
        ));
        assert_eq!(error.completed_dispatch().processed_requests(), 0);
        assert_eq!(
            read_guest_i32(&memory, TEST_PMEM_STATUS_ADDR),
            VIRTIO_PMEM_STATUS_SUCCESS
        );
    }

    #[test]
    fn pmem_device_rate_limiter_charges_exact_file_len_and_rolls_back_ops_on_byte_throttle() {
        let now = Instant::now();
        let mut memory = request_memory();
        write_pmem_flush_chain(&mut memory);
        let rate_limiter = PmemRateLimiterConfig::new(
            Some(PmemTokenBucketConfig::new(100, None, 100)),
            Some(PmemTokenBucketConfig::new(2, None, 100)),
        );
        let mut device = VirtioPmemDevice::with_rate_limiter_at(60, Some(rate_limiter), now);
        device.active_queue = Some(pmem_queue());
        let flush_calls = Cell::new(0);
        let mut flush = || {
            flush_calls.set(flush_calls.get() + 1);
            VirtioPmemFlushStatus::Success
        };

        let first = device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("first pmem event should consume one op and the exact file length");
        write_pmem_flush_chain_at(
            &mut memory,
            2,
            3,
            GuestAddress::new(0x2020),
            GuestAddress::new(0x3020),
        );
        write_available_heads(&mut memory, &[0, 2]);
        let throttled = device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("second pmem event should be throttled before popping the queue");

        assert_eq!(
            first
                .queue_dispatch()
                .expect("first queue dispatch should be present")
                .processed_requests(),
            1
        );
        let throttled_queue = throttled
            .queue_dispatch()
            .expect("throttled queue dispatch should be present");
        assert_eq!(throttled_queue.processed_requests(), 0);
        assert_eq!(
            throttled_queue.rate_limiter_retry_after(),
            Some(Duration::from_millis(20))
        );
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(flush_calls.get(), 1);
        assert!(device.has_pending_rate_limited_queue());

        let retry = device
            .dispatch_drained_queue_notifications_at(
                &mut memory,
                Vec::new(),
                &mut flush,
                now + Duration::from_millis(20),
            )
            .expect("pending pmem queue should retry without another guest notification");
        let retry_queue = retry
            .queue_dispatch()
            .expect("retry queue dispatch should be present");
        assert!(retry.drained_notifications().is_empty());
        assert_eq!(retry_queue.processed_requests(), 1);
        assert_eq!(retry_queue.rate_limiter_events(), 1);
        assert_eq!(read_used_index(&memory), 2);
        assert_eq!(flush_calls.get(), 2);
        assert!(!device.has_pending_rate_limited_queue());

        write_pmem_flush_chain_at(
            &mut memory,
            4,
            5,
            GuestAddress::new(0x2040),
            GuestAddress::new(0x3040),
        );
        write_available_heads(&mut memory, &[0, 2, 4]);
        let ops_throttled = device
            .dispatch_drained_queue_notifications_at(
                &mut memory,
                vec![0],
                &mut flush,
                now + Duration::from_millis(20),
            )
            .expect("rolled-back op should be consumed by the successful byte retry");
        assert!(
            ops_throttled
                .rate_limiter_retry_after()
                .is_some_and(|retry_after| !retry_after.is_zero())
        );
        assert_eq!(read_used_index(&memory), 2);
        assert_eq!(flush_calls.get(), 2);

        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0"]);
        metrics.record_notification_dispatch_for_device("pmem0", &throttled);
        metrics.record_notification_dispatch_for_device("pmem0", &retry);
        let expected_metrics = PmemDeviceMetrics::default()
            .with_queue_event_count(1)
            .with_rate_limiter_throttled_events(1)
            .with_rate_limiter_event_count(1);
        assert_eq!(metrics.aggregate_snapshot(), expected_metrics);
        assert_eq!(
            metrics.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new().with_device_metrics("pmem0", expected_metrics)
        );
    }

    #[test]
    fn pmem_device_rate_limiter_charges_malformed_nonempty_queue_without_flushing() {
        let now = Instant::now();
        let mut memory = request_memory();
        write_request_type(&mut memory, VIRTIO_PMEM_REQUEST_TYPE_FLUSH);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(TEST_PMEM_REQUEST_ADDR, VIRTIO_PMEM_REQUEST_SIZE, None),
        );
        write_available_heads(&mut memory, &[0]);
        let rate_limiter =
            PmemRateLimiterConfig::new(None, Some(PmemTokenBucketConfig::new(1, None, 100)));
        let mut device = VirtioPmemDevice::with_rate_limiter_at(4096, Some(rate_limiter), now);
        device.active_queue = Some(pmem_queue());
        let mut flush_calls = 0;
        let mut flush = || {
            flush_calls += 1;
            VirtioPmemFlushStatus::Success
        };

        let malformed = device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("malformed nonempty queue should still complete");
        write_pmem_flush_chain_at(
            &mut memory,
            2,
            3,
            GuestAddress::new(0x2020),
            GuestAddress::new(0x3020),
        );
        write_available_heads(&mut memory, &[0, 2]);
        let throttled = device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("malformed request should have consumed the only op token");

        let malformed_queue = malformed
            .queue_dispatch()
            .expect("malformed queue dispatch should be present");
        assert_eq!(malformed_queue.parse_failures(), 1);
        assert_eq!(flush_calls, 0);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(
            throttled.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(read_used_index(&memory), 1);
    }

    #[test]
    fn pmem_device_rate_limiter_skips_empty_queue_and_charges_coalesced_event_once() {
        let now = Instant::now();
        let mut memory = request_memory();
        let rate_limiter =
            PmemRateLimiterConfig::new(None, Some(PmemTokenBucketConfig::new(1, None, 100)));
        let mut device = VirtioPmemDevice::with_rate_limiter_at(4096, Some(rate_limiter), now);
        device.active_queue = Some(pmem_queue());
        let flush_calls = Cell::new(0);
        let mut flush = || {
            flush_calls.set(flush_calls.get() + 1);
            VirtioPmemFlushStatus::Success
        };

        let empty = device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("empty pmem queue should not consume a limiter token");
        assert_eq!(
            empty
                .queue_dispatch()
                .expect("empty queue check should be reported")
                .processed_requests(),
            0
        );

        write_pmem_flush_chain(&mut memory);
        write_pmem_flush_chain_at(
            &mut memory,
            2,
            3,
            GuestAddress::new(0x2020),
            GuestAddress::new(0x3020),
        );
        write_available_heads(&mut memory, &[0, 2]);
        let coalesced = device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("one admitted event should drain both coalesced requests");

        assert_eq!(
            coalesced
                .queue_dispatch()
                .expect("coalesced dispatch should be present")
                .processed_requests(),
            2
        );
        assert_eq!(flush_calls.get(), 1);
        assert_eq!(read_used_index(&memory), 2);

        write_pmem_flush_chain_at(
            &mut memory,
            4,
            5,
            GuestAddress::new(0x2040),
            GuestAddress::new(0x3040),
        );
        write_available_heads(&mut memory, &[0, 2, 4]);
        let throttled = device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("the next event should observe the single consumed op token");

        assert_eq!(
            throttled
                .queue_dispatch()
                .expect("throttled event should be reported")
                .rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(flush_calls.get(), 1);
        assert_eq!(read_used_index(&memory), 2);
    }

    #[test]
    fn pmem_bandwidth_limiter_retries_oversized_file_after_full_refill() {
        let now = Instant::now();
        let mut memory = request_memory();
        write_pmem_flush_chain(&mut memory);
        let rate_limiter =
            PmemRateLimiterConfig::new(Some(PmemTokenBucketConfig::new(50, None, 100)), None);
        let mut device = VirtioPmemDevice::with_rate_limiter_at(80, Some(rate_limiter), now);
        device.active_queue = Some(pmem_queue());
        let flush_calls = Cell::new(0);
        let mut flush = || {
            flush_calls.set(flush_calls.get() + 1);
            VirtioPmemFlushStatus::Success
        };

        device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("a full bucket should admit one oversized file charge");
        write_pmem_flush_chain_at(
            &mut memory,
            2,
            3,
            GuestAddress::new(0x2020),
            GuestAddress::new(0x3020),
        );
        write_available_heads(&mut memory, &[0, 2]);
        let throttled = device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("a drained oversized bucket should throttle without popping");
        assert_eq!(
            throttled.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(read_used_index(&memory), 1);

        let retried = device
            .dispatch_drained_queue_notifications_at(
                &mut memory,
                Vec::new(),
                &mut flush,
                now + Duration::from_millis(100),
            )
            .expect("a full refill should admit the pending oversized charge");
        assert_eq!(
            retried
                .queue_dispatch()
                .expect("retried queue dispatch should be present")
                .processed_requests(),
            1
        );
        assert_eq!(flush_calls.get(), 2);
        assert_eq!(read_used_index(&memory), 2);
    }

    #[test]
    fn live_pmem_rate_limiter_disable_reports_and_unblocks_pending_queue() {
        let now = Instant::now();
        let mut memory = request_memory();
        write_pmem_flush_chain(&mut memory);
        let rate_limiter =
            PmemRateLimiterConfig::new(None, Some(PmemTokenBucketConfig::new(1, None, 100)));
        let mut device = VirtioPmemDevice::with_rate_limiter_at(4096, Some(rate_limiter), now);
        device.active_queue = Some(pmem_queue());
        let mut flush_calls = 0;
        let mut flush = || {
            flush_calls += 1;
            VirtioPmemFlushStatus::Success
        };
        device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("first pmem event should dispatch");
        write_pmem_flush_chain_at(
            &mut memory,
            2,
            3,
            GuestAddress::new(0x2020),
            GuestAddress::new(0x3020),
        );
        write_available_heads(&mut memory, &[0, 2]);
        device
            .dispatch_drained_queue_notifications_at(&mut memory, vec![0], &mut flush, now)
            .expect("second pmem event should throttle");
        let disable =
            PmemUpdate::try_from(PmemUpdateInput::new("pmem0", "pmem0").with_rate_limiter(
                PmemRateLimiterConfig::new(None, Some(PmemTokenBucketConfig::new(0, None, 0))),
            ))
            .expect("pmem limiter disable should validate");

        assert!(device.update_rate_limiter_at(&disable, now));
        let retry = device
            .dispatch_drained_queue_notifications_at(&mut memory, Vec::new(), &mut flush, now)
            .expect("disabled limiter should immediately unblock pending work");

        assert_eq!(
            retry
                .queue_dispatch()
                .expect("retry queue dispatch should be present")
                .processed_requests(),
            1
        );
        assert_eq!(read_used_index(&memory), 2);
        assert_eq!(flush_calls, 2);
        assert!(!device.has_pending_rate_limited_queue());
    }

    #[test]
    fn pmem_mmio_devices_register_single_prepared_device() {
        let file = temp_sized_file("pmem-mmio-single.img", VIRTIO_PMEM_ALIGNMENT);
        let configs = [pmem_config(PmemConfigInput::new(
            "pmem0",
            file.as_path().display().to_string(),
        ))];
        let prepared =
            PreparedPmemDevices::from_config_slice(&configs).expect("pmem device should prepare");

        let mut devices = prepared
            .register_mmio(PmemMmioLayout::new(
                TEST_PMEM_MMIO_BASE,
                TEST_PMEM_MMIO_REGION_ID,
            ))
            .expect("pmem MMIO device should register");

        assert_eq!(devices.len(), 1);
        assert_eq!(devices.pmem_devices().len(), 1);
        let registration = &devices.registrations()[0];
        assert_eq!(registration.index(), 0);
        assert_eq!(registration.pmem_id(), "pmem0");
        assert_eq!(registration.region_id(), TEST_PMEM_MMIO_REGION_ID);
        assert_eq!(registration.address(), TEST_PMEM_MMIO_BASE);
        assert_eq!(
            registration.guest_range(),
            devices.pmem_devices()[0].guest_range()
        );
        assert_eq!(
            registration.file_len(),
            devices.pmem_devices()[0].mapping().file_len()
        );
        assert_eq!(
            registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(
            registration.config_space(),
            devices.pmem_devices()[0].config_space()
        );
        assert_eq!(devices.dispatcher().regions().len(), 1);
        assert_eq!(devices.dispatcher().regions()[0], registration.region());
        assert_eq!(
            dispatch_pmem_mmio_read_u32(&mut devices, 0, VirtioMmioRegister::MagicValue.offset()),
            VIRTIO_MMIO_MAGIC_VALUE
        );
        assert_eq!(
            dispatch_pmem_mmio_read_u32(&mut devices, 0, VirtioMmioRegister::DeviceId.offset()),
            VIRTIO_PMEM_DEVICE_ID
        );
    }

    #[test]
    fn pmem_mmio_devices_preserve_order_and_layout() {
        let first = temp_sized_file("pmem-mmio-first.img", VIRTIO_PMEM_ALIGNMENT);
        let second = temp_sized_file("pmem-mmio-second.img", VIRTIO_PMEM_ALIGNMENT);
        let configs = [
            pmem_config(PmemConfigInput::new(
                "pmem0",
                first.as_path().display().to_string(),
            )),
            pmem_config(PmemConfigInput::new(
                "pmem1",
                second.as_path().display().to_string(),
            )),
        ];
        let prepared =
            PreparedPmemDevices::from_config_slice(&configs).expect("pmem devices should prepare");

        let devices = prepared
            .register_mmio(
                PmemMmioLayout::new(TEST_PMEM_MMIO_BASE, MmioRegionId::new(9100))
                    .with_address_stride(VIRTIO_MMIO_DEVICE_WINDOW_SIZE * 2)
                    .with_region_id_stride(3),
            )
            .expect("pmem MMIO devices should register");

        assert_eq!(devices.registrations()[0].pmem_id(), "pmem0");
        assert_eq!(devices.registrations()[0].index(), 0);
        assert_eq!(
            devices.registrations()[0].region_id(),
            MmioRegionId::new(9100)
        );
        assert_eq!(devices.registrations()[0].address(), TEST_PMEM_MMIO_BASE);
        assert_eq!(devices.registrations()[1].pmem_id(), "pmem1");
        assert_eq!(devices.registrations()[1].index(), 1);
        assert_eq!(
            devices.registrations()[1].region_id(),
            MmioRegionId::new(9103)
        );
        assert_eq!(
            devices.registrations()[1].address(),
            GuestAddress::new(
                TEST_PMEM_MMIO_BASE.raw_value() + (VIRTIO_MMIO_DEVICE_WINDOW_SIZE * 2)
            )
        );
        assert_eq!(devices.pmem_devices()[0].id(), "pmem0");
        assert_eq!(devices.pmem_devices()[1].id(), "pmem1");
    }

    #[test]
    fn pmem_mmio_devices_dispatch_config_space_reads() {
        let file = temp_sized_file("pmem-mmio-config.img", VIRTIO_PMEM_ALIGNMENT);
        let configs = [pmem_config(PmemConfigInput::new(
            "pmem0",
            file.as_path().display().to_string(),
        ))];
        let prepared =
            PreparedPmemDevices::from_config_slice(&configs).expect("pmem device should prepare");
        let expected_config = prepared.as_slice()[0].config_space();
        let mut devices = prepared
            .register_mmio(PmemMmioLayout::new(
                TEST_PMEM_MMIO_BASE,
                TEST_PMEM_MMIO_REGION_ID,
            ))
            .expect("pmem MMIO device should register");

        let start = dispatch_pmem_mmio_read(&mut devices, 0, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 8);
        let size =
            dispatch_pmem_mmio_read(&mut devices, 0, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 8, 8);

        assert_eq!(start.as_slice(), &expected_config.start().to_le_bytes());
        assert_eq!(size.as_slice(), &expected_config.size().to_le_bytes());
    }

    #[test]
    fn pmem_mmio_devices_reject_overlapping_existing_dispatcher_region() {
        let file = temp_sized_file("pmem-mmio-overlap.img", VIRTIO_PMEM_ALIGNMENT);
        let configs = [pmem_config(PmemConfigInput::new(
            "pmem0",
            file.as_path().display().to_string(),
        ))];
        let prepared =
            PreparedPmemDevices::from_config_slice(&configs).expect("pmem device should prepare");
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(1),
                TEST_PMEM_MMIO_BASE,
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("existing MMIO region should insert");

        let err = prepared
            .register_mmio_with_dispatcher(
                PmemMmioLayout::new(TEST_PMEM_MMIO_BASE, TEST_PMEM_MMIO_REGION_ID),
                dispatcher,
            )
            .expect_err("overlapping pmem MMIO region should fail");

        assert!(matches!(
            err,
            PmemMmioRegistrationError::InsertRegion {
                pmem_id,
                region_id: TEST_PMEM_MMIO_REGION_ID,
                source: crate::mmio::MmioBusError::OverlappingRegion { .. },
                ..
            } if pmem_id == "pmem0"
        ));
    }

    #[test]
    fn pmem_mmio_layout_rejects_overlapping_address_stride() {
        let err = PreparedPmemDevices::default()
            .register_mmio(
                PmemMmioLayout::new(TEST_PMEM_MMIO_BASE, TEST_PMEM_MMIO_REGION_ID)
                    .with_address_stride(VIRTIO_MMIO_DEVICE_WINDOW_SIZE - 1),
            )
            .expect_err("overlapping pmem MMIO address stride should fail");

        assert!(matches!(
            err,
            PmemMmioRegistrationError::AddressStrideTooSmall {
                stride,
                minimum: VIRTIO_MMIO_DEVICE_WINDOW_SIZE
            } if stride == VIRTIO_MMIO_DEVICE_WINDOW_SIZE - 1
        ));
    }

    #[test]
    fn pmem_mmio_layout_rejects_duplicate_region_id_stride() {
        let err = PreparedPmemDevices::default()
            .register_mmio(
                PmemMmioLayout::new(TEST_PMEM_MMIO_BASE, TEST_PMEM_MMIO_REGION_ID)
                    .with_region_id_stride(0),
            )
            .expect_err("duplicate pmem MMIO region id stride should fail");

        assert!(matches!(
            err,
            PmemMmioRegistrationError::DuplicateRegionIdStride {
                region_id: TEST_PMEM_MMIO_REGION_ID
            }
        ));
    }

    #[test]
    fn input_defaults_to_firecracker_pmem_defaults() {
        let input = PmemConfigInput::new("pmem0", "/tmp/pmem.img");

        assert_eq!(input.id(), "pmem0");
        assert_eq!(input.path_on_host(), "/tmp/pmem.img");
        assert!(!input.root_device());
        assert!(!input.read_only());
        assert!(!input.rate_limiter_configured());
    }

    #[test]
    fn config_normalizes_disabled_rate_limiter_buckets_to_absent() {
        let config = pmem_config(
            PmemConfigInput::new("pmem0", "/tmp/pmem.img").with_rate_limiter(
                PmemRateLimiterConfig::new(
                    Some(PmemTokenBucketConfig::new(0, Some(10), 100)),
                    Some(PmemTokenBucketConfig::new(1, None, 0)),
                ),
            ),
        );

        assert_eq!(config.rate_limiter(), None);
    }

    #[test]
    fn config_accepts_firecracker_id_character_set() {
        let config = pmem_config(PmemConfigInput::new("pmem_\u{00e9}1", "/tmp/pmem.img"));

        assert_eq!(config.id(), "pmem_\u{00e9}1");
    }

    #[test]
    fn config_rejects_empty_pmem_id() {
        let err = PmemConfig::try_from(PmemConfigInput::new("", "/tmp/pmem.img"))
            .expect_err("empty pmem id should fail");

        assert_eq!(err, PmemConfigError::EmptyPmemId);
        assert_eq!(err.to_string(), "pmem id must not be empty");
    }

    #[test]
    fn config_rejects_invalid_pmem_id_without_echoing_it() {
        let invalid = "bad/id\nsecret";
        let err = PmemConfig::try_from(PmemConfigInput::new(invalid, "/tmp/pmem.img"))
            .expect_err("invalid pmem id should fail");

        assert_eq!(err, PmemConfigError::InvalidPmemId);
        assert_eq!(
            err.to_string(),
            "pmem id must contain only alphanumeric characters or '_'"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn config_rejects_empty_path_on_host() {
        let err = PmemConfig::try_from(PmemConfigInput::new("pmem0", ""))
            .expect_err("empty pmem path should fail");

        assert_eq!(err, PmemConfigError::EmptyPathOnHost);
        assert_eq!(err.to_string(), "pmem path_on_host must not be empty");
    }

    #[test]
    fn config_rejects_root_device() {
        let err = PmemConfig::try_from(
            PmemConfigInput::new("pmem0", "/tmp/pmem.img").with_root_device(true),
        )
        .expect_err("pmem root device should fail");

        assert_eq!(err, PmemConfigError::UnsupportedRootDevice);
        assert_eq!(err.to_string(), "pmem root_device is not supported");
    }

    #[test]
    fn update_input_defaults_to_noop_update() {
        let input = PmemUpdateInput::new("pmem0", "pmem0");

        assert_eq!(input.path_pmem_id(), "pmem0");
        assert_eq!(input.body_pmem_id(), "pmem0");
        assert!(!input.rate_limiter_configured());
    }

    #[test]
    fn update_accepts_existing_noop_pmem_update() {
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new("pmem0", "/tmp/pmem.img")));

        let update = configs
            .validate_update(PmemUpdateInput::new("pmem0", "pmem0"))
            .expect("existing no-op pmem update should validate");

        assert_eq!(update.id(), "pmem0");
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].path_on_host(), "/tmp/pmem.img");
    }

    #[test]
    fn update_rejects_unknown_pmem() {
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new("pmem0", "/tmp/pmem.img")));

        let err = configs
            .validate_update(PmemUpdateInput::new("pmem1", "pmem1"))
            .expect_err("unknown pmem update should fail");

        assert_eq!(err, PmemUpdateError::UnknownPmem);
        assert_eq!(err.to_string(), "pmem device is not configured");
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].id(), "pmem0");
        assert_eq!(configs.as_slice()[0].path_on_host(), "/tmp/pmem.img");
    }

    #[test]
    fn update_accepts_configured_rate_limiter_without_committing() {
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new("pmem0", "/tmp/pmem.img")));
        let rate_limiter =
            PmemRateLimiterConfig::new(None, Some(PmemTokenBucketConfig::new(1, None, 100)));

        let update = configs
            .validate_update(PmemUpdateInput::new("pmem0", "pmem0").with_rate_limiter(rate_limiter))
            .expect("configured pmem rate limiter should validate");

        assert_eq!(update.rate_limiter(), Some(rate_limiter));
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].id(), "pmem0");
        assert_eq!(configs.as_slice()[0].path_on_host(), "/tmp/pmem.img");
        assert_eq!(configs.as_slice()[0].rate_limiter(), None);
    }

    #[test]
    fn update_partially_disables_one_bucket_and_preserves_the_other() {
        let bandwidth = PmemTokenBucketConfig::new(4096, Some(8192), 100);
        let ops = PmemTokenBucketConfig::new(10, None, 1000);
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(
            PmemConfigInput::new("pmem0", "/tmp/pmem.img")
                .with_rate_limiter(PmemRateLimiterConfig::new(Some(bandwidth), Some(ops))),
        ));

        let (update, updated) = configs
            .prepare_update(PmemUpdateInput::new("pmem0", "pmem0").with_rate_limiter(
                PmemRateLimiterConfig::new(None, Some(PmemTokenBucketConfig::new(0, None, 0))),
            ))
            .expect("partial pmem limiter update should prepare");

        assert_eq!(
            update.rate_limiter(),
            Some(PmemRateLimiterConfig::new(
                None,
                Some(PmemTokenBucketConfig::new(0, None, 0))
            ))
        );
        assert_eq!(
            updated.rate_limiter(),
            Some(PmemRateLimiterConfig::new(Some(bandwidth), None))
        );
        assert_eq!(
            configs.as_slice()[0].rate_limiter(),
            Some(PmemRateLimiterConfig::new(Some(bandwidth), Some(ops)))
        );

        configs
            .commit_update(updated)
            .expect("prepared pmem limiter update should commit");
        assert_eq!(
            configs.as_slice()[0].rate_limiter(),
            Some(PmemRateLimiterConfig::new(Some(bandwidth), None))
        );
    }

    #[test]
    fn update_rejects_empty_path_pmem_id() {
        let err = PmemUpdate::try_from(PmemUpdateInput::new("", "pmem0"))
            .expect_err("empty path pmem id should fail");

        assert_eq!(
            err,
            PmemUpdateError::EmptyPmemId {
                source: PmemIdSource::Path
            }
        );
        assert_eq!(err.to_string(), "path pmem id must not be empty");
    }

    #[test]
    fn update_rejects_empty_body_pmem_id() {
        let err = PmemUpdate::try_from(PmemUpdateInput::new("pmem0", ""))
            .expect_err("empty body pmem id should fail");

        assert_eq!(
            err,
            PmemUpdateError::EmptyPmemId {
                source: PmemIdSource::Body
            }
        );
        assert_eq!(err.to_string(), "body id must not be empty");
    }

    #[test]
    fn update_rejects_invalid_pmem_id_without_echoing_it() {
        let invalid = "bad/id\nsecret";
        let err = PmemUpdate::try_from(PmemUpdateInput::new(invalid, invalid))
            .expect_err("invalid pmem update id should fail");

        assert_eq!(
            err,
            PmemUpdateError::InvalidPmemId {
                source: PmemIdSource::Path
            }
        );
        assert_eq!(
            err.to_string(),
            "path pmem id must contain only alphanumeric characters or '_'"
        );
        assert!(!err.to_string().contains(invalid));
        assert!(!format!("{err:?}").contains(invalid));
    }

    #[test]
    fn update_rejects_mismatched_pmem_id_without_echoing_values() {
        let err = PmemUpdate::try_from(PmemUpdateInput::new("pmem0", "pmem1"))
            .expect_err("mismatched pmem update id should fail");

        assert_eq!(err, PmemUpdateError::MismatchedPmemId);
        assert_eq!(err.to_string(), "path pmem id must match body id");
        assert!(!format!("{err:?}").contains("pmem0"));
        assert!(!format!("{err:?}").contains("pmem1"));
    }

    #[test]
    fn upsert_replaces_matching_id_without_mutating_others() {
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new("pmem0", "/tmp/old.img")));
        configs.upsert(pmem_config(PmemConfigInput::new("pmem1", "/tmp/other.img")));
        configs.upsert(pmem_config(
            PmemConfigInput::new("pmem0", "/tmp/new.img").with_read_only(true),
        ));

        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].id(), "pmem0");
        assert_eq!(configs.as_slice()[0].path_on_host(), "/tmp/new.img");
        assert!(!configs.as_slice()[0].root_device());
        assert!(configs.as_slice()[0].read_only());
        assert_eq!(configs.as_slice()[1].id(), "pmem1");
        assert_eq!(configs.as_slice()[1].path_on_host(), "/tmp/other.img");
    }

    #[test]
    fn file_backing_opens_regular_file_read_only() {
        let file = temp_file("readonly-pmem.img", b"pmem");
        let backing = open_backing(file.as_path(), true).expect("pmem backing should open");

        assert_eq!(backing.len(), 4);
        assert!(backing.is_read_only());
        assert_eq!(
            backing
                .file()
                .metadata()
                .expect("opened pmem backing should have metadata")
                .len(),
            4
        );
    }

    #[test]
    fn file_backing_opens_regular_file_writable() {
        let file = temp_file("writable-pmem.img", b"pmem");
        let backing = open_backing(file.as_path(), false).expect("pmem backing should open");

        assert_eq!(backing.len(), 4);
        assert!(!backing.is_read_only());
    }

    #[test]
    fn file_backing_adopts_provided_regular_file() {
        let file = temp_file("provided-pmem.img", b"pmem");
        let provided_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(file.as_path())
            .expect("provided pmem backing should open");
        let backing = PmemFileBacking::from_file(provided_file, false)
            .expect("provided pmem backing should validate");

        assert_eq!(backing.len(), 4);
        assert!(!backing.is_empty());
        assert!(!backing.is_read_only());
        assert_eq!(
            backing
                .file()
                .metadata()
                .expect("provided pmem backing should have metadata")
                .len(),
            4
        );
    }

    #[test]
    fn file_backing_rejects_provided_directory_and_zero_sized_file() {
        let dir = temp_dir("provided-dir-pmem.img");
        let provided_dir = fs::File::open(dir.as_path()).expect("provided directory should open");
        let dir_err = PmemFileBacking::from_file(provided_dir, true)
            .expect_err("provided directory should fail");

        let file = temp_file("provided-empty-pmem.img", b"");
        let provided_file = fs::File::open(file.as_path()).expect("provided file should open");
        let file_err = PmemFileBacking::from_file(provided_file, true)
            .expect_err("provided zero-sized file should fail");

        assert!(matches!(dir_err, PmemFileBackingError::NonRegularFile));
        assert!(matches!(file_err, PmemFileBackingError::ZeroSizedFile));
    }

    #[test]
    fn file_backing_rejects_missing_path_without_echoing_it() {
        let path = missing_path("secret-missing-pmem.img");
        let err = open_backing(&path, true).expect_err("missing pmem backing should fail");

        assert!(matches!(err, PmemFileBackingError::OpenFile { .. }));
        assert_eq!(
            err.source()
                .and_then(|source| source.downcast_ref::<io::Error>())
                .map(io::Error::kind),
            Some(io::ErrorKind::NotFound)
        );
        assert!(!err.to_string().contains("secret-missing-pmem"));
    }

    #[test]
    fn file_backing_rejects_directory_path() {
        let dir = temp_dir("dir-pmem.img");
        let err = open_backing(dir.as_path(), true).expect_err("directory backing should fail");

        assert!(matches!(err, PmemFileBackingError::NonRegularFile));
        assert_eq!(
            err.to_string(),
            "pmem backing path does not reference a regular file"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn file_backing_rejects_fifo_path_without_blocking() {
        let fifo = temp_fifo("fifo-pmem.img");
        let err = open_backing(fifo.as_path(), true).expect_err("FIFO backing should fail");

        assert!(matches!(err, PmemFileBackingError::NonRegularFile));
    }

    #[test]
    fn file_backing_rejects_socket_path_without_blocking() {
        let (socket, listener) = temp_socket("socket-pmem.img");
        let err = open_backing(socket.as_path(), true).expect_err("socket backing should fail");
        drop(listener);

        assert!(matches!(
            err,
            PmemFileBackingError::OpenFile { .. } | PmemFileBackingError::NonRegularFile
        ));
        assert!(!err.to_string().contains("socket-pmem"));
    }

    #[test]
    fn file_backing_rejects_zero_sized_file() {
        let file = temp_file("empty-pmem.img", b"");
        let err = open_backing(file.as_path(), true).expect_err("empty pmem backing should fail");

        assert!(matches!(err, PmemFileBackingError::ZeroSizedFile));
        assert_eq!(err.to_string(), "pmem backing file is zero-sized");
        assert!(err.source().is_none());
    }

    #[test]
    fn backing_mapping_maps_unaligned_file_to_2m_region() {
        let file = temp_file("mapped-unaligned-pmem.img", b"pmem");
        let mapping = map_backing(file.as_path(), false);
        let mut bytes = [0; 4];
        let last_offset = mapping
            .host_size()
            .checked_sub(1)
            .expect("mapping should be non-empty");

        // SAFETY: `mapping` owns a live mapping whose first `file_len` bytes
        // are backed by the test file. The final byte is inside the retained
        // aligned reservation, and `bytes` is a valid destination.
        unsafe {
            std::ptr::copy_nonoverlapping(
                mapping.host_address().as_ptr().cast::<u8>(),
                bytes.as_mut_ptr(),
                bytes.len(),
            );
            assert_eq!(
                mapping
                    .host_address()
                    .as_ptr()
                    .cast::<u8>()
                    .add(last_offset)
                    .read(),
                0
            );
        }

        assert_eq!(mapping.file_len(), 4);
        assert_eq!(mapping.mapped_len(), VIRTIO_PMEM_ALIGNMENT);
        assert_eq!(
            mapping.host_size(),
            usize::try_from(VIRTIO_PMEM_ALIGNMENT).expect("alignment should fit usize")
        );
        assert!(!mapping.is_read_only());
        assert_eq!(&bytes, b"pmem");
    }

    #[test]
    fn backing_mapping_keeps_aligned_file_length() {
        let file = temp_sized_file("mapped-aligned-pmem.img", VIRTIO_PMEM_ALIGNMENT);
        let mapping = map_backing(file.as_path(), true);

        assert_eq!(mapping.file_len(), VIRTIO_PMEM_ALIGNMENT);
        assert_eq!(mapping.mapped_len(), VIRTIO_PMEM_ALIGNMENT);
        assert_eq!(
            mapping.host_size(),
            usize::try_from(VIRTIO_PMEM_ALIGNMENT).expect("alignment should fit usize")
        );
        assert!(mapping.is_read_only());
    }

    #[test]
    fn backing_mapping_writable_mapping_updates_file() {
        let file = temp_file("mapped-writable-pmem.img", b"pmem");
        let mapping = map_backing(file.as_path(), false);

        // SAFETY: `mapping` owns a live writable mapping because the backing
        // was opened with read_only=false. The write stays within file_len.
        unsafe {
            mapping.host_address().as_ptr().cast::<u8>().write(b'P');
            let result = libc::msync(
                mapping.host_address().as_ptr(),
                usize::try_from(mapping.file_len()).expect("file length should fit usize"),
                libc::MS_SYNC,
            );
            assert_eq!(result, 0, "msync failed: {}", io::Error::last_os_error());
        }
        drop(mapping);

        assert_eq!(
            fs::read(file.as_path()).expect("test file should read"),
            b"Pmem"
        );
    }

    #[test]
    fn backing_mapping_debug_omits_host_address_and_path() {
        let file = temp_file("secret-debug-pmem.img", b"pmem");
        let mapping = map_backing(file.as_path(), true);
        let debug = format!("{mapping:?}");
        let host_address = format!("{:p}", mapping.host_address().as_ptr());

        assert!(!debug.contains(&host_address));
        assert!(!debug.contains("secret-debug-pmem"));
        assert!(debug.contains("file_len"));
        assert!(debug.contains("mapped_len"));
    }

    #[test]
    fn backing_mapping_alignment_rejects_overflow() {
        let err = align_pmem_mapping_len(u64::MAX)
            .expect_err("maximum length should not align without overflow");

        assert!(matches!(
            err,
            PmemBackingMappingError::MappedLengthOverflow {
                len: u64::MAX,
                alignment: VIRTIO_PMEM_ALIGNMENT,
            }
        ));
    }

    #[test]
    fn guest_range_allocator_starts_after_mmio64_gap() {
        let layout = guest_layout(vec![guest_range(aarch64::DRAM_MEM_START, 8 * 1024 * 1024)]);
        let mut allocator = PmemGuestRangeAllocator::for_layout(&layout);

        let range = allocator
            .allocate(VIRTIO_PMEM_ALIGNMENT)
            .expect("pmem guest range should allocate");

        assert_eq!(range.start(), GuestAddress::new(TEST_PMEM_START));
        assert_eq!(range.size(), VIRTIO_PMEM_ALIGNMENT);
        assert_eq!(
            range.end_exclusive(),
            GuestAddress::new(TEST_PMEM_START + VIRTIO_PMEM_ALIGNMENT)
        );
    }

    #[test]
    fn guest_range_allocator_allocates_multiple_devices_in_order() {
        let mut allocator = PmemGuestRangeAllocator::without_reserved_ranges();

        let first = allocator
            .allocate(VIRTIO_PMEM_ALIGNMENT * 2)
            .expect("first pmem guest range should allocate");
        let second = allocator
            .allocate(VIRTIO_PMEM_ALIGNMENT)
            .expect("second pmem guest range should allocate");

        assert_eq!(first.start(), GuestAddress::new(TEST_PMEM_START));
        assert_eq!(first.size(), VIRTIO_PMEM_ALIGNMENT * 2);
        assert_eq!(
            second.start(),
            GuestAddress::new(TEST_PMEM_START + (VIRTIO_PMEM_ALIGNMENT * 2))
        );
        assert_eq!(second.size(), VIRTIO_PMEM_ALIGNMENT);
        assert!(!first.overlaps(second));
    }

    #[test]
    fn guest_range_allocator_skips_guest_ram_after_mmio64_gap() {
        let high_ram_size = VIRTIO_PMEM_ALIGNMENT * 3;
        let layout = guest_layout(vec![
            guest_range(aarch64::DRAM_MEM_START, 8 * 1024 * 1024),
            guest_range(aarch64::FIRST_ADDR_PAST_64BITS_MMIO, high_ram_size),
        ]);
        let mut allocator = PmemGuestRangeAllocator::for_layout(&layout);

        let range = allocator
            .allocate(VIRTIO_PMEM_ALIGNMENT)
            .expect("pmem guest range should skip high RAM");

        assert_eq!(
            range.start(),
            GuestAddress::new(aarch64::FIRST_ADDR_PAST_64BITS_MMIO + high_ram_size)
        );
        assert_eq!(range.size(), VIRTIO_PMEM_ALIGNMENT);
        for reserved in layout.ranges() {
            assert!(!range.overlaps(*reserved));
        }
    }

    #[test]
    fn guest_range_allocator_skips_guest_ram_inside_candidate_range() {
        let high_ram_start = TEST_PMEM_START + (VIRTIO_PMEM_ALIGNMENT * 4);
        let high_ram_size = VIRTIO_PMEM_ALIGNMENT * 2;
        let requested_size = VIRTIO_PMEM_ALIGNMENT * 6;
        let layout = guest_layout(vec![
            guest_range(aarch64::DRAM_MEM_START, 8 * 1024 * 1024),
            guest_range(high_ram_start, high_ram_size),
        ]);
        let mut allocator = PmemGuestRangeAllocator::for_layout(&layout);

        let range = allocator
            .allocate(requested_size)
            .expect("pmem guest range should skip later high RAM");

        assert_eq!(
            range.start(),
            GuestAddress::new(high_ram_start + high_ram_size)
        );
        assert_eq!(range.size(), requested_size);
        for reserved in layout.ranges() {
            assert!(!range.overlaps(*reserved));
        }
    }

    #[test]
    fn guest_range_allocator_rejects_unaligned_size() {
        let mut allocator = PmemGuestRangeAllocator::without_reserved_ranges();

        let err = allocator
            .allocate(VIRTIO_PMEM_ALIGNMENT - 1)
            .expect_err("unaligned pmem size should fail");

        assert_eq!(
            err,
            PmemGuestRangeAllocationError::UnalignedSize {
                size: VIRTIO_PMEM_ALIGNMENT - 1,
                alignment: VIRTIO_PMEM_ALIGNMENT,
            }
        );
    }

    #[test]
    fn guest_range_allocator_rejects_address_alignment_overflow() {
        let mut allocator = PmemGuestRangeAllocator::with_start_for_test(u64::MAX - 1, &[]);

        let err = allocator
            .allocate(VIRTIO_PMEM_ALIGNMENT)
            .expect_err("unalignable high address should fail");

        assert_eq!(
            err,
            PmemGuestRangeAllocationError::AddressAlignmentOverflow {
                address: u64::MAX - 1,
                alignment: VIRTIO_PMEM_ALIGNMENT,
            }
        );
    }

    #[test]
    fn guest_range_allocator_rejects_range_end_overflow() {
        let start = u64::MAX - VIRTIO_PMEM_ALIGNMENT + 1;
        let mut allocator = PmemGuestRangeAllocator::with_start_for_test(start, &[]);

        let err = allocator
            .allocate(VIRTIO_PMEM_ALIGNMENT)
            .expect_err("range ending past u64 should fail");

        assert!(matches!(
            err,
            PmemGuestRangeAllocationError::InvalidRange {
                start: error_start,
                size,
                source: GuestMemoryError::AddressOverflow { .. },
            } if error_start == GuestAddress::new(start) && size == VIRTIO_PMEM_ALIGNMENT
        ));
    }

    #[test]
    fn prepared_devices_open_all_configured_backings() {
        let first = temp_file("first-pmem.img", b"first");
        let second = temp_file("second-pmem.img", b"second");
        let configs = [
            pmem_config(PmemConfigInput::new(
                "pmem0",
                first.as_path().to_string_lossy().into_owned(),
            )),
            pmem_config(
                PmemConfigInput::new("pmem1", second.as_path().to_string_lossy().into_owned())
                    .with_read_only(true),
            ),
        ];

        let prepared =
            PreparedPmemDevices::from_config_slice(&configs).expect("pmem devices should prepare");

        assert_eq!(prepared.len(), 2);
        assert!(!prepared.is_empty());
        assert_eq!(prepared.as_slice()[0].id(), "pmem0");
        assert_eq!(prepared.as_slice()[0].backing().len(), 5);
        assert!(!prepared.as_slice()[0].backing().is_read_only());
        assert_eq!(prepared.as_slice()[0].mapping().file_len(), 5);
        assert_eq!(
            prepared.as_slice()[0].mapping().mapped_len(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert!(!prepared.as_slice()[0].mapping().is_read_only());
        assert_eq!(
            prepared.as_slice()[0].guest_range(),
            guest_range(TEST_PMEM_START, VIRTIO_PMEM_ALIGNMENT)
        );
        assert_eq!(
            prepared.as_slice()[0].config_space(),
            VirtioPmemConfigSpace::new(TEST_PMEM_START, VIRTIO_PMEM_ALIGNMENT)
        );
        assert_eq!(
            prepared.as_slice()[0].config_space().to_le_bytes(),
            VirtioPmemConfigSpace::new(TEST_PMEM_START, VIRTIO_PMEM_ALIGNMENT).to_le_bytes()
        );
        assert_eq!(prepared.as_slice()[1].id(), "pmem1");
        assert_eq!(prepared.as_slice()[1].backing().len(), 6);
        assert!(prepared.as_slice()[1].backing().is_read_only());
        assert_eq!(prepared.as_slice()[1].mapping().file_len(), 6);
        assert_eq!(
            prepared.as_slice()[1].mapping().mapped_len(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert!(prepared.as_slice()[1].mapping().is_read_only());
        assert_eq!(
            prepared.as_slice()[1].guest_range(),
            guest_range(
                TEST_PMEM_START + VIRTIO_PMEM_ALIGNMENT,
                VIRTIO_PMEM_ALIGNMENT
            )
        );
        assert_eq!(
            prepared.as_slice()[1].config_space(),
            VirtioPmemConfigSpace::new(
                TEST_PMEM_START + VIRTIO_PMEM_ALIGNMENT,
                VIRTIO_PMEM_ALIGNMENT,
            )
        );
    }

    #[test]
    fn prepared_devices_adopt_provided_backing_without_opening_configured_path() {
        let source = temp_file("provided-pmem-source.img", b"provided-pmem");
        let missing = missing_path("provided-pmem-missing.img");
        let configs = [pmem_config(PmemConfigInput::new(
            "pmem0",
            missing.to_string_lossy().into_owned(),
        ))];
        let provided_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(source.as_path())
            .expect("provided pmem backing should open");
        let provided = PmemFileBacking::from_file(provided_file, false)
            .expect("provided pmem backing should validate");
        let mut backings = BTreeMap::new();
        backings.insert("pmem0".to_string(), provided);
        let layout = guest_layout(vec![guest_range(aarch64::DRAM_MEM_START, 8 * 1024 * 1024)]);

        let prepared = PreparedPmemDevices::from_config_slice_with_layout_and_backings(
            &configs, &layout, backings,
        )
        .expect("provided pmem backing should prepare without configured path");
        let rendered = format!("{prepared:?}");

        assert_eq!(prepared.as_slice()[0].backing().len(), 13);
        assert!(!prepared.as_slice()[0].backing().is_read_only());
        assert_eq!(prepared.as_slice()[0].mapping().file_len(), 13);
        assert!(!missing.exists());
        assert!(rendered.contains("<owned>"));
        assert!(!rendered.contains(source.as_path().to_string_lossy().as_ref()));
    }

    #[test]
    fn prepared_devices_reject_provided_backing_with_mismatched_read_only_mode() {
        let source = temp_file("provided-pmem-mode-mismatch.img", b"provided");
        let missing = missing_path("provided-pmem-mode-mismatch-missing.img");
        let configs = [pmem_config(
            PmemConfigInput::new("pmem0", missing.to_string_lossy().into_owned())
                .with_read_only(true),
        )];
        let provided = PmemFileBacking::from_file(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(source.as_path())
                .expect("provided pmem backing should open"),
            false,
        )
        .expect("provided pmem backing should validate");
        let mut backings = BTreeMap::new();
        backings.insert("pmem0".to_string(), provided);
        let layout = guest_layout(vec![guest_range(aarch64::DRAM_MEM_START, 8 * 1024 * 1024)]);

        let err = PreparedPmemDevices::from_config_slice_with_layout_and_backings(
            &configs, &layout, backings,
        )
        .expect_err("mismatched provided backing mode should fail");

        assert!(matches!(
            err,
            PreparedPmemDeviceError::BackingModeMismatch { ref pmem_id }
                if pmem_id == "pmem0"
        ));
        assert!(
            !err.to_string()
                .contains(source.as_path().to_string_lossy().as_ref())
        );
        assert!(!err.to_string().contains(missing.to_string_lossy().as_ref()));
        assert!(!missing.exists());
    }

    #[test]
    fn prepared_devices_reject_provided_backing_without_matching_config() {
        let source = temp_file("provided-pmem-unexpected.img", b"provided");
        let provided = PmemFileBacking::from_file(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(source.as_path())
                .expect("provided pmem backing should open"),
            false,
        )
        .expect("provided pmem backing should validate");
        let mut backings = BTreeMap::new();
        backings.insert("missing".to_string(), provided);
        let layout = guest_layout(vec![guest_range(aarch64::DRAM_MEM_START, 8 * 1024 * 1024)]);

        let err =
            PreparedPmemDevices::from_config_slice_with_layout_and_backings(&[], &layout, backings)
                .expect_err("unexpected pmem backing should fail");

        assert!(matches!(err, PreparedPmemDeviceError::UnexpectedBacking));
        assert_eq!(
            err.to_string(),
            "provided pmem backing does not match a configured device"
        );
    }

    #[test]
    fn pmem_debug_redacts_configured_paths_and_grant_references() {
        let input =
            PmemConfigInput::new("pmem0", "bangbang-grant:secret-pmem-grant").with_read_only(true);
        let config = PmemConfig::try_from(input.clone()).expect("pmem should validate");

        for rendered in [
            format!("{input:?}"),
            format!("{config:?}"),
            format!("{:?}", crate::VmmAction::PutPmem(input)),
        ] {
            assert!(rendered.contains("<redacted>"));
            assert!(!rendered.contains("secret-pmem"));
        }
    }

    #[test]
    fn prepared_devices_report_id_without_echoing_path() {
        let valid = temp_file("valid-pmem.img", b"valid");
        let missing = missing_path("secret-prepared-pmem.img");
        let configs = [
            pmem_config(PmemConfigInput::new(
                "pmem0",
                valid.as_path().to_string_lossy().into_owned(),
            )),
            pmem_config(PmemConfigInput::new(
                "pmem1",
                missing.to_string_lossy().into_owned(),
            )),
        ];

        let err = PreparedPmemDevices::from_config_slice(&configs)
            .expect_err("missing pmem backing should fail preparation");

        assert!(matches!(
            err,
            PreparedPmemDeviceError::OpenBacking {
                ref pmem_id,
                source: PmemFileBackingError::OpenFile { .. },
            } if pmem_id == "pmem1"
        ));
        assert!(err.to_string().contains("pmem1"));
        assert!(!err.to_string().contains("secret-prepared-pmem"));
    }

    #[test]
    fn prepared_devices_cleanup_previous_mappings_after_later_map_failure() {
        let first = temp_file("first-cleanup-pmem.img", b"first");
        let second = temp_file("second-cleanup-pmem.img", b"second");
        let configs = [
            pmem_config(PmemConfigInput::new(
                "pmem0",
                first.as_path().to_string_lossy().into_owned(),
            )),
            pmem_config(PmemConfigInput::new(
                "pmem1",
                second.as_path().to_string_lossy().into_owned(),
            )),
        ];
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = ScriptedPmemBackingMapper::new(Arc::clone(&drop_count), 2);

        let err = PreparedPmemDevices::from_config_slice_with_mapper(&configs, &mut mapper)
            .expect_err("second pmem mapping should fail");

        assert!(matches!(
            err,
            PreparedPmemDeviceError::MapBacking {
                ref pmem_id,
                source: PmemBackingMappingError::MapFile { .. },
            } if pmem_id == "pmem1"
        ));
        assert_eq!(mapper.calls, 2);
        assert_eq!(drop_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn prepared_devices_cleanup_mappings_after_later_guest_range_failure() {
        let first = temp_sized_file("first-range-cleanup-pmem.img", VIRTIO_PMEM_ALIGNMENT);
        let second = temp_sized_file("second-range-cleanup-pmem.img", VIRTIO_PMEM_ALIGNMENT);
        let configs = [
            pmem_config(PmemConfigInput::new(
                "pmem0",
                first.as_path().to_string_lossy().into_owned(),
            )),
            pmem_config(PmemConfigInput::new(
                "pmem1",
                second.as_path().to_string_lossy().into_owned(),
            )),
        ];
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = ScriptedPmemBackingMapper::new(Arc::clone(&drop_count), usize::MAX);
        let start = u64::MAX - (VIRTIO_PMEM_ALIGNMENT * 2) + 1;
        let mut allocator = PmemGuestRangeAllocator::with_start_for_test(start, &[]);

        let err = PreparedPmemDevices::from_config_slice_with_mapper_and_allocator(
            &configs,
            &mut mapper,
            &mut allocator,
        )
        .expect_err("second pmem guest range should fail");

        assert!(matches!(
            err,
            PreparedPmemDeviceError::AllocateGuestRange {
                ref pmem_id,
                source: PmemGuestRangeAllocationError::InvalidRange { .. },
            } if pmem_id == "pmem1"
        ));
        assert_eq!(mapper.calls, 2);
        assert_eq!(drop_count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn prepared_devices_report_mapping_error_without_echoing_path() {
        let file = temp_file("secret-map-failure-pmem.img", b"pmem");
        let configs = [pmem_config(PmemConfigInput::new(
            "pmem0",
            file.as_path().to_string_lossy().into_owned(),
        ))];
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = ScriptedPmemBackingMapper::new(drop_count, 1);

        let err = PreparedPmemDevices::from_config_slice_with_mapper(&configs, &mut mapper)
            .expect_err("pmem mapping should fail");

        assert!(matches!(
            err,
            PreparedPmemDeviceError::MapBacking {
                ref pmem_id,
                source: PmemBackingMappingError::MapFile { .. },
            } if pmem_id == "pmem0"
        ));
        assert!(err.to_string().contains("pmem0"));
        assert!(!err.to_string().contains("secret-map-failure-pmem"));
    }
}
