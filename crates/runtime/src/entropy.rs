use std::collections::TryReserveError;
use std::fmt;

use crate::interrupt::DeviceInterruptKind;
use crate::memory::{GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError};
use crate::mmio::{
    MmioBusError, MmioDispatchError, MmioDispatcher, MmioHandlerError, MmioRegion, MmioRegionId,
};
use crate::virtio_mmio::{
    UnsupportedVirtioMmioDeviceConfig, VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation,
    VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
    VirtioMmioQueueRegisterError, VirtioMmioQueueState, VirtioMmioRegisterHandler,
    VirtioMmioRegisterHandlerError,
};
use crate::virtio_queue::{
    VirtqueueAvailableRing, VirtqueueAvailableRingError, VirtqueueDescriptor,
    VirtqueueDescriptorChain, VirtqueueNotificationSuppression, VirtqueueUsedRing,
    VirtqueueUsedRingError, VirtqueueUsedRingPublication,
};

pub const VIRTIO_RNG_DEVICE_ID: u32 = 4;
pub const VIRTIO_RNG_QUEUE_INDEX: u16 = 0;
pub const VIRTIO_RNG_QUEUE_COUNT: usize = 1;
pub const VIRTIO_RNG_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_RNG_QUEUE_SIZES: [u16; VIRTIO_RNG_QUEUE_COUNT] = [VIRTIO_RNG_QUEUE_SIZE];
pub const VIRTIO_RNG_MAX_REQUEST_BYTES: usize = 64 * 1024;

pub type VirtioRngMmioHandler =
    VirtioMmioRegisterHandler<UnsupportedVirtioMmioDeviceConfig, VirtioRngDevice>;

const VIRTIO_RNG_MAX_REQUEST_BYTES_U64: u64 = 64 * 1024;
const VIRTIO_RNG_QUEUE_INDEX_U32: u32 = 0;
const VIRTIO_RNG_QUEUE_INDEX_USIZE: usize = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntropyConfigInput {
    rate_limiter_configured: bool,
}

impl EntropyConfigInput {
    pub const fn new() -> Self {
        Self {
            rate_limiter_configured: false,
        }
    }

    pub const fn with_rate_limiter_configured(mut self) -> Self {
        self.rate_limiter_configured = true;
        self
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
    }
}

impl Default for EntropyConfigInput {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EntropyConfig;

impl EntropyConfig {
    pub const fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntropyConfigError {
    UnsupportedRateLimiter,
}

impl fmt::Display for EntropyConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRateLimiter => f.write_str("entropy rate_limiter is not supported"),
        }
    }
}

impl std::error::Error for EntropyConfigError {}

#[derive(Debug, Default)]
pub struct PreparedEntropyDevice {
    device: VirtioRngDevice,
}

impl PreparedEntropyDevice {
    pub const fn new() -> Self {
        Self {
            device: VirtioRngDevice::new(),
        }
    }

    pub const fn device(&self) -> &VirtioRngDevice {
        &self.device
    }

    pub fn into_device(self) -> VirtioRngDevice {
        self.device
    }

    pub fn register_mmio(
        self,
        layout: EntropyMmioLayout,
    ) -> Result<EntropyMmioDevice, EntropyMmioRegistrationError> {
        EntropyMmioDevice::from_prepared(self, layout)
    }

    pub fn register_mmio_with_dispatcher(
        self,
        layout: EntropyMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<EntropyMmioDevice, EntropyMmioRegistrationError> {
        EntropyMmioDevice::from_prepared_with_dispatcher(self, layout, dispatcher)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntropyMmioLayout {
    address: GuestAddress,
    region_id: MmioRegionId,
}

impl EntropyMmioLayout {
    pub const fn new(address: GuestAddress, region_id: MmioRegionId) -> Self {
        Self { address, region_id }
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region_id
    }

    fn region(self) -> Result<MmioRegion, EntropyMmioRegistrationError> {
        MmioRegion::new(self.region_id, self.address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE).map_err(
            |source| EntropyMmioRegistrationError::InvalidRegion {
                region_id: self.region_id,
                address: self.address,
                source,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntropyMmioDeviceRegistration {
    region: MmioRegion,
}

impl EntropyMmioDeviceRegistration {
    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    pub const fn region_id(&self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn address(&self) -> GuestAddress {
        self.region.range().start()
    }
}

#[derive(Debug)]
pub struct EntropyMmioDevice {
    dispatcher: MmioDispatcher,
    registration: EntropyMmioDeviceRegistration,
}

impl EntropyMmioDevice {
    pub fn from_prepared(
        prepared: PreparedEntropyDevice,
        layout: EntropyMmioLayout,
    ) -> Result<Self, EntropyMmioRegistrationError> {
        Self::from_prepared_with_dispatcher(prepared, layout, MmioDispatcher::new())
    }

    pub fn from_prepared_with_dispatcher(
        prepared: PreparedEntropyDevice,
        layout: EntropyMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<Self, EntropyMmioRegistrationError> {
        let region = layout.region()?;
        let handler = VirtioRngMmioHandler::with_activation(
            VIRTIO_RNG_DEVICE_ID,
            0,
            &VIRTIO_RNG_QUEUE_SIZES,
            prepared.into_device(),
        )
        .map_err(|source| EntropyMmioRegistrationError::BuildHandler {
            region_id: layout.region_id(),
            source,
        })?;
        let mut dispatcher = dispatcher;
        let inserted_region = dispatcher
            .insert_region(
                layout.region_id(),
                layout.address(),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .map_err(|source| EntropyMmioRegistrationError::InsertRegion {
                region_id: layout.region_id(),
                address: layout.address(),
                source,
            })?;
        dispatcher
            .register_handler(layout.region_id(), handler)
            .map_err(|source| EntropyMmioRegistrationError::RegisterHandler {
                region_id: layout.region_id(),
                source,
            })?;
        debug_assert_eq!(inserted_region, region);

        Ok(Self {
            dispatcher,
            registration: EntropyMmioDeviceRegistration { region },
        })
    }

    pub fn dispatcher(&self) -> &MmioDispatcher {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut MmioDispatcher {
        &mut self.dispatcher
    }

    pub const fn registration(&self) -> &EntropyMmioDeviceRegistration {
        &self.registration
    }

    pub fn into_parts(self) -> (MmioDispatcher, EntropyMmioDeviceRegistration) {
        (self.dispatcher, self.registration)
    }
}

#[derive(Debug)]
pub enum EntropyMmioRegistrationError {
    InvalidRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: GuestMemoryError,
    },
    BuildHandler {
        region_id: MmioRegionId,
        source: VirtioMmioRegisterHandlerError,
    },
    InsertRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for EntropyMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "invalid entropy MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::BuildHandler { region_id, source } => {
                write!(
                    f,
                    "failed to build entropy MMIO handler for region id={region_id}: {source}"
                )
            }
            Self::InsertRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "failed to insert entropy MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::RegisterHandler { region_id, source } => {
                write!(
                    f,
                    "failed to register entropy MMIO handler for region id={region_id}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for EntropyMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRegion { source, .. } => Some(source),
            Self::BuildHandler { source, .. } => Some(source),
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
        }
    }
}

pub trait VirtioRngEntropySource {
    fn fill_entropy(&mut self, destination: &mut [u8]) -> Result<(), VirtioRngEntropySourceError>;
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct VirtioRngOsEntropySource;

impl VirtioRngOsEntropySource {
    pub const fn new() -> Self {
        Self
    }
}

impl VirtioRngEntropySource for VirtioRngOsEntropySource {
    fn fill_entropy(&mut self, destination: &mut [u8]) -> Result<(), VirtioRngEntropySourceError> {
        getrandom::fill(destination).map_err(|_| VirtioRngEntropySourceError::new())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioRngEntropySourceError;

impl VirtioRngEntropySourceError {
    pub const fn new() -> Self {
        Self
    }
}

impl fmt::Display for VirtioRngEntropySourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("virtio-rng entropy source failed")
    }
}

impl std::error::Error for VirtioRngEntropySourceError {}

#[derive(Debug)]
pub enum VirtioRngQueueBuildError {
    QueueNotReady,
    AvailableRing { source: VirtqueueAvailableRingError },
    UsedRing { source: VirtqueueUsedRingError },
}

impl fmt::Display for VirtioRngQueueBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("virtio-rng queue is not ready"),
            Self::AvailableRing { source } => {
                write!(
                    f,
                    "failed to build virtio-rng available ring from queue state: {source}"
                )
            }
            Self::UsedRing { source } => {
                write!(
                    f,
                    "failed to build virtio-rng used ring from queue state: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioRngQueueBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source } => Some(source),
            Self::QueueNotReady => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioRngQueue {
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
}

impl VirtioRngQueue {
    pub const fn new(available: VirtqueueAvailableRing, used: VirtqueueUsedRing) -> Self {
        Self { available, used }
    }

    pub fn from_mmio_queue_state(
        queue: &VirtioMmioQueueState,
    ) -> Result<Self, VirtioRngQueueBuildError> {
        if !queue.ready() {
            return Err(VirtioRngQueueBuildError::QueueNotReady);
        }

        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioRngQueueBuildError::AvailableRing { source })?;
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioRngQueueBuildError::UsedRing { source })?;

        Ok(Self { available, used })
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
    }

    pub fn dispatch_with_source(
        &mut self,
        memory: &mut GuestMemory,
        entropy_source: &mut (impl VirtioRngEntropySource + ?Sized),
    ) -> Result<VirtioRngQueueDispatch, VirtioRngQueueDispatchError> {
        let mut dispatch = VirtioRngQueueDispatch::default();
        let mut entropy_buffer = Vec::new();

        while let Some(chain) = match self.available.pop_descriptor_chain(memory) {
            Ok(chain) => chain,
            Err(source) => {
                return Err(VirtioRngQueueDispatchError::AvailableRing {
                    completed_dispatch: Box::new(dispatch),
                    source,
                });
            }
        } {
            let descriptor_head = match descriptor_chain_head(&chain) {
                Some(descriptor_head) => descriptor_head,
                None => {
                    return Err(VirtioRngQueueDispatchError::EmptyDescriptorChain {
                        completed_dispatch: Box::new(dispatch),
                    });
                }
            };

            let (bytes_written_to_guest, outcome) = match VirtioRngBuffer::parse(memory, &chain) {
                Ok(buffer) => {
                    match fill_entropy_buffer(memory, &buffer, entropy_source, &mut entropy_buffer)
                    {
                        Ok(bytes_written_to_guest) => (
                            bytes_written_to_guest,
                            VirtioRngQueueDispatchOutcome::Filled {
                                bytes_written_to_guest,
                            },
                        ),
                        Err(VirtioRngFillError::EntropyBufferAllocation {
                            requested_len,
                            source,
                        }) => {
                            return Err(VirtioRngQueueDispatchError::EntropyBufferAllocation {
                                completed_dispatch: Box::new(dispatch),
                                requested_len,
                                source,
                            });
                        }
                        Err(VirtioRngFillError::CompletedLengthTooLarge { len }) => {
                            return Err(VirtioRngQueueDispatchError::CompletedLengthTooLarge {
                                completed_dispatch: Box::new(dispatch),
                                len,
                            });
                        }
                        Err(VirtioRngFillError::Source(source)) => {
                            (0, VirtioRngQueueDispatchOutcome::SourceError(source))
                        }
                        Err(VirtioRngFillError::BufferWrite(source)) => {
                            return Err(VirtioRngQueueDispatchError::BufferWrite {
                                completed_dispatch: Box::new(dispatch),
                                descriptor_head,
                                source,
                            });
                        }
                    }
                }
                Err(source) => (0, VirtioRngQueueDispatchOutcome::BufferParseError(source)),
            };

            let publication = match self.used.publish_used_element_with_notification(
                memory,
                descriptor_head,
                bytes_written_to_guest,
                VirtqueueNotificationSuppression::Disabled,
            ) {
                Ok(publication) => publication,
                Err(source) => {
                    return Err(VirtioRngQueueDispatchError::UsedRing {
                        completed_dispatch: Box::new(dispatch),
                        descriptor_head,
                        bytes_written_to_guest,
                        source,
                    });
                }
            };

            dispatch.record(outcome, publication);
        }

        Ok(dispatch)
    }
}

#[derive(Debug, Default)]
pub struct VirtioRngDevice {
    active_queue: Option<VirtioRngQueue>,
}

impl VirtioRngDevice {
    pub const fn new() -> Self {
        Self { active_queue: None }
    }

    pub fn is_activated(&self) -> bool {
        self.active_queue.is_some()
    }

    pub fn active_queue(&self) -> Option<&VirtioRngQueue> {
        self.active_queue.as_ref()
    }

    pub fn active_queue_mut(&mut self) -> Option<&mut VirtioRngQueue> {
        self.active_queue.as_mut()
    }

    pub fn activate_rng(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioRngDeviceActivationError> {
        if self.active_queue.is_some() {
            return Err(VirtioRngDeviceActivationError::AlreadyActive);
        }
        let queue_count = activation.queue_count();
        if queue_count != VIRTIO_RNG_QUEUE_COUNT {
            return Err(VirtioRngDeviceActivationError::QueueCountMismatch {
                expected: VIRTIO_RNG_QUEUE_COUNT,
                actual: queue_count,
            });
        }

        let queue_index = VIRTIO_RNG_QUEUE_INDEX_U32;
        let queue = activation
            .queue(queue_index)
            .map_err(|source| VirtioRngDeviceActivationError::QueueMetadata {
                queue_index,
                source,
            })
            .and_then(|queue| {
                VirtioRngQueue::from_mmio_queue_state(queue).map_err(|source| {
                    VirtioRngDeviceActivationError::QueueBuild {
                        queue_index,
                        source,
                    }
                })
            })?;
        self.active_queue = Some(queue);

        Ok(())
    }

    fn dispatch_drained_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
        entropy_source: &mut (impl VirtioRngEntropySource + ?Sized),
    ) -> Result<VirtioRngDeviceNotificationDispatch, VirtioRngDeviceNotificationError> {
        if drained_notifications.is_empty() {
            return Ok(VirtioRngDeviceNotificationDispatch::new(
                drained_notifications,
                None,
            ));
        }

        if let Some(queue_index) = drained_notifications
            .iter()
            .copied()
            .find(|queue_index| *queue_index != VIRTIO_RNG_QUEUE_INDEX_USIZE)
        {
            return Err(VirtioRngDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        let Some(queue) = self.active_queue.as_mut() else {
            return Err(VirtioRngDeviceNotificationError::Inactive {
                drained_notifications,
            });
        };

        match queue.dispatch_with_source(memory, entropy_source) {
            Ok(dispatch) => Ok(VirtioRngDeviceNotificationDispatch::new(
                drained_notifications,
                Some(dispatch),
            )),
            Err(source) => Err(VirtioRngDeviceNotificationError::QueueDispatch {
                drained_notifications,
                source,
            }),
        }
    }

    pub fn reset(&mut self) {
        self.active_queue = None;
    }
}

impl VirtioMmioRegisterHandler<UnsupportedVirtioMmioDeviceConfig, VirtioRngDevice> {
    pub fn dispatch_rng_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        entropy_source: &mut (impl VirtioRngEntropySource + ?Sized),
    ) -> Result<VirtioRngDeviceNotificationDispatch, VirtioRngDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        let dispatch = self
            .activation_handler_mut()
            .dispatch_drained_queue_notifications(memory, drained_notifications, entropy_source);
        let needs_queue_interrupt = match &dispatch {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) => error
                .completed_dispatch()
                .is_some_and(VirtioRngQueueDispatch::needs_queue_interrupt),
        };
        if needs_queue_interrupt {
            self.mark_interrupt_pending(DeviceInterruptKind::Queue);
        }

        dispatch
    }
}

impl VirtioMmioDeviceActivationHandler for VirtioRngDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        self.activate_rng(activation).map_err(Into::into)
    }

    fn reset(&mut self) {
        VirtioRngDevice::reset(self);
    }
}

#[derive(Debug)]
pub enum VirtioRngDeviceActivationError {
    AlreadyActive,
    QueueCountMismatch {
        expected: usize,
        actual: usize,
    },
    QueueMetadata {
        queue_index: u32,
        source: VirtioMmioQueueRegisterError,
    },
    QueueBuild {
        queue_index: u32,
        source: VirtioRngQueueBuildError,
    },
}

impl fmt::Display for VirtioRngDeviceActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("virtio-rng device is already active"),
            Self::QueueCountMismatch { expected, actual } => {
                write!(
                    f,
                    "virtio-rng device requires {expected} queue(s), got {actual}"
                )
            }
            Self::QueueMetadata {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to read virtio-rng queue {queue_index} activation metadata: {source}"
                )
            }
            Self::QueueBuild {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to activate virtio-rng queue {queue_index}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioRngDeviceActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueMetadata { source, .. } => Some(source),
            Self::QueueBuild { source, .. } => Some(source),
            Self::AlreadyActive | Self::QueueCountMismatch { .. } => None,
        }
    }
}

impl From<VirtioRngDeviceActivationError> for VirtioMmioDeviceActivationError {
    fn from(source: VirtioRngDeviceActivationError) -> Self {
        MmioHandlerError::new(source.to_string()).into()
    }
}

#[derive(Debug)]
pub struct VirtioRngDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
    queue_dispatch: Option<VirtioRngQueueDispatch>,
}

impl VirtioRngDeviceNotificationDispatch {
    fn new(
        drained_notifications: Vec<usize>,
        queue_dispatch: Option<VirtioRngQueueDispatch>,
    ) -> Self {
        Self {
            drained_notifications,
            queue_dispatch,
        }
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub const fn queue_dispatch(&self) -> Option<&VirtioRngQueueDispatch> {
        self.queue_dispatch.as_ref()
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.queue_dispatch
            .as_ref()
            .is_some_and(VirtioRngQueueDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub enum VirtioRngDeviceNotificationError {
    Inactive {
        drained_notifications: Vec<usize>,
    },
    UnsupportedQueue {
        drained_notifications: Vec<usize>,
        queue_index: usize,
    },
    QueueDispatch {
        drained_notifications: Vec<usize>,
        source: VirtioRngQueueDispatchError,
    },
}

impl VirtioRngDeviceNotificationError {
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

    pub const fn completed_dispatch(&self) -> Option<&VirtioRngQueueDispatch> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source.completed_dispatch()),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

impl fmt::Display for VirtioRngDeviceNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive { .. } => {
                f.write_str("virtio-rng queue notification cannot be dispatched before activation")
            }
            Self::UnsupportedQueue { queue_index, .. } => {
                write!(
                    f,
                    "virtio-rng queue notification for unsupported queue {queue_index}"
                )
            }
            Self::QueueDispatch { source, .. } => {
                write!(
                    f,
                    "failed to dispatch virtio-rng queue notification: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioRngDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct VirtioRngQueueDispatch {
    processed_requests: usize,
    successful_requests: usize,
    buffer_parse_failures: usize,
    source_failures: usize,
    bytes_written_to_guest: u64,
    first_buffer_parse_failure: Option<VirtioRngBufferParseError>,
    first_source_failure: Option<VirtioRngEntropySourceError>,
    needs_queue_interrupt: bool,
}

impl VirtioRngQueueDispatch {
    pub const fn processed_requests(&self) -> usize {
        self.processed_requests
    }

    pub const fn successful_requests(&self) -> usize {
        self.successful_requests
    }

    pub const fn buffer_parse_failures(&self) -> usize {
        self.buffer_parse_failures
    }

    pub const fn source_failures(&self) -> usize {
        self.source_failures
    }

    pub const fn bytes_written_to_guest(&self) -> u64 {
        self.bytes_written_to_guest
    }

    pub const fn first_buffer_parse_failure(&self) -> Option<&VirtioRngBufferParseError> {
        self.first_buffer_parse_failure.as_ref()
    }

    pub const fn first_source_failure(&self) -> Option<VirtioRngEntropySourceError> {
        self.first_source_failure
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.needs_queue_interrupt
    }

    fn record(
        &mut self,
        outcome: VirtioRngQueueDispatchOutcome,
        publication: VirtqueueUsedRingPublication,
    ) {
        self.processed_requests += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        match outcome {
            VirtioRngQueueDispatchOutcome::Filled {
                bytes_written_to_guest,
            } => {
                self.successful_requests += 1;
                self.bytes_written_to_guest += u64::from(bytes_written_to_guest);
            }
            VirtioRngQueueDispatchOutcome::BufferParseError(source) => {
                self.buffer_parse_failures += 1;
                if self.first_buffer_parse_failure.is_none() {
                    self.first_buffer_parse_failure = Some(source);
                }
            }
            VirtioRngQueueDispatchOutcome::SourceError(source) => {
                self.source_failures += 1;
                if self.first_source_failure.is_none() {
                    self.first_source_failure = Some(source);
                }
            }
        }
    }
}

#[derive(Debug)]
enum VirtioRngQueueDispatchOutcome {
    Filled { bytes_written_to_guest: u32 },
    BufferParseError(VirtioRngBufferParseError),
    SourceError(VirtioRngEntropySourceError),
}

#[derive(Debug)]
pub enum VirtioRngQueueDispatchError {
    EntropyBufferAllocation {
        completed_dispatch: Box<VirtioRngQueueDispatch>,
        requested_len: usize,
        source: TryReserveError,
    },
    CompletedLengthTooLarge {
        completed_dispatch: Box<VirtioRngQueueDispatch>,
        len: usize,
    },
    AvailableRing {
        completed_dispatch: Box<VirtioRngQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain {
        completed_dispatch: Box<VirtioRngQueueDispatch>,
    },
    UsedRing {
        completed_dispatch: Box<VirtioRngQueueDispatch>,
        descriptor_head: u16,
        bytes_written_to_guest: u32,
        source: VirtqueueUsedRingError,
    },
    BufferWrite {
        completed_dispatch: Box<VirtioRngQueueDispatch>,
        descriptor_head: u16,
        source: VirtioRngBufferWriteError,
    },
}

impl VirtioRngQueueDispatchError {
    pub const fn completed_dispatch(&self) -> &VirtioRngQueueDispatch {
        match self {
            Self::EntropyBufferAllocation {
                completed_dispatch, ..
            }
            | Self::CompletedLengthTooLarge {
                completed_dispatch, ..
            }
            | Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            }
            | Self::BufferWrite {
                completed_dispatch, ..
            } => completed_dispatch,
        }
    }
}

impl fmt::Display for VirtioRngQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntropyBufferAllocation {
                requested_len,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to reserve {requested_len} bytes for virtio-rng entropy: {source}"
                )
            }
            Self::CompletedLengthTooLarge { len, .. } => {
                write!(
                    f,
                    "virtio-rng completed entropy length {len} exceeds used-ring length field"
                )
            }
            Self::AvailableRing { source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-rng available descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain { .. } => {
                f.write_str("virtio-rng queue produced an empty descriptor chain")
            }
            Self::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to publish virtio-rng used descriptor head {descriptor_head} with length {bytes_written_to_guest}: {source}"
                )
            }
            Self::BufferWrite {
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to write virtio-rng entropy into descriptor head {descriptor_head}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioRngQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::EntropyBufferAllocation { source, .. } => Some(source),
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::BufferWrite { source, .. } => Some(source),
            Self::CompletedLengthTooLarge { .. } | Self::EmptyDescriptorChain { .. } => None,
        }
    }
}

#[derive(Debug)]
struct VirtioRngBuffer {
    len: u64,
    segments: Vec<VirtioRngBufferSegment>,
}

impl VirtioRngBuffer {
    fn parse(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioRngBufferParseError> {
        let mut segments = Vec::new();
        segments.try_reserve_exact(chain.len()).map_err(|source| {
            VirtioRngBufferParseError::BufferSegmentsAllocationFailed {
                descriptor_count: chain.len(),
                source,
            }
        })?;

        let mut len = 0_u64;
        for descriptor in chain.descriptors() {
            let segment = VirtioRngBufferSegment::parse(memory, *descriptor)?;
            len = len.checked_add(segment.len()).ok_or(
                VirtioRngBufferParseError::BufferLengthOverflow {
                    current: len,
                    len: descriptor.len(),
                },
            )?;
            segments.push(segment);
        }

        Ok(Self { len, segments })
    }

    const fn len(&self) -> u64 {
        self.len
    }
}

#[derive(Debug)]
struct VirtioRngBufferSegment {
    descriptor_index: u16,
    address: GuestAddress,
    len: u32,
}

impl VirtioRngBufferSegment {
    fn parse(
        memory: &GuestMemory,
        descriptor: VirtqueueDescriptor,
    ) -> Result<Self, VirtioRngBufferParseError> {
        if !descriptor.is_write_only() {
            return Err(VirtioRngBufferParseError::BufferDescriptorReadOnly {
                index: descriptor.index(),
            });
        }
        if descriptor.is_empty() {
            return Err(VirtioRngBufferParseError::BufferDescriptorEmpty {
                index: descriptor.index(),
            });
        }

        let range =
            crate::memory::GuestMemoryRange::new(descriptor.address(), u64::from(descriptor.len()))
                .map_err(|source| VirtioRngBufferParseError::BufferDescriptorRange {
                    index: descriptor.index(),
                    address: descriptor.address(),
                    len: descriptor.len(),
                    source,
                })?;
        memory.validate_mapped_range(range).map_err(|source| {
            VirtioRngBufferParseError::BufferDescriptorAccess {
                index: descriptor.index(),
                address: descriptor.address(),
                len: descriptor.len(),
                source,
            }
        })?;

        Ok(Self {
            descriptor_index: descriptor.index(),
            address: descriptor.address(),
            len: descriptor.len(),
        })
    }

    fn len(&self) -> u64 {
        u64::from(self.len)
    }
}

#[derive(Debug)]
pub enum VirtioRngBufferParseError {
    BufferDescriptorReadOnly {
        index: u16,
    },
    BufferDescriptorEmpty {
        index: u16,
    },
    BufferDescriptorRange {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryError,
    },
    BufferDescriptorAccess {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    BufferLengthOverflow {
        current: u64,
        len: u32,
    },
    BufferSegmentsAllocationFailed {
        descriptor_count: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtioRngBufferParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferDescriptorReadOnly { index } => {
                write!(f, "virtio-rng buffer descriptor {index} is read-only")
            }
            Self::BufferDescriptorEmpty { index } => {
                write!(f, "virtio-rng buffer descriptor {index} is empty")
            }
            Self::BufferDescriptorRange {
                index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "virtio-rng buffer descriptor {index} at {address} with length {len} is invalid: {source}"
                )
            }
            Self::BufferDescriptorAccess {
                index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "virtio-rng buffer descriptor {index} at {address} with length {len} is not fully mapped: {source}"
                )
            }
            Self::BufferLengthOverflow { current, len } => {
                write!(
                    f,
                    "virtio-rng buffer length {current} overflows when adding descriptor length {len}"
                )
            }
            Self::BufferSegmentsAllocationFailed {
                descriptor_count,
                source,
            } => {
                write!(
                    f,
                    "failed to reserve virtio-rng buffer metadata for {descriptor_count} descriptors: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioRngBufferParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BufferDescriptorRange { source, .. } => Some(source),
            Self::BufferDescriptorAccess { source, .. } => Some(source),
            Self::BufferSegmentsAllocationFailed { source, .. } => Some(source),
            Self::BufferDescriptorReadOnly { .. }
            | Self::BufferDescriptorEmpty { .. }
            | Self::BufferLengthOverflow { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioRngBufferWriteError {
    SegmentWrite {
        descriptor_index: u16,
        address: GuestAddress,
        len: usize,
        source: GuestMemoryAccessError,
    },
    IncompleteBufferWrite {
        remaining_bytes: usize,
    },
}

impl fmt::Display for VirtioRngBufferWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SegmentWrite {
                descriptor_index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "failed to write {len} bytes into virtio-rng buffer descriptor {descriptor_index} at {address}: {source}"
                )
            }
            Self::IncompleteBufferWrite { remaining_bytes } => {
                write!(
                    f,
                    "virtio-rng buffer write finished with {remaining_bytes} entropy bytes remaining"
                )
            }
        }
    }
}

impl std::error::Error for VirtioRngBufferWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SegmentWrite { source, .. } => Some(source),
            Self::IncompleteBufferWrite { .. } => None,
        }
    }
}

#[derive(Debug)]
enum VirtioRngFillError {
    EntropyBufferAllocation {
        requested_len: usize,
        source: TryReserveError,
    },
    CompletedLengthTooLarge {
        len: usize,
    },
    Source(VirtioRngEntropySourceError),
    BufferWrite(VirtioRngBufferWriteError),
}

fn fill_entropy_buffer(
    memory: &mut GuestMemory,
    buffer: &VirtioRngBuffer,
    entropy_source: &mut (impl VirtioRngEntropySource + ?Sized),
    entropy_buffer: &mut Vec<u8>,
) -> Result<u32, VirtioRngFillError> {
    let requested_len = entropy_request_len(buffer.len())?;
    if entropy_buffer.capacity() < requested_len {
        entropy_buffer
            .try_reserve_exact(requested_len - entropy_buffer.capacity())
            .map_err(|source| VirtioRngFillError::EntropyBufferAllocation {
                requested_len,
                source,
            })?;
    }
    entropy_buffer.clear();
    entropy_buffer.resize(requested_len, 0);

    entropy_source
        .fill_entropy(entropy_buffer)
        .map_err(VirtioRngFillError::Source)?;
    write_entropy_to_buffer(memory, buffer, entropy_buffer)
        .map_err(VirtioRngFillError::BufferWrite)?;

    u32::try_from(requested_len)
        .map_err(|_| VirtioRngFillError::CompletedLengthTooLarge { len: requested_len })
}

fn entropy_request_len(buffer_len: u64) -> Result<usize, VirtioRngFillError> {
    if buffer_len > VIRTIO_RNG_MAX_REQUEST_BYTES_U64 {
        return Ok(VIRTIO_RNG_MAX_REQUEST_BYTES);
    }

    usize::try_from(buffer_len)
        .map_err(|_| VirtioRngFillError::CompletedLengthTooLarge { len: usize::MAX })
}

fn write_entropy_to_buffer(
    memory: &mut GuestMemory,
    buffer: &VirtioRngBuffer,
    entropy: &[u8],
) -> Result<(), VirtioRngBufferWriteError> {
    let mut remaining = entropy;
    for segment in &buffer.segments {
        if remaining.is_empty() {
            break;
        }

        let write_len = match usize::try_from(segment.len) {
            Ok(segment_len) => segment_len.min(remaining.len()),
            Err(_) => remaining.len(),
        };
        let (source_segment, next_remaining) = remaining.split_at(write_len);
        memory
            .write_slice(source_segment, segment.address)
            .map_err(|source| VirtioRngBufferWriteError::SegmentWrite {
                descriptor_index: segment.descriptor_index,
                address: segment.address,
                len: write_len,
                source,
            })?;
        remaining = next_remaining;
    }

    if remaining.is_empty() {
        Ok(())
    } else {
        Err(VirtioRngBufferWriteError::IncompleteBufferWrite {
            remaining_bytes: remaining.len(),
        })
    }
}

fn descriptor_chain_head(chain: &VirtqueueDescriptorChain) -> Option<u16> {
    chain
        .descriptors()
        .first()
        .copied()
        .map(VirtqueueDescriptor::index)
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use crate::interrupt::DeviceInterruptKind;
    use crate::memory::{GuestMemoryError, GuestMemoryLayout, GuestMemoryRange};
    use crate::metrics::{EntropyDeviceMetrics, SharedEntropyDeviceMetrics};
    use crate::mmio::{
        MmioAccess, MmioAccessBytes, MmioBusError, MmioDispatchError, MmioDispatcher, MmioHandler,
        MmioHandlerError, MmioHandlerLookupError, MmioRegionId,
    };
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation,
        VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
        VirtioMmioDeviceRegisters, VirtioMmioQueueRegisters, VirtioMmioRegister,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
        VirtqueueAvailableRing, VirtqueueUsedRing,
    };

    use super::{
        EntropyMmioLayout, EntropyMmioRegistrationError, GuestAddress, GuestMemory,
        PreparedEntropyDevice, VIRTIO_RNG_DEVICE_ID, VIRTIO_RNG_MAX_REQUEST_BYTES,
        VIRTIO_RNG_QUEUE_SIZES, VirtioRngBufferParseError, VirtioRngDevice,
        VirtioRngDeviceActivationError, VirtioRngDeviceNotificationError, VirtioRngEntropySource,
        VirtioRngEntropySourceError, VirtioRngMmioHandler, VirtioRngOsEntropySource,
        VirtioRngQueue, VirtioRngQueueBuildError, VirtioRngQueueDispatchError,
    };

    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x1000);
    const TEST_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x5000);
    const TEST_USED_RING: GuestAddress = GuestAddress::new(0x6000);
    const TEST_DATA: GuestAddress = GuestAddress::new(0x8000);
    const TEST_SECOND_DATA: GuestAddress = GuestAddress::new(0xa000);
    const TEST_MMIO_BASE: GuestAddress = GuestAddress::new(0x1_0000);
    const TEST_QUEUE_SIZE: u16 = 8;
    const TEST_MEMORY_SIZE: u64 = 0x4_0000;
    const TEST_AVAILABLE_RING_IDX_OFFSET: u64 = 2;
    const TEST_AVAILABLE_RING_RING_OFFSET: u64 = 4;
    const TEST_AVAILABLE_RING_ENTRY_SIZE: u64 = 2;
    const TEST_USED_RING_IDX_OFFSET: u64 = 2;
    const TEST_USED_RING_RING_OFFSET: u64 = 4;
    const TEST_USED_RING_ELEMENT_SIZE: u64 = 8;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;

    #[derive(Debug)]
    struct OtherMmioHandler;

    impl MmioHandler for OtherMmioHandler {
        fn read(&mut self, _access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
            MmioAccessBytes::zeroed(1).map_err(|source| MmioHandlerError::new(source.to_string()))
        }

        fn write(
            &mut self,
            _access: MmioAccess,
            _data: MmioAccessBytes,
        ) -> Result<(), MmioHandlerError> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct TestEntropySource {
        calls: Vec<usize>,
        next_byte: u8,
        fail: bool,
    }

    impl TestEntropySource {
        fn failing() -> Self {
            Self {
                calls: Vec::new(),
                next_byte: 0,
                fail: true,
            }
        }

        fn calls(&self) -> &[usize] {
            &self.calls
        }
    }

    impl VirtioRngEntropySource for TestEntropySource {
        fn fill_entropy(
            &mut self,
            destination: &mut [u8],
        ) -> Result<(), VirtioRngEntropySourceError> {
            self.calls.push(destination.len());
            if self.fail {
                return Err(VirtioRngEntropySourceError::new());
            }

            for byte in destination {
                *byte = self.next_byte;
                self.next_byte = self.next_byte.wrapping_add(1);
            }

            Ok(())
        }
    }

    #[test]
    fn os_entropy_source_accepts_empty_destination() {
        let mut source = VirtioRngOsEntropySource::new();
        let mut bytes = [];

        source
            .fill_entropy(&mut bytes)
            .expect("OS entropy source should accept empty requests");

        assert!(bytes.is_empty());
    }

    #[test]
    fn os_entropy_source_accepts_non_empty_destination() {
        let mut source = VirtioRngOsEntropySource::new();
        let mut bytes = [0_u8; 32];

        source
            .fill_entropy(&mut bytes)
            .expect("OS entropy source should fill non-empty requests");

        assert_eq!(bytes.len(), 32);
    }

    fn memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("guest memory range should be valid"),
        ])
        .expect("guest memory layout should be valid");
        GuestMemory::allocate(&layout).expect("guest memory should allocate")
    }

    fn rng_queue() -> VirtioRngQueue {
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            TEST_AVAILABLE_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let used = VirtqueueUsedRing::new(TEST_USED_RING, TEST_QUEUE_SIZE)
            .expect("used ring should build");
        VirtioRngQueue::new(available, used)
    }

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test address should fit in queue low register")
    }

    fn configured_mmio_queue(size: u16, ready: bool) -> VirtioMmioQueueRegisters {
        configured_mmio_queue_with_device_ring(size, guest_address_low(TEST_USED_RING), 0, ready)
    }

    fn rng_device_activation<'a>(
        device: &'a VirtioMmioDeviceRegisters,
        queues: &'a VirtioMmioQueueRegisters,
    ) -> VirtioMmioDeviceActivation<'a> {
        VirtioMmioDeviceActivation::new(device, queues)
    }

    fn rng_device_registers() -> VirtioMmioDeviceRegisters {
        VirtioMmioDeviceRegisters::new(VIRTIO_RNG_DEVICE_ID, 0)
    }

    fn rng_mmio_handler() -> VirtioRngMmioHandler {
        VirtioRngMmioHandler::with_activation(
            VIRTIO_RNG_DEVICE_ID,
            0,
            &VIRTIO_RNG_QUEUE_SIZES,
            VirtioRngDevice::new(),
        )
        .expect("virtio-rng MMIO handler should build")
    }

    fn entropy_mmio_layout() -> EntropyMmioLayout {
        entropy_mmio_layout_at(TEST_MMIO_BASE, 7)
    }

    fn entropy_mmio_layout_at(address: GuestAddress, region_id: u64) -> EntropyMmioLayout {
        EntropyMmioLayout::new(address, MmioRegionId::new(region_id))
    }

    fn configure_rng_mmio_handler_queue(
        handler: &mut VirtioRngMmioHandler,
        device_ring: GuestAddress,
    ) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("ACKNOWLEDGE status should write");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("DRIVER status should write");
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("FEATURES_OK status should write");
        handler
            .write_register(VirtioMmioRegister::QueueNum, u32::from(TEST_QUEUE_SIZE))
            .expect("queue size should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(TEST_DESCRIPTOR_TABLE),
            )
            .expect("queue descriptor table should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(TEST_AVAILABLE_RING),
            )
            .expect("queue driver ring should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                guest_address_low(device_ring),
            )
            .expect("queue device ring should write");
        handler
            .write_register(VirtioMmioRegister::QueueReady, 1)
            .expect("queue ready should write");
    }

    fn activate_rng_mmio_handler(handler: &mut VirtioRngMmioHandler) {
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("DRIVER_OK status should activate virtio-rng");
    }

    fn notify_rng_queue(handler: &mut VirtioRngMmioHandler, queue_index: u32) {
        handler
            .write_register(VirtioMmioRegister::QueueNotify, queue_index)
            .expect("queue notification should write");
    }

    fn configured_mmio_queue_with_device_ring(
        size: u16,
        device_ring_low: u32,
        device_ring_high: u32,
        ready: bool,
    ) -> VirtioMmioQueueRegisters {
        let mut queues =
            VirtioMmioQueueRegisters::new(&[TEST_QUEUE_SIZE]).expect("queue table should build");
        queues
            .write_register(
                VirtioMmioRegister::QueueNum,
                u32::from(size),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue size should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(TEST_DESCRIPTOR_TABLE),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue descriptor table should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(TEST_AVAILABLE_RING),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue driver ring should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                device_ring_low,
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue device ring should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceHigh,
                device_ring_high,
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue device ring high should write");

        if ready {
            queues
                .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
                .expect("queue ready should write");
        }

        queues
    }

    fn descriptor_address(index: u16) -> GuestAddress {
        TEST_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE)
                        .expect("descriptor size should fit u64"),
            )
            .expect("descriptor address should not overflow")
    }

    fn write_descriptor(
        memory: &mut GuestMemory,
        index: u16,
        address: GuestAddress,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let descriptor = descriptor_address(index);
        write_u64(memory, descriptor, address.raw_value());
        write_u32(
            memory,
            descriptor
                .checked_add(8)
                .expect("descriptor len address should not overflow"),
            len,
        );
        write_u16(
            memory,
            descriptor
                .checked_add(12)
                .expect("descriptor flags address should not overflow"),
            flags,
        );
        write_u16(
            memory,
            descriptor
                .checked_add(14)
                .expect("descriptor next address should not overflow"),
            next,
        );
    }

    fn queue_head(memory: &mut GuestMemory, ring_index: u16, head: u16) {
        let address = TEST_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("available ring entry address should not overflow");
        write_u16(memory, address, head);
    }

    fn set_available_index(memory: &mut GuestMemory, index: u16) {
        write_u16(
            memory,
            TEST_AVAILABLE_RING
                .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
                .expect("available index address should not overflow"),
            index,
        );
    }

    fn used_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_USED_RING
            .checked_add(
                TEST_USED_RING_RING_OFFSET + u64::from(ring_index) * TEST_USED_RING_ELEMENT_SIZE,
            )
            .expect("used ring entry address should not overflow")
    }

    fn read_used_index(memory: &GuestMemory) -> u16 {
        read_u16(
            memory,
            TEST_USED_RING
                .checked_add(TEST_USED_RING_IDX_OFFSET)
                .expect("used index address should not overflow"),
        )
    }

    fn read_used_id(memory: &GuestMemory, ring_index: u16) -> u32 {
        read_u32(memory, used_ring_entry_address(ring_index))
    }

    fn read_used_len(memory: &GuestMemory, ring_index: u16) -> u32 {
        read_u32(
            memory,
            used_ring_entry_address(ring_index)
                .checked_add(4)
                .expect("used len address should not overflow"),
        )
    }

    fn write_u64(memory: &mut GuestMemory, address: GuestAddress, value: u64) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u64 should write");
    }

    fn write_u32(memory: &mut GuestMemory, address: GuestAddress, value: u32) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u32 should write");
    }

    fn write_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 should write");
    }

    fn read_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("u32 should read");
        u32::from_le_bytes(bytes)
    }

    fn read_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("u16 should read");
        u16::from_le_bytes(bytes)
    }

    fn read_guest_bytes(memory: &GuestMemory, address: GuestAddress, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest bytes should read");
        bytes
    }

    #[test]
    fn prepared_entropy_device_registers_mmio_handler_in_fresh_dispatcher() {
        let layout = entropy_mmio_layout();

        let device = PreparedEntropyDevice::new()
            .register_mmio(layout)
            .expect("entropy MMIO registration should succeed");
        let registration = *device.registration();

        assert_eq!(registration.region_id(), layout.region_id());
        assert_eq!(registration.address(), layout.address());
        assert_eq!(
            registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(device.dispatcher().regions(), &[registration.region()]);

        let (mut dispatcher, registration) = device.into_parts();
        dispatcher
            .handler_mut::<VirtioRngMmioHandler>(registration.region_id())
            .expect("registered entropy handler should have the expected type");
    }

    #[test]
    fn prepared_entropy_device_registers_mmio_handler_in_existing_dispatcher() {
        let mut dispatcher = MmioDispatcher::new();
        let existing_region = dispatcher
            .insert_region(MmioRegionId::new(1), GuestAddress::new(0x8000), 0x1000)
            .expect("existing region should insert");
        let layout = entropy_mmio_layout_at(GuestAddress::new(0x1_0000), 2);

        let device = PreparedEntropyDevice::new()
            .register_mmio_with_dispatcher(layout, dispatcher)
            .expect("entropy MMIO registration should succeed");

        assert_eq!(
            device.dispatcher().regions(),
            &[existing_region, device.registration().region()]
        );
    }

    #[test]
    fn prepared_entropy_device_rejects_invalid_mmio_region() {
        let layout = entropy_mmio_layout_at(GuestAddress::new(u64::MAX), 3);

        let error = PreparedEntropyDevice::new()
            .register_mmio(layout)
            .expect_err("overflowing MMIO region should fail");

        match error {
            EntropyMmioRegistrationError::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                assert_eq!(region_id, layout.region_id());
                assert_eq!(address, layout.address());
                assert!(matches!(source, GuestMemoryError::AddressOverflow { .. }));
            }
            source => panic!("unexpected error: {source}"),
        }
    }

    #[test]
    fn prepared_entropy_device_rejects_overlapping_mmio_region() {
        let layout = entropy_mmio_layout();
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                MmioRegionId::new(1),
                layout.address(),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("existing region should insert");

        let error = PreparedEntropyDevice::new()
            .register_mmio_with_dispatcher(layout, dispatcher)
            .expect_err("overlapping region should fail");

        assert!(matches!(
            error,
            EntropyMmioRegistrationError::InsertRegion {
                source: MmioBusError::OverlappingRegion { .. },
                ..
            }
        ));
    }

    #[test]
    fn prepared_entropy_device_rejects_duplicate_mmio_handler() {
        let layout = entropy_mmio_layout();
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .register_handler(layout.region_id(), OtherMmioHandler)
            .expect("existing handler should register");

        let error = PreparedEntropyDevice::new()
            .register_mmio_with_dispatcher(layout, dispatcher)
            .expect_err("duplicate handler should fail");

        assert!(matches!(
            error,
            EntropyMmioRegistrationError::RegisterHandler {
                region_id,
                source: MmioDispatchError::DuplicateHandler { .. },
            } if region_id == layout.region_id()
        ));
    }

    #[test]
    fn prepared_entropy_device_preserves_typed_handler_lookup() {
        let layout = entropy_mmio_layout();
        let device = PreparedEntropyDevice::new()
            .register_mmio(layout)
            .expect("entropy MMIO registration should succeed");
        let (mut dispatcher, registration) = device.into_parts();

        let error = dispatcher
            .handler_mut::<OtherMmioHandler>(registration.region_id())
            .expect_err("wrong handler type lookup should fail");

        assert_eq!(
            error,
            MmioHandlerLookupError::UnexpectedHandlerType {
                region_id: registration.region_id(),
                expected: std::any::type_name::<OtherMmioHandler>(),
            }
        );
    }

    #[test]
    fn prepared_entropy_device_registrations_are_independent() {
        let first = PreparedEntropyDevice::new()
            .register_mmio(entropy_mmio_layout_at(GuestAddress::new(0x1_0000), 10))
            .expect("first entropy MMIO registration should succeed");
        let second = PreparedEntropyDevice::new()
            .register_mmio(entropy_mmio_layout_at(GuestAddress::new(0x2_0000), 11))
            .expect("second entropy MMIO registration should succeed");
        let (mut first_dispatcher, first_registration) = first.into_parts();
        let (mut second_dispatcher, second_registration) = second.into_parts();

        let first_handler = first_dispatcher
            .handler_mut::<VirtioRngMmioHandler>(first_registration.region_id())
            .expect("first entropy handler should exist");
        configure_rng_mmio_handler_queue(first_handler, TEST_USED_RING);
        activate_rng_mmio_handler(first_handler);

        let second_handler = second_dispatcher
            .handler_mut::<VirtioRngMmioHandler>(second_registration.region_id())
            .expect("second entropy handler should exist");
        assert!(!second_handler.activation_handler().is_activated());
        assert_ne!(
            first_registration.region_id(),
            second_registration.region_id()
        );
        assert_ne!(first_registration.address(), second_registration.address());
    }

    #[test]
    fn displays_entropy_mmio_registration_errors_and_preserves_sources() {
        let error = PreparedEntropyDevice::new()
            .register_mmio(entropy_mmio_layout_at(GuestAddress::new(u64::MAX), 12))
            .expect_err("overflowing MMIO region should fail");

        assert!(
            error
                .to_string()
                .contains("invalid entropy MMIO region id=12")
        );
        assert!(error.source().is_some());
    }

    #[test]
    fn rng_queue_from_mmio_queue_state_uses_configured_queue_metadata() {
        let queues = configured_mmio_queue(4, true);

        let queue = VirtioRngQueue::from_mmio_queue_state(
            queues.queue(0).expect("queue state should exist"),
        )
        .expect("rng queue should build from ready mmio queue state");

        assert_eq!(
            queue.available_ring().descriptor_table(),
            TEST_DESCRIPTOR_TABLE
        );
        assert_eq!(queue.available_ring().available_ring(), TEST_AVAILABLE_RING);
        assert_eq!(queue.available_ring().queue_size(), 4);
        assert_eq!(queue.available_ring().next_avail(), 0);
        assert_eq!(queue.used_ring().used_ring(), TEST_USED_RING);
        assert_eq!(queue.used_ring().queue_size(), 4);
        assert_eq!(queue.used_ring().next_used(), 0);
    }

    #[test]
    fn rng_queue_from_mmio_queue_state_rejects_not_ready_queue() {
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, false);

        let error = VirtioRngQueue::from_mmio_queue_state(
            queues.queue(0).expect("queue state should exist"),
        )
        .expect_err("not-ready queue should not build");

        assert!(matches!(error, VirtioRngQueueBuildError::QueueNotReady));
        assert_eq!(error.to_string(), "virtio-rng queue is not ready");
        assert!(error.source().is_none());
    }

    #[test]
    fn rng_queue_from_mmio_queue_state_wraps_available_ring_error() {
        let mut queues =
            VirtioMmioQueueRegisters::new(&[TEST_QUEUE_SIZE]).expect("queue table should build");
        queues
            .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
            .expect("queue ready should write");

        let error = VirtioRngQueue::from_mmio_queue_state(
            queues.queue(0).expect("queue state should exist"),
        )
        .expect_err("zero-size queue should not build");

        assert!(matches!(
            error,
            VirtioRngQueueBuildError::AvailableRing { .. }
        ));
        assert_eq!(
            error
                .source()
                .expect("source should be preserved")
                .to_string(),
            "virtqueue size 0 must be a nonzero power of two"
        );
    }

    #[test]
    fn rng_queue_from_mmio_queue_state_wraps_used_ring_error() {
        let queues =
            configured_mmio_queue_with_device_ring(TEST_QUEUE_SIZE, u32::MAX - 3, u32::MAX, true);

        let error = VirtioRngQueue::from_mmio_queue_state(
            queues.queue(0).expect("queue state should exist"),
        )
        .expect_err("overflowing used ring should not build");

        assert!(matches!(error, VirtioRngQueueBuildError::UsedRing { .. }));
        assert_eq!(
            error
                .source()
                .expect("source should be preserved")
                .to_string(),
            "virtqueue used ring address 0xfffffffffffffffc with queue size 8 overflows address space"
        );
    }

    #[test]
    fn rng_device_starts_inactive_and_reset_clears_active_queue() {
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, true);
        let device_registers = rng_device_registers();
        let mut device = VirtioRngDevice::new();

        assert!(!device.is_activated());
        assert!(device.active_queue().is_none());

        device
            .activate_rng(rng_device_activation(&device_registers, &queues))
            .expect("virtio-rng device should activate");

        assert!(device.is_activated());
        assert!(device.active_queue().is_some());

        device.reset();

        assert!(!device.is_activated());
        assert!(device.active_queue().is_none());
    }

    #[test]
    fn rng_device_activation_uses_configured_queue_metadata() {
        let queues = configured_mmio_queue(4, true);
        let device_registers = rng_device_registers();
        let mut device = VirtioRngDevice::new();

        device
            .activate_rng(rng_device_activation(&device_registers, &queues))
            .expect("virtio-rng device should activate");

        let queue = device
            .active_queue()
            .expect("activated device should retain active queue");
        assert_eq!(
            queue.available_ring().descriptor_table(),
            TEST_DESCRIPTOR_TABLE
        );
        assert_eq!(queue.available_ring().available_ring(), TEST_AVAILABLE_RING);
        assert_eq!(queue.available_ring().queue_size(), 4);
        assert_eq!(queue.used_ring().used_ring(), TEST_USED_RING);
        assert_eq!(queue.used_ring().queue_size(), 4);
    }

    #[test]
    fn rng_device_activation_rejects_duplicate_activation() {
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, true);
        let device_registers = rng_device_registers();
        let mut device = VirtioRngDevice::new();

        device
            .activate_rng(rng_device_activation(&device_registers, &queues))
            .expect("first virtio-rng activation should succeed");
        let error = device
            .activate_rng(rng_device_activation(&device_registers, &queues))
            .expect_err("second virtio-rng activation should fail");

        assert!(matches!(
            error,
            VirtioRngDeviceActivationError::AlreadyActive
        ));
        assert_eq!(error.to_string(), "virtio-rng device is already active");
    }

    #[test]
    fn rng_device_activation_rejects_unexpected_queue_count() {
        let queues = VirtioMmioQueueRegisters::new(&[TEST_QUEUE_SIZE, TEST_QUEUE_SIZE])
            .expect("queue table should build");
        let device_registers = rng_device_registers();
        let mut device = VirtioRngDevice::new();

        let error = device
            .activate_rng(rng_device_activation(&device_registers, &queues))
            .expect_err("extra queue should fail virtio-rng activation");

        assert!(matches!(
            error,
            VirtioRngDeviceActivationError::QueueCountMismatch {
                expected: 1,
                actual: 2
            }
        ));
        assert_eq!(
            error.to_string(),
            "virtio-rng device requires 1 queue(s), got 2"
        );
    }

    #[test]
    fn rng_device_activation_wraps_not_ready_queue_error() {
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, false);
        let device_registers = rng_device_registers();
        let mut device = VirtioRngDevice::new();

        let error = device
            .activate_rng(rng_device_activation(&device_registers, &queues))
            .expect_err("not-ready queue should fail virtio-rng activation");

        assert!(matches!(
            error,
            VirtioRngDeviceActivationError::QueueBuild {
                queue_index: 0,
                source: VirtioRngQueueBuildError::QueueNotReady
            }
        ));
        assert_eq!(
            error
                .source()
                .expect("queue build error source should be preserved")
                .to_string(),
            "virtio-rng queue is not ready"
        );
    }

    #[test]
    fn rng_device_activation_trait_error_is_generic_handler_error() {
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, false);
        let device_registers = rng_device_registers();
        let mut device = VirtioRngDevice::new();

        let error = VirtioMmioDeviceActivationHandler::activate(
            &mut device,
            rng_device_activation(&device_registers, &queues),
        )
        .expect_err("trait activation should fail with generic handler error");

        match error {
            VirtioMmioDeviceActivationError::Handler { source } => {
                assert_eq!(
                    source.to_string(),
                    "failed to activate virtio-rng queue 0: virtio-rng queue is not ready"
                );
            }
        }
        assert!(!device.is_activated());
        assert!(device.active_queue().is_none());
    }

    #[test]
    fn rng_device_notification_without_pending_queues_is_noop() {
        let mut memory = memory();
        let mut source = TestEntropySource::default();
        let mut handler = rng_mmio_handler();
        configure_rng_mmio_handler_queue(&mut handler, TEST_USED_RING);
        activate_rng_mmio_handler(&mut handler);

        let dispatch = handler
            .dispatch_rng_queue_notifications(&mut memory, &mut source)
            .expect("empty notification dispatch should succeed");

        assert!(dispatch.drained_notifications().is_empty());
        assert!(dispatch.queue_dispatch().is_none());
        assert!(!dispatch.needs_queue_interrupt());
        assert!(source.calls().is_empty());
        assert!(!handler.has_pending_queue_notifications());
        assert!(
            !handler
                .interrupt_registers()
                .pending_status()
                .contains(DeviceInterruptKind::Queue)
        );
    }

    #[test]
    fn rng_mmio_handler_reset_clears_active_queue_and_pending_notification() {
        let mut handler = rng_mmio_handler();
        configure_rng_mmio_handler_queue(&mut handler, TEST_USED_RING);
        activate_rng_mmio_handler(&mut handler);
        notify_rng_queue(&mut handler, 0);

        assert!(handler.is_device_activated());
        assert!(handler.activation_handler().is_activated());
        assert_eq!(handler.pending_queue_notifications(), vec![0]);

        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_INIT)
            .expect("reset status should write");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
        assert!(handler.pending_queue_notifications().is_empty());
        assert!(handler.interrupt_registers().pending_status().is_empty());
    }

    #[test]
    fn rng_device_notification_rejects_inactive_device() {
        let mut memory = memory();
        let mut source = TestEntropySource::default();
        let mut device = VirtioRngDevice::new();

        let error = device
            .dispatch_drained_queue_notifications(&mut memory, vec![0], &mut source)
            .expect_err("inactive virtio-rng device should reject notifications");

        assert!(matches!(
            error,
            VirtioRngDeviceNotificationError::Inactive { .. }
        ));
        assert_eq!(error.drained_notifications(), &[0]);
        assert_eq!(
            error.to_string(),
            "virtio-rng queue notification cannot be dispatched before activation"
        );
    }

    #[test]
    fn rng_device_notification_rejects_unsupported_queue() {
        let mut memory = memory();
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, true);
        let device_registers = rng_device_registers();
        let mut source = TestEntropySource::default();
        let mut device = VirtioRngDevice::new();
        device
            .activate_rng(rng_device_activation(&device_registers, &queues))
            .expect("virtio-rng device should activate");

        let error = device
            .dispatch_drained_queue_notifications(&mut memory, vec![0, 1], &mut source)
            .expect_err("unsupported virtio-rng queue should fail notification dispatch");

        match &error {
            VirtioRngDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            } => {
                assert_eq!(drained_notifications, &[0, 1]);
                assert_eq!(*queue_index, 1);
            }
            other => panic!("expected unsupported queue error, got {other:?}"),
        }
        assert!(source.calls().is_empty());
    }

    #[test]
    fn rng_mmio_handler_dispatches_notification_and_marks_queue_interrupt() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 8, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut handler = rng_mmio_handler();
        configure_rng_mmio_handler_queue(&mut handler, TEST_USED_RING);
        activate_rng_mmio_handler(&mut handler);
        notify_rng_queue(&mut handler, 0);

        let mut source = TestEntropySource::default();
        let dispatch = handler
            .dispatch_rng_queue_notifications(&mut memory, &mut source)
            .expect("virtio-rng notification should dispatch");

        assert_eq!(dispatch.drained_notifications(), &[0]);
        let queue_dispatch = dispatch
            .queue_dispatch()
            .expect("queue dispatch should be present");
        assert_eq!(queue_dispatch.processed_requests(), 1);
        assert_eq!(queue_dispatch.successful_requests(), 1);
        assert!(queue_dispatch.needs_queue_interrupt());
        assert_eq!(source.calls(), &[8]);
        assert_eq!(
            read_guest_bytes(&memory, TEST_DATA, 8),
            vec![0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_len(&memory, 0), 8);
        assert!(!handler.has_pending_queue_notifications());
        assert!(
            handler
                .interrupt_registers()
                .pending_status()
                .contains(DeviceInterruptKind::Queue)
        );
    }

    #[test]
    fn entropy_metrics_record_notification_dispatch() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 8, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut handler = rng_mmio_handler();
        configure_rng_mmio_handler_queue(&mut handler, TEST_USED_RING);
        activate_rng_mmio_handler(&mut handler);
        notify_rng_queue(&mut handler, 0);

        let mut source = TestEntropySource::default();
        let dispatch = handler
            .dispatch_rng_queue_notifications(&mut memory, &mut source)
            .expect("virtio-rng notification should dispatch");
        let metrics = SharedEntropyDeviceMetrics::default();
        metrics.record_notification_dispatch(&dispatch);

        assert_eq!(
            metrics.snapshot(),
            EntropyDeviceMetrics::default()
                .with_entropy_event_count(1)
                .with_entropy_bytes(8)
        );
    }

    #[test]
    fn rng_mmio_handler_preserves_partial_queue_error_and_marks_interrupt() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 4, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        queue_head(&mut memory, 1, TEST_QUEUE_SIZE);
        set_available_index(&mut memory, 2);

        let mut handler = rng_mmio_handler();
        configure_rng_mmio_handler_queue(&mut handler, TEST_USED_RING);
        activate_rng_mmio_handler(&mut handler);
        notify_rng_queue(&mut handler, 0);

        let mut source = TestEntropySource::default();
        let error = handler
            .dispatch_rng_queue_notifications(&mut memory, &mut source)
            .expect_err("second available head should fail");

        match &error {
            VirtioRngDeviceNotificationError::QueueDispatch { source, .. } => {
                assert!(matches!(
                    source,
                    VirtioRngQueueDispatchError::AvailableRing { .. }
                ));
            }
            other => panic!("expected queue dispatch error, got {other:?}"),
        }
        assert_eq!(error.drained_notifications(), &[0]);
        let completed = error
            .completed_dispatch()
            .expect("partial dispatch should be preserved");
        assert_eq!(completed.processed_requests(), 1);
        assert_eq!(completed.successful_requests(), 1);
        assert_eq!(completed.bytes_written_to_guest(), 4);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(source.calls(), &[4]);
        assert_eq!(read_guest_bytes(&memory, TEST_DATA, 4), vec![0, 1, 2, 3]);
        assert!(!handler.has_pending_queue_notifications());
        assert!(
            handler
                .interrupt_registers()
                .pending_status()
                .contains(DeviceInterruptKind::Queue)
        );
    }

    #[test]
    fn entropy_metrics_record_partial_notification_error() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 4, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        queue_head(&mut memory, 1, TEST_QUEUE_SIZE);
        set_available_index(&mut memory, 2);

        let mut handler = rng_mmio_handler();
        configure_rng_mmio_handler_queue(&mut handler, TEST_USED_RING);
        activate_rng_mmio_handler(&mut handler);
        notify_rng_queue(&mut handler, 0);

        let mut source = TestEntropySource::default();
        let error = handler
            .dispatch_rng_queue_notifications(&mut memory, &mut source)
            .expect_err("second available head should fail");
        let metrics = SharedEntropyDeviceMetrics::default();
        metrics.record_notification_error(&error);

        assert_eq!(
            metrics.snapshot(),
            EntropyDeviceMetrics::default()
                .with_entropy_event_fails(1)
                .with_entropy_event_count(1)
                .with_entropy_bytes(4)
        );
    }

    #[test]
    fn rng_mmio_handler_repeated_notifications_do_not_reuse_stale_state() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 4, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut handler = rng_mmio_handler();
        configure_rng_mmio_handler_queue(&mut handler, TEST_USED_RING);
        activate_rng_mmio_handler(&mut handler);
        notify_rng_queue(&mut handler, 0);

        let mut source = TestEntropySource::default();
        let first = handler
            .dispatch_rng_queue_notifications(&mut memory, &mut source)
            .expect("first notification should dispatch");

        write_descriptor(
            &mut memory,
            1,
            TEST_SECOND_DATA,
            4,
            VIRTQUEUE_DESC_F_WRITE,
            0,
        );
        queue_head(&mut memory, 1, 1);
        set_available_index(&mut memory, 2);
        notify_rng_queue(&mut handler, 0);
        let second = handler
            .dispatch_rng_queue_notifications(&mut memory, &mut source)
            .expect("second notification should dispatch");

        assert_eq!(
            first
                .queue_dispatch()
                .expect("first queue dispatch should be present")
                .processed_requests(),
            1
        );
        assert_eq!(
            second
                .queue_dispatch()
                .expect("second queue dispatch should be present")
                .processed_requests(),
            1
        );
        assert_eq!(source.calls(), &[4, 4]);
        assert_eq!(read_guest_bytes(&memory, TEST_DATA, 4), vec![0, 1, 2, 3]);
        assert_eq!(
            read_guest_bytes(&memory, TEST_SECOND_DATA, 4),
            vec![4, 5, 6, 7]
        );
        assert_eq!(read_used_index(&memory), 2);
        assert!(!handler.has_pending_queue_notifications());
    }

    #[test]
    fn rng_queue_dispatch_fills_single_writable_descriptor() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 8, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(dispatch.buffer_parse_failures(), 0);
        assert_eq!(dispatch.source_failures(), 0);
        assert_eq!(dispatch.bytes_written_to_guest(), 8);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(source.calls(), &[8]);
        assert_eq!(
            read_guest_bytes(&memory, TEST_DATA, 8),
            vec![0, 1, 2, 3, 4, 5, 6, 7]
        );
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_id(&memory, 0), 0);
        assert_eq!(read_used_len(&memory, 0), 8);
    }

    #[test]
    fn rng_queue_dispatch_empty_available_ring_is_noop() {
        let mut memory = memory();
        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();

        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("empty rng queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 0);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.bytes_written_to_guest(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert!(source.calls().is_empty());
        assert_eq!(read_used_index(&memory), 0);
    }

    #[test]
    fn rng_queue_dispatch_fills_split_descriptor_chain() {
        let mut memory = memory();
        write_descriptor(
            &mut memory,
            0,
            TEST_DATA,
            4,
            VIRTQUEUE_DESC_F_WRITE | VIRTQUEUE_DESC_F_NEXT,
            1,
        );
        write_descriptor(
            &mut memory,
            1,
            TEST_SECOND_DATA,
            4,
            VIRTQUEUE_DESC_F_WRITE,
            0,
        );
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(dispatch.bytes_written_to_guest(), 8);
        assert_eq!(source.calls(), &[8]);
        assert_eq!(read_guest_bytes(&memory, TEST_DATA, 4), vec![0, 1, 2, 3]);
        assert_eq!(
            read_guest_bytes(&memory, TEST_SECOND_DATA, 4),
            vec![4, 5, 6, 7]
        );
        assert_eq!(read_used_len(&memory, 0), 8);
    }

    #[test]
    fn rng_queue_dispatch_writes_overlapping_descriptors_sequentially() {
        let mut memory = memory();
        write_descriptor(
            &mut memory,
            0,
            TEST_DATA,
            4,
            VIRTQUEUE_DESC_F_WRITE | VIRTQUEUE_DESC_F_NEXT,
            1,
        );
        write_descriptor(
            &mut memory,
            1,
            TEST_DATA
                .checked_add(2)
                .expect("overlapping descriptor address should not overflow"),
            4,
            VIRTQUEUE_DESC_F_WRITE,
            0,
        );
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(dispatch.bytes_written_to_guest(), 8);
        assert_eq!(source.calls(), &[8]);
        assert_eq!(
            read_guest_bytes(&memory, TEST_DATA, 6),
            vec![0, 1, 4, 5, 6, 7]
        );
        assert_eq!(read_used_len(&memory, 0), 8);
    }

    #[test]
    fn rng_queue_dispatch_processes_multiple_requests() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 4, VIRTQUEUE_DESC_F_WRITE, 0);
        write_descriptor(
            &mut memory,
            1,
            TEST_SECOND_DATA,
            4,
            VIRTQUEUE_DESC_F_WRITE,
            0,
        );
        queue_head(&mut memory, 0, 0);
        queue_head(&mut memory, 1, 1);
        set_available_index(&mut memory, 2);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 2);
        assert_eq!(dispatch.successful_requests(), 2);
        assert_eq!(dispatch.bytes_written_to_guest(), 8);
        assert_eq!(source.calls(), &[4, 4]);
        assert_eq!(read_guest_bytes(&memory, TEST_DATA, 4), vec![0, 1, 2, 3]);
        assert_eq!(
            read_guest_bytes(&memory, TEST_SECOND_DATA, 4),
            vec![4, 5, 6, 7]
        );
        assert_eq!(read_used_index(&memory), 2);
        assert_eq!(read_used_id(&memory, 0), 0);
        assert_eq!(read_used_len(&memory, 0), 4);
        assert_eq!(read_used_id(&memory, 1), 1);
        assert_eq!(read_used_len(&memory, 1), 4);
    }

    #[test]
    fn rng_queue_dispatch_preserves_ring_state_across_dispatch_calls() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 4, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let first = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("first rng queue dispatch should succeed");

        write_descriptor(
            &mut memory,
            1,
            TEST_SECOND_DATA,
            4,
            VIRTQUEUE_DESC_F_WRITE,
            0,
        );
        queue_head(&mut memory, 1, 1);
        set_available_index(&mut memory, 2);
        let second = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("second rng queue dispatch should succeed");

        assert_eq!(first.processed_requests(), 1);
        assert_eq!(second.processed_requests(), 1);
        assert_eq!(source.calls(), &[4, 4]);
        assert_eq!(read_guest_bytes(&memory, TEST_DATA, 4), vec![0, 1, 2, 3]);
        assert_eq!(
            read_guest_bytes(&memory, TEST_SECOND_DATA, 4),
            vec![4, 5, 6, 7]
        );
        assert_eq!(read_used_index(&memory), 2);
        assert_eq!(read_used_id(&memory, 0), 0);
        assert_eq!(read_used_id(&memory, 1), 1);
    }

    #[test]
    fn rng_queue_dispatch_caps_single_request_to_sixty_four_kib() {
        let mut memory = memory();
        let requested_len =
            u32::try_from(VIRTIO_RNG_MAX_REQUEST_BYTES + 4096).expect("test len should fit u32");
        write_descriptor(
            &mut memory,
            0,
            TEST_DATA,
            requested_len,
            VIRTQUEUE_DESC_F_WRITE,
            0,
        );
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(
            dispatch.bytes_written_to_guest(),
            u64::try_from(VIRTIO_RNG_MAX_REQUEST_BYTES).expect("max request should fit u64")
        );
        assert_eq!(source.calls(), &[VIRTIO_RNG_MAX_REQUEST_BYTES]);
        assert_eq!(
            read_used_len(&memory, 0),
            u32::try_from(VIRTIO_RNG_MAX_REQUEST_BYTES).expect("max request should fit u32")
        );
        assert_eq!(read_guest_bytes(&memory, TEST_DATA, 4), vec![0, 1, 2, 3]);
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_DATA
                    .checked_add(
                        u64::try_from(VIRTIO_RNG_MAX_REQUEST_BYTES)
                            .expect("max request should fit u64")
                    )
                    .expect("post-cap byte address should not overflow"),
                1,
            ),
            vec![0]
        );
    }

    #[test]
    fn rng_queue_dispatch_completes_read_only_descriptor_with_zero_len() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 8, 0, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should complete invalid descriptor");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 1);
        assert_eq!(dispatch.bytes_written_to_guest(), 0);
        assert!(source.calls().is_empty());
        assert!(matches!(
            dispatch.first_buffer_parse_failure(),
            Some(VirtioRngBufferParseError::BufferDescriptorReadOnly { index: 0 })
        ));
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_id(&memory, 0), 0);
        assert_eq!(read_used_len(&memory, 0), 0);
    }

    #[test]
    fn rng_queue_dispatch_completes_empty_descriptor_with_zero_len() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 0, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should complete invalid descriptor");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 1);
        assert_eq!(dispatch.bytes_written_to_guest(), 0);
        assert!(source.calls().is_empty());
        assert!(matches!(
            dispatch.first_buffer_parse_failure(),
            Some(VirtioRngBufferParseError::BufferDescriptorEmpty { index: 0 })
        ));
        assert_eq!(read_used_len(&memory, 0), 0);
    }

    #[test]
    fn rng_queue_dispatch_validates_unmapped_descriptor_before_entropy_source() {
        let mut memory = memory();
        write_descriptor(
            &mut memory,
            0,
            GuestAddress::new(TEST_MEMORY_SIZE),
            8,
            VIRTQUEUE_DESC_F_WRITE,
            0,
        );
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should complete invalid descriptor");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 1);
        assert!(source.calls().is_empty());
        assert!(matches!(
            dispatch.first_buffer_parse_failure(),
            Some(VirtioRngBufferParseError::BufferDescriptorAccess { index: 0, .. })
        ));
        assert_eq!(read_used_len(&memory, 0), 0);
    }

    #[test]
    fn rng_queue_dispatch_validates_range_overflow_before_entropy_source() {
        let mut memory = memory();
        write_descriptor(
            &mut memory,
            0,
            GuestAddress::new(u64::MAX),
            1,
            VIRTQUEUE_DESC_F_WRITE,
            0,
        );
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::default();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should complete invalid descriptor");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 1);
        assert!(source.calls().is_empty());
        assert!(matches!(
            dispatch.first_buffer_parse_failure(),
            Some(VirtioRngBufferParseError::BufferDescriptorRange { index: 0, .. })
        ));
        assert_eq!(read_used_len(&memory, 0), 0);
    }

    #[test]
    fn rng_queue_dispatch_records_source_failure_with_zero_len() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 8, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::failing();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should complete source failure");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.source_failures(), 1);
        assert_eq!(
            dispatch.first_source_failure(),
            Some(VirtioRngEntropySourceError)
        );
        assert_eq!(source.calls(), &[8]);
        assert_eq!(read_guest_bytes(&memory, TEST_DATA, 8), vec![0; 8]);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_len(&memory, 0), 0);
    }

    #[test]
    fn entropy_metrics_record_source_failure() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 8, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let mut source = TestEntropySource::failing();
        let mut queue = rng_queue();
        let dispatch = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect("rng queue dispatch should complete source failure");
        let metrics = SharedEntropyDeviceMetrics::default();
        metrics.record_queue_dispatch(&dispatch);

        assert_eq!(
            metrics.snapshot(),
            EntropyDeviceMetrics::default()
                .with_entropy_event_fails(1)
                .with_entropy_event_count(1)
                .with_host_rng_fails(1)
        );
    }

    #[test]
    fn rng_queue_dispatch_returns_used_ring_error_with_completed_dispatch() {
        let mut memory = memory();
        write_descriptor(&mut memory, 0, TEST_DATA, 8, VIRTQUEUE_DESC_F_WRITE, 0);
        queue_head(&mut memory, 0, 0);
        set_available_index(&mut memory, 1);

        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            TEST_AVAILABLE_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let used = VirtqueueUsedRing::new(GuestAddress::new(TEST_MEMORY_SIZE), TEST_QUEUE_SIZE)
            .expect("used ring metadata should build");
        let mut queue = VirtioRngQueue::new(available, used);
        let mut source = TestEntropySource::default();

        let error = queue
            .dispatch_with_source(&mut memory, &mut source)
            .expect_err("unmapped used ring should fail");

        match &error {
            VirtioRngQueueDispatchError::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
                ..
            } => {
                assert_eq!(*descriptor_head, 0);
                assert_eq!(*bytes_written_to_guest, 8);
                assert!(source.to_string().contains("is not fully mapped"));
            }
            other => panic!("expected used ring error, got {other:?}"),
        }
        assert_eq!(error.completed_dispatch().processed_requests(), 0);
        assert_eq!(source.calls(), &[8]);
    }
}
