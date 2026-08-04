//! Modern, non-transitional virtio-pci transport primitives.
//!
//! The endpoint deliberately exposes separate PCI-configuration and BAR
//! handles backed by one synchronization boundary. Guest-programmed MSI-X
//! messages are resolved only through a device-scoped registry of opaque live
//! backend routes.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::logger::{GuestLogger, LoggerDeviceKind, LoggerTransportOutcome};
use crate::memory::{GuestAddress, GuestMemoryRange};
use crate::message_interrupt::{
    GuestMessage, GuestMessageInterruptRegistry, GuestMessageInterruptRegistryError,
    GuestMessageInterruptRegistryPhase, GuestMessageInterruptResources,
    GuestMessageInterruptResourcesError,
};
use crate::mmio::{
    MmioAccess, MmioAccessBytes, MmioDispatcher, MmioHandler, MmioHandlerError, MmioOperationKind,
    MmioRegionId, MmioRegionRequest, MmioRegistrationError, MmioRegistrationLease,
    MmioRegistrationOwner, MmioRegistrationReleaseError,
};
use crate::pci::{
    PciBarAddressSpace, PciBarAllocationError, PciBarAllocator, PciBarConfigurationError,
    PciBarLease, PciBarPrefetchable, PciBarReleaseError, PciCapabilityError, PciCapabilityId,
    PciClassCode, PciConfigAccessError, PciConfigFunction, PciFunctionLease,
    PciFunctionReleaseError, PciSbdf, PciSegment, PciSegmentError, PciSegmentLockError,
    PciType0Configuration, PciType0GuestState, PciType0SnapshotError, PciType0SnapshotProfile,
    SharedPciSegment,
};
use crate::snapshot_device_v2::{
    SnapshotV2InterruptIntent, SnapshotV2PciDeviceState, SnapshotV2VirtioState,
};
use crate::virtio::{
    VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER, VIRTIO_DEVICE_STATUS_DRIVER_OK,
    VIRTIO_DEVICE_STATUS_FAILED, VIRTIO_DEVICE_STATUS_FEATURES_OK, VIRTIO_DEVICE_STATUS_INIT,
    VIRTIO_MMIO_VERSION_1_FEATURE, VirtioDeviceActivation, VirtioDeviceActivationHandler,
    VirtioDeviceConfigAccess, VirtioDeviceConfigError, VirtioDeviceConfigHandler, VirtioDeviceCore,
    VirtioDeviceResetError, VirtioDeviceResetOutcome, VirtioDeviceType, VirtioDeviceTypeError,
    VirtioDeviceWorkGuard, VirtioInterruptIntent, VirtioQueueError, VirtioQueueNotificationError,
    VirtioQueueNotificationState, VirtioQueues,
};
use crate::virtio_mmio::{
    VirtioMmioDeviceRegisters, VirtioMmioQueueRegisterError, VirtioMmioRegister,
    VirtioMmioRegisterStateError,
};
use crate::virtio_queue::{
    VIRTQUEUE_AVAILABLE_RING_ALIGNMENT, VIRTQUEUE_DESCRIPTOR_ALIGNMENT,
    VIRTQUEUE_USED_RING_ALIGNMENT,
};

pub const VIRTIO_PCI_VENDOR_ID: u16 = 0x1af4;
pub const VIRTIO_PCI_REVISION_ID: u8 = 1;
pub const VIRTIO_PCI_NON_TRANSITIONAL_SUBCLASS: u8 = 0xff;
pub const VIRTIO_PCI_CAPABILITY_BAR_INDEX: u8 = 0;
pub const VIRTIO_PCI_CAPABILITY_BAR_SIZE: u64 = 0x80000;

pub const VIRTIO_PCI_COMMON_CONFIG_OFFSET: u64 = 0x0000;
pub const VIRTIO_PCI_COMMON_CONFIG_SIZE: u64 = 56;
pub const VIRTIO_PCI_ISR_CONFIG_OFFSET: u64 = 0x2000;
pub const VIRTIO_PCI_ISR_CONFIG_SIZE: u64 = 1;
pub const VIRTIO_PCI_DEVICE_CONFIG_OFFSET: u64 = 0x4000;
pub const VIRTIO_PCI_DEVICE_CONFIG_SIZE: u64 = 0x1000;
pub const VIRTIO_PCI_NOTIFICATION_OFFSET: u64 = 0x6000;
pub const VIRTIO_PCI_NOTIFICATION_SIZE: u64 = 0x1000;
pub const VIRTIO_PCI_NOTIFICATION_MULTIPLIER: u32 = 4;
pub const VIRTIO_PCI_MSIX_TABLE_OFFSET: u64 = 0x8000;
pub const VIRTIO_PCI_MSIX_TABLE_SIZE: u64 = 0x40000;
pub const VIRTIO_PCI_MSIX_PBA_OFFSET: u64 = 0x48000;
pub const VIRTIO_PCI_MSIX_PBA_SIZE: u64 = 0x800;

pub const VIRTIO_PCI_NO_VECTOR: u16 = u16::MAX;
pub const VIRTIO_PCI_MAX_MSIX_VECTORS: usize = 2048;

const VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT: u64 = 0x00;
const VIRTIO_PCI_COMMON_DEVICE_FEATURE: u64 = 0x04;
const VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT: u64 = 0x08;
const VIRTIO_PCI_COMMON_DRIVER_FEATURE: u64 = 0x0c;
const VIRTIO_PCI_COMMON_MSIX_CONFIG: u64 = 0x10;
const VIRTIO_PCI_COMMON_NUM_QUEUES: u64 = 0x12;
const VIRTIO_PCI_COMMON_DEVICE_STATUS: u64 = 0x14;
const VIRTIO_PCI_COMMON_CONFIG_GENERATION: u64 = 0x15;
const VIRTIO_PCI_COMMON_QUEUE_SELECT: u64 = 0x16;
const VIRTIO_PCI_COMMON_QUEUE_SIZE: u64 = 0x18;
const VIRTIO_PCI_COMMON_QUEUE_MSIX_VECTOR: u64 = 0x1a;
const VIRTIO_PCI_COMMON_QUEUE_ENABLE: u64 = 0x1c;
const VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF: u64 = 0x1e;
const VIRTIO_PCI_COMMON_QUEUE_DESC_LO: u64 = 0x20;
const VIRTIO_PCI_COMMON_QUEUE_DESC_HI: u64 = 0x24;
const VIRTIO_PCI_COMMON_QUEUE_AVAIL_LO: u64 = 0x28;
const VIRTIO_PCI_COMMON_QUEUE_AVAIL_HI: u64 = 0x2c;
const VIRTIO_PCI_COMMON_QUEUE_USED_LO: u64 = 0x30;
const VIRTIO_PCI_COMMON_QUEUE_USED_HI: u64 = 0x34;

const VIRTIO_PCI_CAP_COMMON: u8 = 1;
const VIRTIO_PCI_CAP_NOTIFY: u8 = 2;
const VIRTIO_PCI_CAP_ISR: u8 = 3;
const VIRTIO_PCI_CAP_DEVICE: u8 = 4;
const VIRTIO_PCI_CAP_PCI_CFG: u8 = 5;
const VIRTIO_PCI_GENERIC_CAP_TOTAL_SIZE: u8 = 16;
const VIRTIO_PCI_NOTIFY_CAP_TOTAL_SIZE: u8 = 20;
const VIRTIO_PCI_CFG_CAP_TOTAL_SIZE: u8 = 20;

const MSIX_FUNCTION_MASK: u16 = 1 << 14;
const MSIX_ENABLE: u16 = 1 << 15;
const MSIX_TABLE_ENTRY_SIZE: u64 = 16;
const MSIX_PBA_WORD_SIZE: u64 = 8;
const MSIX_BITS_PER_PBA_WORD: usize = 64;

const VIRTIO_DRIVER_READY_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
    | VIRTIO_DEVICE_STATUS_DRIVER
    | VIRTIO_DEVICE_STATUS_FEATURES_OK
    | VIRTIO_DEVICE_STATUS_DRIVER_OK;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioPciEndpointPhase {
    Active,
    Quiescing,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioPciIdentity {
    device_type: VirtioDeviceType,
    device_features: u64,
    config_generation: u32,
}

/// Value-redacted state used by the signed driver-conformance harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioPciDiagnostics {
    pub phase: VirtioPciEndpointPhase,
    pub device_activated: bool,
    pub driver_ready: bool,
    pub driver_features: u64,
    pub msix_enabled: bool,
    pub msix_function_masked: bool,
    pub programmed_msix_entries: usize,
    pub unmasked_msix_entries: usize,
    pub config_vector: Option<u16>,
    pub queue_vectors: Vec<Option<u16>>,
    pub pending_transition_observed: bool,
}

/// Complete detached modern virtio-pci transport state.
///
/// The value intentionally keeps guest-programmed registers private and uses a
/// redacted `Debug` implementation. It is an internal persistence handoff, not
/// a diagnostic surface.
#[derive(Clone, PartialEq, Eq)]
pub struct VirtioPciTransportState {
    phase: VirtioPciEndpointPhase,
    configuration: PciType0Configuration,
    pci_cfg_cap_offset: u16,
    msix_cap_offset: u16,
    pci_cfg_bar: u8,
    pci_cfg_offset: u32,
    pci_cfg_length: u32,
    device_feature_select: u32,
    driver_feature_select: u32,
    queue_select: u16,
    device: VirtioMmioDeviceRegisters,
    queues: VirtioQueues,
    queue_notifications: VirtioQueueNotificationState,
    device_activated: bool,
    requires_device_config_write_status: bool,
    interrupt_intents: Vec<VirtioInterruptIntent>,
    msix: VirtioPciMsixState,
}

impl VirtioPciTransportState {
    pub(crate) fn from_snapshot_v2_parts(
        identity: VirtioPciIdentity,
        common: &SnapshotV2VirtioState,
        pci: &SnapshotV2PciDeviceState,
        requires_device_config_write_status: bool,
    ) -> Result<Self, VirtioPciEndpointError> {
        let vector_count = pci.msix().entries().len();
        let (profile, profile_pci_cfg_offset, profile_msix_offset) =
            build_pci_snapshot_profile(identity.device_type, pci.bar_range(), vector_count)
                .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let (mut configuration, pci_cfg_cap_offset, msix_cap_offset) = build_pci_configuration(
            identity,
            pci.bar_range(),
            pci.bar_address_space(),
            vector_count,
        )?;
        if profile_pci_cfg_offset != pci_cfg_cap_offset || profile_msix_offset != msix_cap_offset {
            return Err(retained_error(VirtioPciRetainedStateError::Configuration));
        }

        let expected = configuration
            .snapshot_guest_state(&profile)
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let selector_bar_offset = pci_cfg_cap_offset
            .checked_add(4)
            .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let selector_offset_start = pci_cfg_cap_offset
            .checked_add(8)
            .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let selector_length_start = pci_cfg_cap_offset
            .checked_add(12)
            .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let data_start = pci_cfg_cap_offset
            .checked_add(16)
            .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let data_end = data_start
            .checked_add(4)
            .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let msix_control_high = msix_cap_offset
            .checked_add(3)
            .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let selector_offset = pci.pci_cfg_offset().to_le_bytes();
        let selector_length = pci.pci_cfg_length().to_le_bytes();

        let mut writable_bytes = Vec::new();
        writable_bytes
            .try_reserve_exact(expected.writable_bytes().len())
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;
        for writable in expected.writable_bytes().iter().copied() {
            let offset = writable.offset();
            let value = if offset == selector_bar_offset {
                pci.pci_cfg_bar()
            } else if (selector_offset_start..selector_length_start).contains(&offset) {
                selector_offset
                    .get(usize::from(offset - selector_offset_start))
                    .copied()
                    .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?
            } else if (selector_length_start..data_start).contains(&offset) {
                selector_length
                    .get(usize::from(offset - selector_length_start))
                    .copied()
                    .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?
            } else if (data_start..data_end).contains(&offset) {
                writable.value()
            } else if offset == msix_control_high {
                (u8::from(pci.msix().enabled()) << 7)
                    | (u8::from(pci.msix().function_masked()) << 6)
            } else {
                pci.writable_bytes()
                    .iter()
                    .copied()
                    .find(|candidate| candidate.offset() == offset)
                    .map(|candidate| candidate.value())
                    .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?
            };
            writable_bytes.push(
                writable
                    .with_value(value)
                    .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?,
            );
        }

        let mut bar_probes = Vec::new();
        bar_probes
            .try_reserve_exact(expected.bar_probes().len())
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;
        for probe in expected.bar_probes().iter().copied() {
            let pending = pci
                .bar_probes()
                .iter()
                .copied()
                .find(|candidate| candidate.index() == probe.index())
                .map(|candidate| candidate.pending())
                .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))?;
            bar_probes.push(probe.with_pending(pending));
        }
        let guest_state = PciType0GuestState::from_parts(writable_bytes, bar_probes);
        configuration
            .apply_guest_state(&profile, &guest_state)
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;

        let device = VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(
            identity.device_type.raw_value(),
            0,
            common.available_features(),
            common.config_generation(),
        )
        .with_runtime_state(
            [pci.device_feature_select(), pci.driver_feature_select()],
            common.driver_features(),
            common.status(),
        );
        let mut queue_states = Vec::new();
        queue_states
            .try_reserve_exact(common.queues().len())
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Queue))?;
        for queue in common.queues() {
            queue_states.push(crate::virtio::VirtioQueueState::from_parts(
                queue.max_size(),
                queue.size(),
                queue.ready(),
                queue.descriptor_table(),
                queue.driver_ring(),
                queue.device_ring(),
            ));
        }
        let queues = VirtioQueues::from_parts(u32::from(pci.queue_select()), queue_states);
        let mut pending_notifications = Vec::new();
        pending_notifications
            .try_reserve_exact(common.queues().len())
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Notification))?;
        pending_notifications.resize(common.queues().len(), false);
        for queue_index in common.pending_notifications().iter().copied() {
            let pending = pending_notifications
                .get_mut(usize::from(queue_index))
                .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Notification))?;
            *pending = true;
        }
        let queue_notifications = VirtioQueueNotificationState::from_parts(pending_notifications);
        let mut interrupt_intents = Vec::new();
        interrupt_intents
            .try_reserve_exact(common.interrupt_intents().len())
            .map_err(|_| retained_error(VirtioPciRetainedStateError::InterruptIntent))?;
        for intent in common.interrupt_intents() {
            interrupt_intents.push(match *intent {
                SnapshotV2InterruptIntent::Queue { queue_index } => {
                    VirtioInterruptIntent::Queue { queue_index }
                }
                SnapshotV2InterruptIntent::Configuration => VirtioInterruptIntent::Configuration,
            });
        }
        let mut msix_entries = Vec::new();
        msix_entries
            .try_reserve_exact(pci.msix().entries().len())
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Msix))?;
        for entry in pci.msix().entries() {
            msix_entries.push(VirtioPciMsixTableEntry::from_parts(
                entry.message_address_low(),
                entry.message_address_high(),
                entry.message_data(),
                entry.vector_control(),
            ));
        }
        let mut pending_words = Vec::new();
        pending_words
            .try_reserve_exact(pci.msix().pending_words().len())
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Msix))?;
        pending_words.extend_from_slice(pci.msix().pending_words());
        let mut queue_vectors = Vec::new();
        queue_vectors
            .try_reserve_exact(pci.msix().queue_vectors().len())
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Msix))?;
        queue_vectors.extend_from_slice(pci.msix().queue_vectors());
        let msix = VirtioPciMsixState::from_parts(
            msix_entries,
            pending_words,
            pci.msix().enabled(),
            pci.msix().function_masked(),
            pci.msix().config_vector(),
            queue_vectors,
            pci.msix().pending_transition_observed(),
        );

        let retained = Self {
            phase: pci.phase(),
            configuration,
            pci_cfg_cap_offset,
            msix_cap_offset,
            pci_cfg_bar: pci.pci_cfg_bar(),
            pci_cfg_offset: pci.pci_cfg_offset(),
            pci_cfg_length: pci.pci_cfg_length(),
            device_feature_select: pci.device_feature_select(),
            driver_feature_select: pci.driver_feature_select(),
            queue_select: pci.queue_select(),
            device,
            queues,
            queue_notifications,
            device_activated: common.is_activated(),
            requires_device_config_write_status,
            interrupt_intents,
            msix,
        };
        Ok(retained)
    }

    pub const fn phase(&self) -> VirtioPciEndpointPhase {
        self.phase
    }

    pub const fn configuration(&self) -> &PciType0Configuration {
        &self.configuration
    }

    pub const fn pci_cfg_cap_offset(&self) -> u16 {
        self.pci_cfg_cap_offset
    }

    pub const fn msix_cap_offset(&self) -> u16 {
        self.msix_cap_offset
    }

    pub const fn pci_cfg_bar(&self) -> u8 {
        self.pci_cfg_bar
    }

    pub const fn pci_cfg_offset(&self) -> u32 {
        self.pci_cfg_offset
    }

    pub const fn pci_cfg_length(&self) -> u32 {
        self.pci_cfg_length
    }

    pub const fn device_feature_select(&self) -> u32 {
        self.device_feature_select
    }

    pub const fn driver_feature_select(&self) -> u32 {
        self.driver_feature_select
    }

    pub const fn queue_select(&self) -> u16 {
        self.queue_select
    }

    pub const fn device_registers(&self) -> &VirtioMmioDeviceRegisters {
        &self.device
    }

    pub const fn queues(&self) -> &VirtioQueues {
        &self.queues
    }

    pub const fn queue_notifications(&self) -> &VirtioQueueNotificationState {
        &self.queue_notifications
    }

    pub const fn is_device_activated(&self) -> bool {
        self.device_activated
    }

    pub const fn requires_device_config_write_status(&self) -> bool {
        self.requires_device_config_write_status
    }

    pub fn interrupt_intents(&self) -> &[VirtioInterruptIntent] {
        &self.interrupt_intents
    }

    pub fn msix_vector_count(&self) -> usize {
        self.msix.vector_count()
    }

    pub const fn msix_state(&self) -> &VirtioPciMsixState {
        &self.msix
    }

    pub(crate) fn checked_configuration_guest_state(
        &self,
        bar_range: GuestMemoryRange,
    ) -> Result<PciType0GuestState, VirtioPciConfigurationSnapshotError> {
        if bar_range.size() != VIRTIO_PCI_CAPABILITY_BAR_SIZE {
            return Err(VirtioPciConfigurationSnapshotError::BarSize);
        }
        let device_type = VirtioDeviceType::new(self.device.device_id())
            .map_err(VirtioPciConfigurationSnapshotError::DeviceType)?;
        let (profile, pci_cfg_cap_offset, msix_cap_offset) =
            build_pci_snapshot_profile(device_type, bar_range, self.msix.vector_count())?;
        if self.pci_cfg_cap_offset != pci_cfg_cap_offset || self.msix_cap_offset != msix_cap_offset
        {
            return Err(VirtioPciConfigurationSnapshotError::CapabilityOffset);
        }
        self.configuration
            .snapshot_guest_state(&profile)
            .map_err(VirtioPciConfigurationSnapshotError::Configuration)
    }
}

fn build_pci_snapshot_profile(
    device_type: VirtioDeviceType,
    bar_range: GuestMemoryRange,
    vector_count: usize,
) -> Result<(PciType0SnapshotProfile, u16, u16), VirtioPciConfigurationSnapshotError> {
    if bar_range.size() != VIRTIO_PCI_CAPABILITY_BAR_SIZE {
        return Err(VirtioPciConfigurationSnapshotError::BarSize);
    }
    let (class_code, subclass) = pci_class(device_type);
    let device_id = device_type.modern_pci_device_id();
    let mut profile = PciType0SnapshotProfile::new(
        VIRTIO_PCI_VENDOR_ID,
        device_id,
        VIRTIO_PCI_REVISION_ID,
        class_code,
        subclass,
        0,
        VIRTIO_PCI_VENDOR_ID,
        device_id,
    );
    profile
        .install_bar(
            VIRTIO_PCI_CAPABILITY_BAR_INDEX,
            bar_range,
            PciBarAddressSpace::Memory64,
            PciBarPrefetchable::No,
        )
        .map_err(VirtioPciConfigurationSnapshotError::Bar)?;
    let (pci_cfg_cap_offset, msix_cap_offset) =
        add_virtio_pci_snapshot_capabilities(&mut profile, vector_count)?;
    Ok((profile, pci_cfg_cap_offset, msix_cap_offset))
}

pub(crate) enum VirtioPciConfigurationSnapshotError {
    DeviceType(VirtioDeviceTypeError),
    BarSize,
    Bar(PciBarConfigurationError),
    TooManyVectors,
    Capability(PciCapabilityError),
    CapabilityOffset,
    Configuration(PciType0SnapshotError),
}

impl fmt::Debug for VirtioPciConfigurationSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = match self {
            Self::DeviceType(_) => "DeviceType",
            Self::BarSize => "BarSize",
            Self::Bar(_) => "Bar",
            Self::TooManyVectors => "TooManyVectors",
            Self::Capability(_) => "Capability",
            Self::CapabilityOffset => "CapabilityOffset",
            Self::Configuration(_) => "Configuration",
        };
        formatter
            .debug_struct("VirtioPciConfigurationSnapshotError")
            .field("kind", &kind)
            .field("detail", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for VirtioPciConfigurationSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceType(_) => {
                formatter.write_str("virtio-pci snapshot device type is invalid")
            }
            Self::BarSize => {
                formatter.write_str("virtio-pci snapshot capability BAR size is invalid")
            }
            Self::Bar(_) => formatter.write_str("virtio-pci snapshot BAR profile is invalid"),
            Self::TooManyVectors => {
                formatter.write_str("virtio-pci snapshot vector count is invalid")
            }
            Self::Capability(_) => {
                formatter.write_str("virtio-pci snapshot capability profile is invalid")
            }
            Self::CapabilityOffset => {
                formatter.write_str("virtio-pci snapshot capability offsets do not match")
            }
            Self::Configuration(_) => {
                formatter.write_str("virtio-pci snapshot configuration profile does not match")
            }
        }
    }
}

impl std::error::Error for VirtioPciConfigurationSnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DeviceType(source) => Some(source),
            Self::Bar(source) => Some(source),
            Self::Capability(source) => Some(source),
            Self::Configuration(source) => Some(source),
            Self::BarSize | Self::TooManyVectors | Self::CapabilityOffset => None,
        }
    }
}

impl fmt::Debug for VirtioPciTransportState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioPciTransportState")
            .field("state", &"<redacted>")
            .finish()
    }
}

impl VirtioPciIdentity {
    pub const fn new(device_type: VirtioDeviceType, device_features: u64) -> Self {
        Self {
            device_type,
            device_features,
            config_generation: 0,
        }
    }

    pub const fn with_config_generation(mut self, config_generation: u32) -> Self {
        self.config_generation = config_generation;
        self
    }

    pub const fn device_type(self) -> VirtioDeviceType {
        self.device_type
    }
}

pub struct VirtioPciEndpoint<C, A> {
    inner: Arc<VirtioPciEndpointInner<C, A>>,
}

/// One fully validated retained endpoint awaiting exact resource publication.
pub struct PreparedVirtioPciEndpoint<C, A> {
    endpoint: VirtioPciEndpoint<C, A>,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
    region_id: MmioRegionId,
}

/// One admitted device-work transaction for a modern virtio-pci endpoint.
///
/// Teardown closes admission and waits for every retained transaction before
/// revoking the endpoint and its message routes. Methods on this value remain
/// usable while teardown is waiting, so already admitted queue work can publish
/// its used ring and interrupt as one ordered operation.
pub struct VirtioPciEndpointWork<'a, C, A> {
    endpoint: &'a VirtioPciEndpoint<C, A>,
    _guard: VirtioDeviceWorkGuard,
}

impl<C, A> fmt::Debug for VirtioPciEndpointWork<'_, C, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtioPciEndpointWork")
            .field("endpoint", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl<C, A> Clone for VirtioPciEndpoint<C, A> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

struct VirtioPciEndpointInner<C, A> {
    state: Mutex<VirtioPciEndpointState<C, A>>,
    messages: GuestMessageInterruptRegistry,
    interrupt_success_logging_enabled: AtomicBool,
}

struct VirtioPciEndpointState<C, A> {
    phase: VirtioPciEndpointPhase,
    configuration: PciType0Configuration,
    pci_cfg_cap_offset: u16,
    msix_cap_offset: u16,
    pci_cfg_bar: u8,
    pci_cfg_offset: u32,
    pci_cfg_length: u32,
    device_feature_select: u32,
    driver_feature_select: u32,
    queue_select: u16,
    core: VirtioDeviceCore<C, A>,
    msix: VirtioPciMsixState,
    guest_logger: GuestLogger,
    queue_notification_success_logging_enabled: bool,
}

impl<C, A> fmt::Debug for VirtioPciEndpoint<C, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtioPciEndpoint")
            .field("state", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl<C, A> fmt::Debug for PreparedVirtioPciEndpoint<C, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedVirtioPciEndpoint")
            .field("state", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl<C, A> fmt::Debug for VirtioPciEndpointInner<C, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtioPciEndpointInner")
            .field("state", &"<redacted>")
            .field("messages", &self.messages)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct VirtioPciConfigFunction<C, A> {
    inner: Arc<VirtioPciEndpointInner<C, A>>,
}

impl<C, A> fmt::Debug for VirtioPciConfigFunction<C, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtioPciConfigFunction")
            .field("endpoint", &"<redacted>")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct VirtioPciBarHandler<C, A> {
    inner: Arc<VirtioPciEndpointInner<C, A>>,
    guest_logger: GuestLogger,
    device_kind: Option<LoggerDeviceKind>,
}

impl<C, A> fmt::Debug for VirtioPciBarHandler<C, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtioPciBarHandler")
            .field("endpoint", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl<C: VirtioDeviceConfigHandler, A: VirtioDeviceActivationHandler> VirtioPciEndpoint<C, A> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: VirtioPciIdentity,
        queue_max_sizes: &[u16],
        device_config: C,
        activation: A,
        requires_device_config_write_status: bool,
        capability_bar: &PciBarLease,
        messages: GuestMessageInterruptRegistry,
    ) -> Result<Self, VirtioPciEndpointError> {
        let queues = VirtioQueues::new(queue_max_sizes)
            .map_err(|source| VirtioPciEndpointError::QueueInitialization { source })?;
        let queue_count = queues.queue_count();
        let vector_count = queue_count
            .checked_add(1)
            .ok_or(VirtioPciEndpointError::VectorCountOverflow)?;
        if vector_count > VIRTIO_PCI_MAX_MSIX_VECTORS {
            return Err(VirtioPciEndpointError::TooManyVectors { vector_count });
        }
        if messages.route_count() < vector_count {
            return Err(VirtioPciEndpointError::MessageRouteCount {
                expected: vector_count,
                actual: messages.route_count(),
            });
        }
        let message_phase = messages
            .phase()
            .map_err(|source| VirtioPciEndpointError::MessageRegistry { source })?;
        if message_phase != GuestMessageInterruptRegistryPhase::Active {
            return Err(VirtioPciEndpointError::MessageRegistry {
                source: GuestMessageInterruptRegistryError::NotActive {
                    phase: message_phase,
                },
            });
        }
        let notifications = VirtioQueueNotificationState::new(queue_count)
            .map_err(|source| VirtioPciEndpointError::QueueNotificationInitialization { source })?;
        let device = VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(
            identity.device_type.raw_value(),
            0,
            identity.device_features,
            identity.config_generation,
        );
        let core = VirtioDeviceCore::from_parts(
            device,
            queues,
            notifications,
            device_config,
            activation,
            requires_device_config_write_status,
        );

        let (configuration, pci_cfg_cap_offset, msix_cap_offset) = build_pci_configuration(
            identity,
            capability_bar.range(),
            capability_bar.address_space(),
            vector_count,
        )?;

        Ok(Self {
            inner: Arc::new(VirtioPciEndpointInner {
                state: Mutex::new(VirtioPciEndpointState {
                    phase: VirtioPciEndpointPhase::Active,
                    configuration,
                    pci_cfg_cap_offset,
                    msix_cap_offset,
                    pci_cfg_bar: 0,
                    pci_cfg_offset: 0,
                    pci_cfg_length: 0,
                    device_feature_select: 0,
                    driver_feature_select: 0,
                    queue_select: 0,
                    core,
                    msix: VirtioPciMsixState::new(vector_count, queue_count),
                    guest_logger: GuestLogger::default(),
                    queue_notification_success_logging_enabled: false,
                }),
                messages,
                interrupt_success_logging_enabled: AtomicBool::new(false),
            }),
        })
    }

    pub fn config_function(&self) -> VirtioPciConfigFunction<C, A> {
        VirtioPciConfigFunction {
            inner: Arc::clone(&self.inner),
        }
    }

    pub fn bar_handler(&self) -> VirtioPciBarHandler<C, A> {
        let logging = self.logging_context();
        VirtioPciBarHandler {
            inner: Arc::clone(&self.inner),
            guest_logger: logging
                .as_ref()
                .map_or_else(GuestLogger::default, |(logger, _)| logger.clone()),
            device_kind: logging.map(|(_, kind)| kind),
        }
    }

    fn attach_guest_logger(&self, logger: GuestLogger) {
        if let Ok(mut state) = self.inner.state.lock() {
            let interrupt_success_logging_enabled = LoggerDeviceKind::from_virtio_device_id(
                state.core.device.device_id(),
            )
            .is_some_and(|kind| {
                logger.transport_outcome_enabled(LoggerTransportOutcome::InterruptDelivered(kind))
            });
            state.queue_notification_success_logging_enabled =
                LoggerDeviceKind::from_virtio_device_id(state.core.device.device_id()).is_some_and(
                    |kind| {
                        logger.transport_outcome_enabled(
                            LoggerTransportOutcome::QueueNotificationSucceeded(kind),
                        )
                    },
                );
            state.core.activation.attach_guest_logger(logger.clone());
            state.guest_logger = logger;
            self.inner
                .interrupt_success_logging_enabled
                .store(interrupt_success_logging_enabled, Ordering::Release);
        }
    }

    fn logging_context(&self) -> Option<(GuestLogger, LoggerDeviceKind)> {
        let state = self.inner.state.lock().ok()?;
        state.logging_context()
    }

    pub fn phase(&self) -> Result<VirtioPciEndpointPhase, VirtioPciEndpointError> {
        self.inner
            .state
            .lock()
            .map(|state| state.phase)
            .map_err(|_| VirtioPciEndpointError::StatePoisoned)
    }

    /// Returns transport state without disclosing guest-programmed messages.
    pub fn diagnostics(&self) -> Result<VirtioPciDiagnostics, VirtioPciEndpointError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
        let config_vector =
            (state.msix.config_vector != VIRTIO_PCI_NO_VECTOR).then_some(state.msix.config_vector);
        let queue_vectors = state
            .msix
            .queue_vectors
            .iter()
            .copied()
            .map(|vector| (vector != VIRTIO_PCI_NO_VECTOR).then_some(vector))
            .collect();
        Ok(VirtioPciDiagnostics {
            phase: state.phase,
            device_activated: state.core.device_activated,
            driver_ready: state.core.device.status() == VIRTIO_DRIVER_READY_STATUS,
            driver_features: state.core.device.driver_features(),
            msix_enabled: state.msix.enabled,
            msix_function_masked: state.msix.function_masked,
            programmed_msix_entries: state
                .msix
                .entries
                .iter()
                .filter(|entry| {
                    entry.message_address_low != 0
                        || entry.message_address_high != 0
                        || entry.message_data != 0
                })
                .count(),
            unmasked_msix_entries: state
                .msix
                .entries
                .iter()
                .filter(|entry| !entry.is_masked())
                .count(),
            config_vector,
            queue_vectors,
            pending_transition_observed: state.msix.pending_transition_observed,
        })
    }

    /// Clones the complete canonical transport state under the endpoint lock.
    pub fn transport_state(&self) -> Result<VirtioPciTransportState, VirtioPciEndpointError> {
        let state = self.lock_active()?;
        Ok(clone_transport_state(&state))
    }

    /// Captures device and transport state from one canonical endpoint observation.
    pub(crate) fn capture_transport_with<R>(
        &self,
        capture: impl FnOnce(&VirtioMmioDeviceRegisters, &VirtioQueues, &C, &A, bool) -> R,
    ) -> Result<(R, VirtioPciTransportState), VirtioPciEndpointError> {
        let state = self.lock_active()?;
        let device = capture(
            &state.core.device,
            &state.core.queues,
            &state.core.device_config,
            &state.core.activation,
            state.core.device_activated,
        );
        let transport = clone_transport_state(&state);
        Ok((device, transport))
    }

    /// Captures device and transport state while permitting one validated
    /// device-local source normalization under the same endpoint lock.
    pub(crate) fn capture_transport_with_mut<R>(
        &self,
        capture: impl FnOnce(&VirtioMmioDeviceRegisters, &VirtioQueues, &C, &mut A, bool) -> R,
    ) -> Result<(R, VirtioPciTransportState), VirtioPciEndpointError> {
        let mut state = self.lock_active()?;
        let core = &mut state.core;
        let device = capture(
            &core.device,
            &core.queues,
            &core.device_config,
            &mut core.activation,
            core.device_activated,
        );
        let transport = clone_transport_state(&state);
        Ok((device, transport))
    }

    pub fn admit_device_work(
        &self,
    ) -> Result<VirtioPciEndpointWork<'_, C, A>, VirtioPciEndpointError> {
        let gate = {
            let state = self.lock_active()?;
            state.core.work_gate().clone()
        };
        let guard = gate
            .admit()
            .map_err(|source| VirtioPciEndpointError::WorkGate { source })?;
        Ok(VirtioPciEndpointWork {
            endpoint: self,
            _guard: guard,
        })
    }

    pub fn pending_queue_notifications(&self) -> Result<Vec<usize>, VirtioPciEndpointError> {
        self.admit_device_work()?.pending_queue_notifications()
    }

    pub fn take_pending_queue_notifications(&self) -> Result<Vec<usize>, VirtioPciEndpointError> {
        self.admit_device_work()?.take_pending_queue_notifications()
    }

    pub fn trigger(&self, intent: VirtioInterruptIntent) -> Result<(), VirtioPciEndpointError> {
        self.admit_device_work()?.trigger(intent)
    }

    pub fn drain_interrupt_intents(&self) -> Result<(), VirtioPciEndpointError> {
        self.admit_device_work()?.drain_interrupt_intents()
    }

    pub fn increment_config_generation_and_trigger(&self) -> Result<(), VirtioPciEndpointError> {
        self.admit_device_work()?
            .increment_config_generation_and_trigger()
    }

    pub fn begin_quiesce(&self) -> Result<(), VirtioPciEndpointError> {
        let gate = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
            match state.phase {
                VirtioPciEndpointPhase::Active | VirtioPciEndpointPhase::Quiescing => {}
                VirtioPciEndpointPhase::Released => {
                    return Err(VirtioPciEndpointError::NotActive { phase: state.phase });
                }
            }
            state.core.work_gate().clone()
        };
        gate.quiesce_and_wait()
            .map_err(|source| VirtioPciEndpointError::WorkGate { source })?;
        if let Err(source) = self.inner.messages.quiesce_and_wait() {
            let _ = gate.resume();
            return Err(VirtioPciEndpointError::MessageRegistry { source });
        }
        let mut state = match self.inner.state.lock() {
            Ok(state) => state,
            Err(_) => {
                let _ = self.inner.messages.resume();
                let _ = gate.resume();
                return Err(VirtioPciEndpointError::StatePoisoned);
            }
        };
        if state.phase == VirtioPciEndpointPhase::Released {
            let phase = state.phase;
            drop(state);
            let _ = self.inner.messages.resume();
            let _ = gate.resume();
            return Err(VirtioPciEndpointError::NotActive { phase });
        }
        state.phase = VirtioPciEndpointPhase::Quiescing;
        Ok(())
    }

    /// Restores a quiesced endpoint after a recoverable removal abort.
    pub fn resume(&self) -> Result<(), VirtioPciEndpointError> {
        let gate = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
            match state.phase {
                VirtioPciEndpointPhase::Active => return Ok(()),
                VirtioPciEndpointPhase::Quiescing => state.core.work_gate().clone(),
                VirtioPciEndpointPhase::Released => {
                    return Err(VirtioPciEndpointError::NotActive { phase: state.phase });
                }
            }
        };
        self.inner
            .messages
            .resume()
            .map_err(|source| VirtioPciEndpointError::MessageRegistry { source })?;
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
            if state.phase != VirtioPciEndpointPhase::Quiescing {
                let phase = state.phase;
                drop(state);
                let _ = self.inner.messages.begin_quiesce();
                return Err(VirtioPciEndpointError::NotActive { phase });
            }
            state.phase = VirtioPciEndpointPhase::Active;
        }
        if let Err(source) = gate.resume() {
            if let Ok(mut state) = self.inner.state.lock() {
                state.phase = VirtioPciEndpointPhase::Quiescing;
            }
            let _ = self.inner.messages.begin_quiesce();
            return Err(VirtioPciEndpointError::WorkGate { source });
        }
        Ok(())
    }

    pub fn release(&self) -> Result<(), VirtioPciEndpointError> {
        let gate = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
            if state.phase == VirtioPciEndpointPhase::Released {
                return Ok(());
            }
            state.core.work_gate().clone()
        };
        gate.quiesce_and_wait()
            .map_err(|source| VirtioPciEndpointError::WorkGate { source })?;
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
            if state.phase == VirtioPciEndpointPhase::Released {
                return Ok(());
            }
            state.phase = VirtioPciEndpointPhase::Quiescing;
        }
        self.inner
            .messages
            .release()
            .map_err(|source| VirtioPciEndpointError::MessageRegistry { source })?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
        state.phase = VirtioPciEndpointPhase::Released;
        Ok(())
    }

    /// Runs one transport-owned operation after guest paths and ordinary work
    /// admission have been quiesced for teardown.
    pub(crate) fn with_quiesced_core_mut<R>(
        &self,
        operation: impl FnOnce(&mut VirtioDeviceCore<C, A>) -> R,
    ) -> Result<R, VirtioPciEndpointError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
        if state.phase != VirtioPciEndpointPhase::Quiescing {
            return Err(VirtioPciEndpointError::NotActive { phase: state.phase });
        }
        Ok(operation(&mut state.core))
    }

    fn lock_active(
        &self,
    ) -> Result<MutexGuard<'_, VirtioPciEndpointState<C, A>>, VirtioPciEndpointError> {
        let state = self
            .inner
            .state
            .lock()
            .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
        if state.phase != VirtioPciEndpointPhase::Active {
            return Err(VirtioPciEndpointError::NotActive { phase: state.phase });
        }
        Ok(state)
    }
}

impl<C: VirtioDeviceConfigHandler, A: VirtioDeviceActivationHandler>
    PreparedVirtioPciEndpoint<C, A>
{
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: VirtioPciIdentity,
        queue_max_sizes: &[u16],
        device_config: C,
        activation: A,
        activation_is_active: bool,
        requires_device_config_write_status: bool,
        retained: &VirtioPciTransportState,
        sbdf: PciSbdf,
        bar_range: GuestMemoryRange,
        region_id: MmioRegionId,
        messages: GuestMessageInterruptRegistry,
    ) -> Result<Self, VirtioPciEndpointError> {
        validate_retained_transport_state(
            retained,
            identity,
            queue_max_sizes,
            activation_is_active,
            requires_device_config_write_status,
            sbdf,
            bar_range,
            &messages,
        )?;

        let guest_state = retained
            .checked_configuration_guest_state(bar_range)
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let (profile, profile_pci_cfg_offset, profile_msix_offset) = build_pci_snapshot_profile(
            identity.device_type,
            bar_range,
            retained.msix.vector_count(),
        )
        .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;
        let (mut configuration, pci_cfg_cap_offset, msix_cap_offset) = build_pci_configuration(
            identity,
            bar_range,
            PciBarAddressSpace::Memory64,
            retained.msix.vector_count(),
        )?;
        if profile_pci_cfg_offset != pci_cfg_cap_offset
            || profile_msix_offset != msix_cap_offset
            || retained.pci_cfg_cap_offset != pci_cfg_cap_offset
            || retained.msix_cap_offset != msix_cap_offset
        {
            return Err(retained_error(VirtioPciRetainedStateError::Configuration));
        }
        configuration
            .apply_guest_state(&profile, &guest_state)
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;

        let mut core = VirtioDeviceCore::from_parts(
            retained.device,
            retained.queues.clone(),
            retained.queue_notifications.clone(),
            device_config,
            activation,
            requires_device_config_write_status,
        );
        core.device_activated = retained.device_activated;
        core.interrupt_intents = retained.interrupt_intents.clone();

        let endpoint = VirtioPciEndpoint {
            inner: Arc::new(VirtioPciEndpointInner {
                state: Mutex::new(VirtioPciEndpointState {
                    phase: VirtioPciEndpointPhase::Active,
                    configuration,
                    pci_cfg_cap_offset,
                    msix_cap_offset,
                    pci_cfg_bar: retained.pci_cfg_bar,
                    pci_cfg_offset: retained.pci_cfg_offset,
                    pci_cfg_length: retained.pci_cfg_length,
                    device_feature_select: retained.device_feature_select,
                    driver_feature_select: retained.driver_feature_select,
                    queue_select: retained.queue_select,
                    core,
                    msix: retained.msix.clone(),
                    guest_logger: GuestLogger::default(),
                    queue_notification_success_logging_enabled: false,
                }),
                messages,
                interrupt_success_logging_enabled: AtomicBool::new(false),
            }),
        };
        if endpoint.transport_state()? != *retained {
            return Err(retained_error(VirtioPciRetainedStateError::Recapture));
        }
        Ok(Self {
            endpoint,
            sbdf,
            bar_range,
            region_id,
        })
    }

    /// Returns the reconstructed endpoint before exact resource publication.
    pub const fn endpoint(&self) -> &VirtioPciEndpoint<C, A> {
        &self.endpoint
    }

    pub const fn sbdf(&self) -> PciSbdf {
        self.sbdf
    }

    pub const fn bar_range(&self) -> GuestMemoryRange {
        self.bar_range
    }

    pub const fn region_id(&self) -> MmioRegionId {
        self.region_id
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_retained_transport_state(
    retained: &VirtioPciTransportState,
    identity: VirtioPciIdentity,
    queue_max_sizes: &[u16],
    activation_is_active: bool,
    requires_device_config_write_status: bool,
    sbdf: PciSbdf,
    bar_range: GuestMemoryRange,
    messages: &GuestMessageInterruptRegistry,
) -> Result<(), VirtioPciEndpointError> {
    if retained.phase != VirtioPciEndpointPhase::Active {
        return Err(retained_error(VirtioPciRetainedStateError::Phase));
    }
    PciSegment::validate_function_identity(sbdf)
        .map_err(|_| retained_error(VirtioPciRetainedStateError::Sbdf))?;
    let expected_features = identity.device_features | VIRTIO_MMIO_VERSION_1_FEATURE;
    if retained.device.device_id() != identity.device_type.raw_value()
        || retained.device.vendor_id() != 0
        || retained.device.config_generation() != identity.config_generation
    {
        return Err(retained_error(VirtioPciRetainedStateError::DeviceIdentity));
    }
    if retained.device.device_features() != expected_features
        || retained.device.driver_features() & !expected_features != 0
        || retained.device.device_features_select() > 1
        || retained.device.driver_features_select() > 1
        || (retained.device.status() & VIRTIO_DEVICE_STATUS_DRIVER == 0
            && retained.device.driver_features() != 0)
        || (retained.device.status() & VIRTIO_DEVICE_STATUS_FEATURES_OK != 0
            && retained.device.driver_features() & VIRTIO_MMIO_VERSION_1_FEATURE == 0)
    {
        return Err(retained_error(VirtioPciRetainedStateError::Features));
    }
    if !retained_status_is_healthy(retained.device.status()) {
        return Err(retained_error(VirtioPciRetainedStateError::Status));
    }
    let status_is_active = retained.device.status() == VIRTIO_DRIVER_READY_STATUS;
    if retained.device_activated != status_is_active
        || retained.device_activated != activation_is_active
    {
        return Err(retained_error(VirtioPciRetainedStateError::Activation));
    }
    if retained.requires_device_config_write_status != requires_device_config_write_status {
        return Err(retained_error(
            VirtioPciRetainedStateError::DeviceConfigPolicy,
        ));
    }
    if bar_range.size() != VIRTIO_PCI_CAPABILITY_BAR_SIZE
        || bar_range
            .validate_alignment(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .is_err()
    {
        return Err(retained_error(VirtioPciRetainedStateError::Bar));
    }

    validate_retained_queues(retained, queue_max_sizes)?;
    validate_retained_notifications(retained)?;
    validate_retained_interrupt_intents(retained)?;
    validate_retained_msix(retained, messages)?;
    validate_retained_configuration_relationships(retained)?;
    retained
        .checked_configuration_guest_state(bar_range)
        .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;
    Ok(())
}

fn validate_retained_queues(
    retained: &VirtioPciTransportState,
    queue_max_sizes: &[u16],
) -> Result<(), VirtioPciEndpointError> {
    let queue_count = retained.queues.queue_count();
    if queue_count == 0
        || queue_count != queue_max_sizes.len()
        || usize::try_from(retained.queues.queue_select())
            .ok()
            .is_none_or(|selector| selector >= queue_count)
    {
        return Err(retained_error(VirtioPciRetainedStateError::Queue));
    }
    let range_capacity = queue_count
        .checked_mul(3)
        .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Queue))?;
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(range_capacity)
        .map_err(|_| retained_error(VirtioPciRetainedStateError::Queue))?;
    for (index, expected_max_size) in queue_max_sizes.iter().copied().enumerate() {
        let queue = retained
            .queues
            .queue(
                u32::try_from(index)
                    .map_err(|_| retained_error(VirtioPciRetainedStateError::Queue))?,
            )
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Queue))?;
        if queue.max_size() != expected_max_size
            || expected_max_size == 0
            || !expected_max_size.is_power_of_two()
            || (queue.size() != 0
                && (!queue.size().is_power_of_two() || queue.size() > expected_max_size))
            || (queue.ready() && queue.size() == 0)
            || (retained.device_activated && !queue.ready())
            || !queue
                .descriptor_table()
                .raw_value()
                .is_multiple_of(VIRTQUEUE_DESCRIPTOR_ALIGNMENT)
            || !queue
                .driver_ring()
                .raw_value()
                .is_multiple_of(VIRTQUEUE_AVAILABLE_RING_ALIGNMENT)
            || !queue
                .device_ring()
                .raw_value()
                .is_multiple_of(VIRTQUEUE_USED_RING_ALIGNMENT)
        {
            return Err(retained_error(VirtioPciRetainedStateError::Queue));
        }
        if retained.device.status() & VIRTIO_DEVICE_STATUS_FEATURES_OK == 0
            && (queue.size() != 0
                || queue.ready()
                || queue.descriptor_table().raw_value() != 0
                || queue.driver_ring().raw_value() != 0
                || queue.device_ring().raw_value() != 0)
        {
            return Err(retained_error(VirtioPciRetainedStateError::Queue));
        }
        if queue.size() != 0 {
            ranges.extend_from_slice(&retained_queue_ranges(*queue)?);
        }
    }
    for (first_index, first) in ranges.iter().copied().enumerate() {
        for second in ranges.iter().copied().skip(first_index + 1) {
            if first.overlaps(second) {
                return Err(retained_error(VirtioPciRetainedStateError::Queue));
            }
        }
    }
    Ok(())
}

fn retained_queue_ranges(
    queue: crate::virtio::VirtioQueueState,
) -> Result<[GuestMemoryRange; 3], VirtioPciEndpointError> {
    let size = u64::from(queue.size());
    let descriptor_size = size
        .checked_mul(16)
        .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Queue))?;
    let available_size = size
        .checked_mul(2)
        .and_then(|size| size.checked_add(6))
        .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Queue))?;
    let used_size = size
        .checked_mul(8)
        .and_then(|size| size.checked_add(6))
        .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Queue))?;
    Ok([
        GuestMemoryRange::new(queue.descriptor_table(), descriptor_size)
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Queue))?,
        GuestMemoryRange::new(queue.driver_ring(), available_size)
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Queue))?,
        GuestMemoryRange::new(queue.device_ring(), used_size)
            .map_err(|_| retained_error(VirtioPciRetainedStateError::Queue))?,
    ])
}

fn validate_retained_notifications(
    retained: &VirtioPciTransportState,
) -> Result<(), VirtioPciEndpointError> {
    if retained.queue_notifications.queue_count() != retained.queues.queue_count()
        || (!retained.device_activated
            && retained
                .queue_notifications
                .has_pending_queue_notifications())
    {
        Err(retained_error(VirtioPciRetainedStateError::Notification))
    } else {
        Ok(())
    }
}

fn validate_retained_interrupt_intents(
    retained: &VirtioPciTransportState,
) -> Result<(), VirtioPciEndpointError> {
    for (index, intent) in retained.interrupt_intents.iter().enumerate() {
        if retained
            .interrupt_intents
            .iter()
            .take(index)
            .any(|previous| previous == intent)
            || matches!(
                intent,
                VirtioInterruptIntent::Queue { queue_index }
                    if usize::from(*queue_index) >= retained.queues.queue_count()
            )
        {
            return Err(retained_error(VirtioPciRetainedStateError::InterruptIntent));
        }
    }
    Ok(())
}

fn validate_retained_msix(
    retained: &VirtioPciTransportState,
    messages: &GuestMessageInterruptRegistry,
) -> Result<(), VirtioPciEndpointError> {
    let queue_count = retained.queues.queue_count();
    let vector_count = queue_count
        .checked_add(1)
        .ok_or(VirtioPciEndpointError::VectorCountOverflow)?;
    if vector_count > VIRTIO_PCI_MAX_MSIX_VECTORS
        || retained.msix.entries.len() != vector_count
        || retained.msix.pending.len() != vector_count.div_ceil(MSIX_BITS_PER_PBA_WORD)
        || retained.msix.queue_vectors.len() != queue_count
        || !valid_retained_vector(retained.msix.config_vector, vector_count)
        || retained
            .msix
            .queue_vectors
            .iter()
            .copied()
            .any(|vector| !valid_retained_vector(vector, vector_count))
        || retained
            .msix
            .entries
            .iter()
            .any(|entry| entry.vector_control & !1 != 0)
    {
        return Err(retained_error(VirtioPciRetainedStateError::Msix));
    }
    if let Some(last) = retained.msix.pending.last().copied() {
        let used_bits = vector_count % MSIX_BITS_PER_PBA_WORD;
        if used_bits != 0 && last & !((1_u64 << used_bits) - 1) != 0 {
            return Err(retained_error(VirtioPciRetainedStateError::Msix));
        }
    }
    if messages.route_count() < vector_count {
        return Err(VirtioPciEndpointError::MessageRouteCount {
            expected: vector_count,
            actual: messages.route_count(),
        });
    }
    let phase = messages
        .phase()
        .map_err(|source| VirtioPciEndpointError::MessageRegistry { source })?;
    if phase != GuestMessageInterruptRegistryPhase::Active {
        return Err(VirtioPciEndpointError::MessageRegistry {
            source: GuestMessageInterruptRegistryError::NotActive { phase },
        });
    }

    let mut referenced = vec![false; vector_count];
    if retained.msix.config_vector != VIRTIO_PCI_NO_VECTOR {
        *referenced
            .get_mut(usize::from(retained.msix.config_vector))
            .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Msix))? = true;
    }
    for vector in retained.msix.queue_vectors.iter().copied() {
        if vector != VIRTIO_PCI_NO_VECTOR {
            *referenced
                .get_mut(usize::from(vector))
                .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Msix))? = true;
        }
    }
    for (index, (entry, referenced)) in retained
        .msix
        .entries
        .iter()
        .copied()
        .zip(referenced.iter().copied())
        .enumerate()
    {
        let pending = retained
            .msix
            .pending
            .get(index / MSIX_BITS_PER_PBA_WORD)
            .is_some_and(|word| word & (1_u64 << (index % MSIX_BITS_PER_PBA_WORD)) != 0);
        if !entry.is_masked() && (referenced || pending) {
            messages
                .validate_message(entry.message())
                .map_err(|source| VirtioPciEndpointError::MessageRegistry { source })?;
        }
    }
    Ok(())
}

fn valid_retained_vector(vector: u16, vector_count: usize) -> bool {
    vector == VIRTIO_PCI_NO_VECTOR || usize::from(vector) < vector_count
}

fn validate_retained_configuration_relationships(
    retained: &VirtioPciTransportState,
) -> Result<(), VirtioPciEndpointError> {
    let bar = read_retained_configuration::<1>(
        &retained.configuration,
        retained_configuration_offset(retained.pci_cfg_cap_offset, 4)?,
    )?;
    let offset = read_retained_configuration::<4>(
        &retained.configuration,
        retained_configuration_offset(retained.pci_cfg_cap_offset, 8)?,
    )?;
    let length = read_retained_configuration::<4>(
        &retained.configuration,
        retained_configuration_offset(retained.pci_cfg_cap_offset, 12)?,
    )?;
    if bar[0] != retained.pci_cfg_bar
        || u32::from_le_bytes(offset) != retained.pci_cfg_offset
        || u32::from_le_bytes(length) != retained.pci_cfg_length
    {
        return Err(retained_error(VirtioPciRetainedStateError::Configuration));
    }
    let control = read_retained_configuration::<2>(
        &retained.configuration,
        retained_configuration_offset(retained.msix_cap_offset, 2)?,
    )?;
    let control = u16::from_le_bytes(control);
    if (control & MSIX_ENABLE != 0) != retained.msix.enabled
        || (control & MSIX_FUNCTION_MASK != 0) != retained.msix.function_masked
    {
        return Err(retained_error(VirtioPciRetainedStateError::Msix));
    }
    Ok(())
}

fn read_retained_configuration<const N: usize>(
    configuration: &PciType0Configuration,
    offset: u16,
) -> Result<[u8; N], VirtioPciEndpointError> {
    let mut configuration = configuration.clone();
    let mut bytes = [0_u8; N];
    configuration
        .read_config(offset, &mut bytes)
        .map_err(|_| retained_error(VirtioPciRetainedStateError::Configuration))?;
    Ok(bytes)
}

fn retained_configuration_offset(base: u16, relative: u16) -> Result<u16, VirtioPciEndpointError> {
    base.checked_add(relative)
        .ok_or_else(|| retained_error(VirtioPciRetainedStateError::Configuration))
}

const fn retained_status_is_healthy(status: u32) -> bool {
    status == VIRTIO_DEVICE_STATUS_INIT
        || status == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        || status == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER
        || status
            == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK
        || status == VIRTIO_DRIVER_READY_STATUS
}

const fn retained_error(kind: VirtioPciRetainedStateError) -> VirtioPciEndpointError {
    VirtioPciEndpointError::RetainedState { kind }
}

fn clone_transport_state<C, A>(state: &VirtioPciEndpointState<C, A>) -> VirtioPciTransportState {
    VirtioPciTransportState {
        phase: state.phase,
        configuration: state.configuration.clone(),
        pci_cfg_cap_offset: state.pci_cfg_cap_offset,
        msix_cap_offset: state.msix_cap_offset,
        pci_cfg_bar: state.pci_cfg_bar,
        pci_cfg_offset: state.pci_cfg_offset,
        pci_cfg_length: state.pci_cfg_length,
        device_feature_select: state.device_feature_select,
        driver_feature_select: state.driver_feature_select,
        queue_select: state.queue_select,
        device: state.core.device,
        queues: state.core.queues.clone(),
        queue_notifications: state.core.queue_notifications.clone(),
        device_activated: state.core.device_activated,
        requires_device_config_write_status: state.core.requires_device_config_write_status,
        interrupt_intents: state.core.interrupt_intents.clone(),
        msix: state.msix.clone(),
    }
}

impl<C: VirtioDeviceConfigHandler, A: VirtioDeviceActivationHandler>
    VirtioPciEndpointWork<'_, C, A>
{
    /// Runs one typed device operation while this admitted work transaction owns
    /// the canonical endpoint state.
    ///
    /// This boundary intentionally remains crate-private. Device modules expose
    /// semantic helpers instead of allowing transport callers to retain or mutate
    /// the common core directly. Guest-message signaling must happen after the
    /// closure returns and the endpoint mutex is released.
    pub(crate) fn with_core_mut<R>(
        &self,
        operation: impl FnOnce(&mut VirtioDeviceCore<C, A>) -> R,
    ) -> Result<R, VirtioPciEndpointError> {
        let mut state = self.endpoint.lock_active()?;
        Ok(operation(&mut state.core))
    }

    pub fn pending_queue_notifications(&self) -> Result<Vec<usize>, VirtioPciEndpointError> {
        let state = self.endpoint.lock_active()?;
        Ok(state.core.queue_notifications.pending_queue_notifications())
    }

    pub fn take_pending_queue_notifications(&self) -> Result<Vec<usize>, VirtioPciEndpointError> {
        let mut state = self.endpoint.lock_active()?;
        Ok(state
            .core
            .queue_notifications
            .take_pending_queue_notifications())
    }

    pub fn trigger(&self, intent: VirtioInterruptIntent) -> Result<(), VirtioPciEndpointError> {
        let messages = {
            let mut state = self.endpoint.lock_active()?;
            state.msix.trigger(intent)?
        };
        self.endpoint.inner.signal_messages(messages)
    }

    pub fn drain_interrupt_intents(&self) -> Result<(), VirtioPciEndpointError> {
        let (messages, first_trigger_error) = {
            let mut state = self.endpoint.lock_active()?;
            let intents = state.core.take_interrupt_intents();
            let mut messages = Vec::new();
            let mut first_error = None;
            for intent in intents {
                match state.msix.trigger(intent) {
                    Ok(triggered) => messages.extend(triggered),
                    Err(error) => {
                        first_error.get_or_insert(error);
                    }
                }
            }
            (messages, first_error)
        };
        self.endpoint.inner.signal_messages(messages)?;
        if let Some(error) = first_trigger_error {
            return Err(error);
        }
        Ok(())
    }

    pub fn increment_config_generation_and_trigger(&self) -> Result<(), VirtioPciEndpointError> {
        let messages = {
            let mut state = self.endpoint.lock_active()?;
            state.core.device.increment_config_generation();
            state.msix.trigger(VirtioInterruptIntent::Configuration)?
        };
        self.endpoint.inner.signal_messages(messages)
    }
}

/// Result of canonical device work followed by virtio-pci interrupt delivery.
#[derive(Debug)]
pub enum VirtioPciDeviceOperationError<E, T> {
    Device(Box<E>),
    Endpoint(VirtioPciEndpointError),
    CompletedAndEndpoint {
        completed: Box<T>,
        endpoint: VirtioPciEndpointError,
    },
    DeviceAndEndpoint {
        device: Box<E>,
        endpoint: VirtioPciEndpointError,
    },
}

impl<E, T> VirtioPciDeviceOperationError<E, T> {
    pub fn combine(
        device: Result<T, E>,
        endpoint: Result<(), VirtioPciEndpointError>,
    ) -> Result<T, Self> {
        match (device, endpoint) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(device), Ok(())) => Err(Self::Device(Box::new(device))),
            (Ok(completed), Err(endpoint)) => Err(Self::CompletedAndEndpoint {
                completed: Box::new(completed),
                endpoint,
            }),
            (Err(device), Err(endpoint)) => Err(Self::DeviceAndEndpoint {
                device: Box::new(device),
                endpoint,
            }),
        }
    }

    pub fn device_error(&self) -> Option<&E> {
        match self {
            Self::Device(device) | Self::DeviceAndEndpoint { device, .. } => Some(device.as_ref()),
            Self::Endpoint(_) | Self::CompletedAndEndpoint { .. } => None,
        }
    }

    pub fn completed_device_operation(&self) -> Option<&T> {
        match self {
            Self::CompletedAndEndpoint { completed, .. } => Some(completed.as_ref()),
            Self::Device(_) | Self::Endpoint(_) | Self::DeviceAndEndpoint { .. } => None,
        }
    }

    pub const fn endpoint_error(&self) -> Option<&VirtioPciEndpointError> {
        match self {
            Self::Endpoint(endpoint)
            | Self::CompletedAndEndpoint { endpoint, .. }
            | Self::DeviceAndEndpoint { endpoint, .. } => Some(endpoint),
            Self::Device(_) => None,
        }
    }
}

impl<E: fmt::Display, T> fmt::Display for VirtioPciDeviceOperationError<E, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(device) => write!(f, "virtio device operation failed: {device}"),
            Self::Endpoint(endpoint) => {
                write!(f, "virtio-pci endpoint operation failed: {endpoint}")
            }
            Self::CompletedAndEndpoint { endpoint, .. } => write!(
                f,
                "virtio device operation completed, but virtio-pci endpoint operation failed: {endpoint}"
            ),
            Self::DeviceAndEndpoint { device, endpoint } => write!(
                f,
                "virtio device operation failed: {device}; virtio-pci endpoint operation also failed: {endpoint}"
            ),
        }
    }
}

impl<E, T> std::error::Error for VirtioPciDeviceOperationError<E, T>
where
    E: std::error::Error + 'static,
    T: fmt::Debug,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Device(device) | Self::DeviceAndEndpoint { device, .. } => Some(device.as_ref()),
            Self::Endpoint(endpoint) | Self::CompletedAndEndpoint { endpoint, .. } => {
                Some(endpoint)
            }
        }
    }
}

impl<C: VirtioDeviceConfigHandler, A: VirtioDeviceActivationHandler> VirtioPciEndpointInner<C, A> {
    fn signal_messages(&self, messages: Vec<GuestMessage>) -> Result<(), VirtioPciEndpointError> {
        let attempted = !messages.is_empty();
        let mut first_error = None;
        for message in messages {
            if let Err(source) = self.messages.signal(message) {
                first_error.get_or_insert(source);
            }
        }
        if let Some(source) = first_error {
            if let Ok(state) = self.state.lock() {
                state.log_transport(LoggerTransportOutcome::InterruptDeliveryFailed);
            }
            return Err(VirtioPciEndpointError::MessageRegistry { source });
        }
        if attempted
            && self
                .interrupt_success_logging_enabled
                .load(Ordering::Acquire)
            && let Ok(state) = self.state.lock()
        {
            state.log_transport(LoggerTransportOutcome::InterruptDelivered);
        }
        Ok(())
    }
}

fn build_pci_configuration(
    identity: VirtioPciIdentity,
    capability_bar: GuestMemoryRange,
    address_space: PciBarAddressSpace,
    vector_count: usize,
) -> Result<(PciType0Configuration, u16, u16), VirtioPciEndpointError> {
    if capability_bar.size() != VIRTIO_PCI_CAPABILITY_BAR_SIZE {
        return Err(VirtioPciEndpointError::CapabilityBarSize {
            expected: VIRTIO_PCI_CAPABILITY_BAR_SIZE,
            actual: capability_bar.size(),
        });
    }
    if address_space != PciBarAddressSpace::Memory64 {
        return Err(VirtioPciEndpointError::CapabilityBarAddressSpace {
            actual: address_space,
        });
    }
    let (class_code, subclass) = pci_class(identity.device_type);
    let device_id = identity.device_type.modern_pci_device_id();
    let mut configuration = PciType0Configuration::new(
        VIRTIO_PCI_VENDOR_ID,
        device_id,
        VIRTIO_PCI_REVISION_ID,
        class_code,
        subclass,
        0,
        VIRTIO_PCI_VENDOR_ID,
        device_id,
    );
    configuration
        .install_bar_range(
            VIRTIO_PCI_CAPABILITY_BAR_INDEX,
            capability_bar,
            address_space,
            PciBarPrefetchable::No,
        )
        .map_err(|source| VirtioPciEndpointError::BarConfiguration { source })?;
    let (pci_cfg_cap_offset, msix_cap_offset) =
        add_virtio_pci_capabilities(&mut configuration, vector_count)?;
    Ok((configuration, pci_cfg_cap_offset, msix_cap_offset))
}

fn pci_class(device_type: VirtioDeviceType) -> (PciClassCode, u8) {
    match device_type.raw_value() {
        1 => (PciClassCode::Network, 0),
        2 => (PciClassCode::MassStorage, 0x80),
        _ => (
            PciClassCode::Unassigned,
            VIRTIO_PCI_NON_TRANSITIONAL_SUBCLASS,
        ),
    }
}

fn add_virtio_pci_capabilities(
    configuration: &mut PciType0Configuration,
    vector_count: usize,
) -> Result<(u16, u16), VirtioPciEndpointError> {
    add_virtio_pci_capability_profile(
        vector_count,
        |id, body, writable_mask| {
            configuration
                .add_capability(id, body, writable_mask)
                .map_err(|source| VirtioPciEndpointError::Capability { source })
        },
        || VirtioPciEndpointError::TooManyVectors { vector_count },
    )
}

fn add_virtio_pci_capability_profile<E>(
    vector_count: usize,
    mut add: impl FnMut(PciCapabilityId, &[u8], &[u8]) -> Result<u8, E>,
    mut too_many_vectors: impl FnMut() -> E,
) -> Result<(u16, u16), E> {
    let empty_mask = [0_u8; 14];
    add(
        PciCapabilityId::VendorSpecific,
        &virtio_capability_body(
            VIRTIO_PCI_GENERIC_CAP_TOTAL_SIZE,
            VIRTIO_PCI_CAP_COMMON,
            VIRTIO_PCI_COMMON_CONFIG_OFFSET,
            VIRTIO_PCI_COMMON_CONFIG_SIZE,
        ),
        &empty_mask,
    )?;
    add(
        PciCapabilityId::VendorSpecific,
        &virtio_capability_body(
            VIRTIO_PCI_GENERIC_CAP_TOTAL_SIZE,
            VIRTIO_PCI_CAP_ISR,
            VIRTIO_PCI_ISR_CONFIG_OFFSET,
            VIRTIO_PCI_ISR_CONFIG_SIZE,
        ),
        &empty_mask,
    )?;
    add(
        PciCapabilityId::VendorSpecific,
        &virtio_capability_body(
            VIRTIO_PCI_GENERIC_CAP_TOTAL_SIZE,
            VIRTIO_PCI_CAP_DEVICE,
            VIRTIO_PCI_DEVICE_CONFIG_OFFSET,
            VIRTIO_PCI_DEVICE_CONFIG_SIZE,
        ),
        &empty_mask,
    )?;

    let mut notify = virtio_capability_body(
        VIRTIO_PCI_NOTIFY_CAP_TOTAL_SIZE,
        VIRTIO_PCI_CAP_NOTIFY,
        VIRTIO_PCI_NOTIFICATION_OFFSET,
        VIRTIO_PCI_NOTIFICATION_SIZE,
    );
    notify.extend_from_slice(&VIRTIO_PCI_NOTIFICATION_MULTIPLIER.to_le_bytes());
    add(PciCapabilityId::VendorSpecific, &notify, &[0; 18])?;

    let mut pci_cfg =
        virtio_capability_body(VIRTIO_PCI_CFG_CAP_TOTAL_SIZE, VIRTIO_PCI_CAP_PCI_CFG, 0, 0);
    pci_cfg.extend_from_slice(&[0; 4]);
    let mut pci_cfg_mask = [0_u8; 18];
    if let Some(mask) = pci_cfg_mask.get_mut(2) {
        *mask = u8::MAX;
    }
    if let Some(masks) = pci_cfg_mask.get_mut(6..18) {
        masks.fill(u8::MAX);
    }
    let pci_cfg_cap_offset = add(PciCapabilityId::VendorSpecific, &pci_cfg, &pci_cfg_mask)?;

    let Some(table_size) = vector_count
        .checked_sub(1)
        .and_then(|value| u16::try_from(value).ok())
    else {
        return Err(too_many_vectors());
    };
    let mut msix_body = Vec::with_capacity(10);
    msix_body.extend_from_slice(&(MSIX_ENABLE | table_size).to_le_bytes());
    msix_body.extend_from_slice(&(VIRTIO_PCI_MSIX_TABLE_OFFSET as u32).to_le_bytes());
    msix_body.extend_from_slice(&(VIRTIO_PCI_MSIX_PBA_OFFSET as u32).to_le_bytes());
    let msix_cap_offset = add(
        PciCapabilityId::MsiX,
        &msix_body,
        &[0, 0xc0, 0, 0, 0, 0, 0, 0, 0, 0],
    )?;
    Ok((u16::from(pci_cfg_cap_offset), u16::from(msix_cap_offset)))
}

fn add_virtio_pci_snapshot_capabilities(
    profile: &mut PciType0SnapshotProfile,
    vector_count: usize,
) -> Result<(u16, u16), VirtioPciConfigurationSnapshotError> {
    add_virtio_pci_capability_profile(
        vector_count,
        |id, body, writable_mask| {
            profile
                .add_capability(id, body, writable_mask)
                .map_err(VirtioPciConfigurationSnapshotError::Capability)
        },
        || VirtioPciConfigurationSnapshotError::TooManyVectors,
    )
}

fn virtio_capability_body(total_size: u8, kind: u8, offset: u64, length: u64) -> Vec<u8> {
    let mut body = Vec::with_capacity(14);
    body.extend_from_slice(&[total_size, kind, VIRTIO_PCI_CAPABILITY_BAR_INDEX, 0, 0, 0]);
    body.extend_from_slice(&(offset as u32).to_le_bytes());
    body.extend_from_slice(&(length as u32).to_le_bytes());
    body
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioPciMsixTableEntry {
    message_address_low: u32,
    message_address_high: u32,
    message_data: u32,
    vector_control: u32,
}

impl fmt::Debug for VirtioPciMsixTableEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioPciMsixTableEntry")
            .field("state", &"<redacted>")
            .finish()
    }
}

impl Default for VirtioPciMsixTableEntry {
    fn default() -> Self {
        Self {
            message_address_low: 0,
            message_address_high: 0,
            message_data: 0,
            vector_control: 1,
        }
    }
}

impl VirtioPciMsixTableEntry {
    pub(crate) const fn from_parts(
        message_address_low: u32,
        message_address_high: u32,
        message_data: u32,
        vector_control: u32,
    ) -> Self {
        Self {
            message_address_low,
            message_address_high,
            message_data,
            vector_control,
        }
    }

    pub const fn message_address_low(self) -> u32 {
        self.message_address_low
    }

    pub const fn message_address_high(self) -> u32 {
        self.message_address_high
    }

    pub const fn message_data(self) -> u32 {
        self.message_data
    }

    pub const fn vector_control(self) -> u32 {
        self.vector_control
    }

    pub const fn is_masked(self) -> bool {
        (self.vector_control & 1) != 0
    }

    fn message(self) -> GuestMessage {
        GuestMessage::new(
            (u64::from(self.message_address_high) << 32) | u64::from(self.message_address_low),
            self.message_data,
        )
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct VirtioPciMsixState {
    entries: Vec<VirtioPciMsixTableEntry>,
    pending: Vec<u64>,
    enabled: bool,
    function_masked: bool,
    config_vector: u16,
    queue_vectors: Vec<u16>,
    pending_transition_observed: bool,
}

impl fmt::Debug for VirtioPciMsixState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioPciMsixState")
            .field("state", &"<redacted>")
            .finish()
    }
}

impl VirtioPciMsixState {
    fn new(vector_count: usize, queue_count: usize) -> Self {
        Self {
            entries: vec![VirtioPciMsixTableEntry::default(); vector_count],
            pending: vec![0; vector_count.div_ceil(MSIX_BITS_PER_PBA_WORD)],
            enabled: true,
            function_masked: false,
            config_vector: VIRTIO_PCI_NO_VECTOR,
            queue_vectors: vec![VIRTIO_PCI_NO_VECTOR; queue_count],
            pending_transition_observed: false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_parts(
        entries: Vec<VirtioPciMsixTableEntry>,
        pending: Vec<u64>,
        enabled: bool,
        function_masked: bool,
        config_vector: u16,
        queue_vectors: Vec<u16>,
        pending_transition_observed: bool,
    ) -> Self {
        Self {
            entries,
            pending,
            enabled,
            function_masked,
            config_vector,
            queue_vectors,
            pending_transition_observed,
        }
    }

    pub fn vector_count(&self) -> usize {
        self.entries.len()
    }

    pub fn entries(&self) -> &[VirtioPciMsixTableEntry] {
        &self.entries
    }

    pub fn pending_words(&self) -> &[u64] {
        &self.pending
    }

    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub const fn function_masked(&self) -> bool {
        self.function_masked
    }

    pub const fn config_vector(&self) -> u16 {
        self.config_vector
    }

    pub fn queue_vectors(&self) -> &[u16] {
        &self.queue_vectors
    }

    pub const fn pending_transition_observed(&self) -> bool {
        self.pending_transition_observed
    }

    fn set_config_vector(&mut self, vector: u16) {
        self.config_vector = self.checked_vector(vector);
    }

    fn queue_vector(&self, queue_index: u16) -> u16 {
        self.queue_vectors
            .get(usize::from(queue_index))
            .copied()
            .unwrap_or(VIRTIO_PCI_NO_VECTOR)
    }

    fn set_queue_vector(&mut self, queue_index: u16, vector: u16) {
        let checked = self.checked_vector(vector);
        if let Some(slot) = self.queue_vectors.get_mut(usize::from(queue_index)) {
            *slot = checked;
        }
    }

    fn reset_common_vectors(&mut self) {
        self.config_vector = VIRTIO_PCI_NO_VECTOR;
        self.queue_vectors.fill(VIRTIO_PCI_NO_VECTOR);
        self.pending.fill(0);
    }

    fn checked_vector(&self, vector: u16) -> u16 {
        if usize::from(vector) < self.vector_count() {
            vector
        } else {
            VIRTIO_PCI_NO_VECTOR
        }
    }

    fn set_message_control(&mut self, value: u16) -> Vec<GuestMessage> {
        let was_deliverable = self.enabled && !self.function_masked;
        self.enabled = (value & MSIX_ENABLE) != 0;
        self.function_masked = (value & MSIX_FUNCTION_MASK) != 0;
        if !was_deliverable && self.enabled && !self.function_masked {
            self.take_deliverable_pending()
        } else {
            Vec::new()
        }
    }

    fn trigger(
        &mut self,
        intent: VirtioInterruptIntent,
    ) -> Result<Vec<GuestMessage>, VirtioPciEndpointError> {
        let vector = match intent {
            VirtioInterruptIntent::Configuration => self.config_vector,
            VirtioInterruptIntent::Queue { queue_index } => self
                .queue_vectors
                .get(usize::from(queue_index))
                .copied()
                .ok_or(VirtioPciEndpointError::InvalidQueueIndex {
                    queue_index,
                    queue_count: self.queue_vectors.len(),
                })?,
        };
        if vector == VIRTIO_PCI_NO_VECTOR {
            return Ok(Vec::new());
        }
        let index = usize::from(vector);
        let entry =
            self.entries
                .get(index)
                .copied()
                .ok_or(VirtioPciEndpointError::InvalidVectorIndex {
                    vector,
                    vector_count: self.vector_count(),
                })?;
        if !self.enabled || self.function_masked || entry.is_masked() {
            self.pending_transition_observed = true;
            self.set_pending(index, true);
            Ok(Vec::new())
        } else {
            Ok(vec![entry.message()])
        }
    }

    fn read_table(&self, offset: u64, data: &mut [u8]) {
        let Some(index) = usize::try_from(offset / MSIX_TABLE_ENTRY_SIZE).ok() else {
            data.fill(u8::MAX);
            return;
        };
        let within = offset % MSIX_TABLE_ENTRY_SIZE;
        let Some(entry) = self.entries.get(index).copied() else {
            data.fill(u8::MAX);
            return;
        };
        match (within, data.len()) {
            (0, 4) => data.copy_from_slice(&entry.message_address_low.to_le_bytes()),
            (4, 4) => data.copy_from_slice(&entry.message_address_high.to_le_bytes()),
            (8, 4) => data.copy_from_slice(&entry.message_data.to_le_bytes()),
            (12, 4) => data.copy_from_slice(&entry.vector_control.to_le_bytes()),
            (0, 8) => data.copy_from_slice(
                &((u64::from(entry.message_address_high) << 32)
                    | u64::from(entry.message_address_low))
                .to_le_bytes(),
            ),
            (8, 8) => data.copy_from_slice(
                &((u64::from(entry.vector_control) << 32) | u64::from(entry.message_data))
                    .to_le_bytes(),
            ),
            _ => data.fill(u8::MAX),
        }
    }

    fn write_table(
        &mut self,
        offset: u64,
        data: &[u8],
    ) -> Result<Vec<GuestMessage>, VirtioPciEndpointError> {
        let Ok(index) = usize::try_from(offset / MSIX_TABLE_ENTRY_SIZE) else {
            return Ok(Vec::new());
        };
        let within = offset % MSIX_TABLE_ENTRY_SIZE;
        let vector_count = self.vector_count();
        let Some(entry) = self.entries.get_mut(index) else {
            return Ok(Vec::new());
        };
        let was_masked = entry.is_masked();
        match (within, data.len()) {
            (0, 4) => entry.message_address_low = read_u32(data),
            (4, 4) => entry.message_address_high = read_u32(data),
            (8, 4) => entry.message_data = read_u32(data),
            (12, 4) => entry.vector_control = read_u32(data),
            (0, 8) => {
                let value = read_u64(data);
                entry.message_address_low = value as u32;
                entry.message_address_high = (value >> 32) as u32;
            }
            (8, 8) => {
                let value = read_u64(data);
                entry.message_data = value as u32;
                entry.vector_control = (value >> 32) as u32;
            }
            _ => {
                return Ok(Vec::new());
            }
        }
        let now_unmasked = was_masked && !entry.is_masked();
        let message = entry.message();
        if self.enabled
            && !self.function_masked
            && now_unmasked
            && pending_bit(&self.pending, index, vector_count)?
        {
            self.set_pending(index, false);
            Ok(vec![message])
        } else {
            Ok(Vec::new())
        }
    }

    fn read_pba(&self, offset: u64, data: &mut [u8]) {
        let Some(index) = usize::try_from(offset / MSIX_PBA_WORD_SIZE).ok() else {
            data.fill(u8::MAX);
            return;
        };
        let within = offset % MSIX_PBA_WORD_SIZE;
        let Some(word) = self.pending.get(index).copied() else {
            data.fill(u8::MAX);
            return;
        };
        match (within, data.len()) {
            (0, 4) => data.copy_from_slice(&(word as u32).to_le_bytes()),
            (4, 4) => data.copy_from_slice(&((word >> 32) as u32).to_le_bytes()),
            (0, 8) => data.copy_from_slice(&word.to_le_bytes()),
            _ => data.fill(u8::MAX),
        }
    }

    fn set_pending(&mut self, index: usize, pending: bool) {
        let word = index / MSIX_BITS_PER_PBA_WORD;
        let bit = index % MSIX_BITS_PER_PBA_WORD;
        if let Some(value) = self.pending.get_mut(word) {
            if pending {
                *value |= 1_u64 << bit;
            } else {
                *value &= !(1_u64 << bit);
            }
        }
    }

    fn take_deliverable_pending(&mut self) -> Vec<GuestMessage> {
        let mut messages = Vec::new();
        for index in 0..self.entries.len() {
            let pending = pending_bit(&self.pending, index, self.entries.len()).unwrap_or(false);
            let Some(entry) = self.entries.get(index).copied() else {
                continue;
            };
            if pending && !entry.is_masked() {
                self.set_pending(index, false);
                messages.push(entry.message());
            }
        }
        messages
    }
}

fn pending_bit(
    pending: &[u64],
    index: usize,
    vector_count: usize,
) -> Result<bool, VirtioPciEndpointError> {
    if index >= vector_count {
        return Err(VirtioPciEndpointError::InvalidVectorIndex {
            vector: u16::try_from(index).unwrap_or(u16::MAX),
            vector_count,
        });
    }
    let word = index / MSIX_BITS_PER_PBA_WORD;
    let bit = index % MSIX_BITS_PER_PBA_WORD;
    Ok(pending
        .get(word)
        .is_some_and(|value| (value & (1_u64 << bit)) != 0))
}

fn read_u32(data: &[u8]) -> u32 {
    match data.try_into() {
        Ok(bytes) => u32::from_le_bytes(bytes),
        Err(_) => 0,
    }
}

fn read_u64(data: &[u8]) -> u64 {
    match data.try_into() {
        Ok(bytes) => u64::from_le_bytes(bytes),
        Err(_) => 0,
    }
}

fn read_u16(data: &[u8]) -> u16 {
    match data.try_into() {
        Ok(bytes) => u16::from_le_bytes(bytes),
        Err(_) => 0,
    }
}

impl<C: VirtioDeviceConfigHandler, A: VirtioDeviceActivationHandler> VirtioPciEndpointState<C, A> {
    fn logging_context(&self) -> Option<(GuestLogger, LoggerDeviceKind)> {
        LoggerDeviceKind::from_virtio_device_id(self.core.device.device_id())
            .map(|kind| (self.guest_logger.clone(), kind))
    }

    fn log_transport(&self, outcome: impl FnOnce(LoggerDeviceKind) -> LoggerTransportOutcome) {
        if let Some(kind) = LoggerDeviceKind::from_virtio_device_id(self.core.device.device_id()) {
            self.guest_logger.log_transport(outcome(kind));
        }
    }

    fn log_endpoint_error(&self, error: &VirtioPciEndpointError) {
        self.log_transport(|kind| transport_outcome_for_endpoint_error(kind, error));
    }

    fn ensure_active(&self) -> Result<(), VirtioPciEndpointError> {
        if self.phase == VirtioPciEndpointPhase::Active {
            Ok(())
        } else {
            Err(VirtioPciEndpointError::NotActive { phase: self.phase })
        }
    }

    fn read_pci_config(
        &mut self,
        offset: u16,
        data: &mut [u8],
    ) -> Result<(), VirtioPciEndpointError> {
        let pci_data = self.pci_cfg_cap_offset + 16;
        if access_within_u16(offset, data.len(), pci_data, 4) {
            data.fill(0);
            if self.pci_cfg_bar != VIRTIO_PCI_CAPABILITY_BAR_INDEX
                || !matches!(self.pci_cfg_length, 1 | 2 | 4)
            {
                return Ok(());
            }
            let length = usize::try_from(self.pci_cfg_length)
                .map_err(|_| VirtioPciEndpointError::PciConfigurationAccess)?;
            let mut value = [0_u8; 4];
            let selected = value
                .get_mut(..length)
                .ok_or(VirtioPciEndpointError::PciConfigurationAccess)?;
            self.read_bar_bytes(u64::from(self.pci_cfg_offset), selected)?;
            let relative = usize::from(offset - pci_data);
            let available = length.saturating_sub(relative).min(data.len());
            if available != 0 {
                let end = relative
                    .checked_add(available)
                    .ok_or(VirtioPciEndpointError::PciConfigurationAccess)?;
                let destination = data
                    .get_mut(..available)
                    .ok_or(VirtioPciEndpointError::PciConfigurationAccess)?;
                let source = value
                    .get(relative..end)
                    .ok_or(VirtioPciEndpointError::PciConfigurationAccess)?;
                destination.copy_from_slice(source);
            }
            return Ok(());
        }
        self.configuration
            .read_config(offset, data)
            .map_err(|_| VirtioPciEndpointError::PciConfigurationAccess)
    }

    fn write_pci_config(
        &mut self,
        offset: u16,
        data: &[u8],
    ) -> Result<Vec<GuestMessage>, VirtioPciEndpointError> {
        self.configuration
            .write_config(offset, data)
            .map_err(|_| VirtioPciEndpointError::PciConfigurationAccess)?;
        self.refresh_pci_cfg_selector()?;

        let pci_data = self.pci_cfg_cap_offset + 16;
        if access_within_u16(offset, data.len(), pci_data, 4)
            && self.pci_cfg_bar == VIRTIO_PCI_CAPABILITY_BAR_INDEX
            && matches!(self.pci_cfg_length, 1 | 2 | 4)
        {
            let relative = usize::from(offset - pci_data);
            let configured_len = usize::try_from(self.pci_cfg_length)
                .map_err(|_| VirtioPciEndpointError::PciConfigurationAccess)?;
            let available = configured_len.saturating_sub(relative).min(data.len());
            if available != 0 {
                let target = u64::from(self.pci_cfg_offset)
                    .checked_add(
                        u64::try_from(relative)
                            .map_err(|_| VirtioPciEndpointError::PciConfigurationAccess)?,
                    )
                    .ok_or(VirtioPciEndpointError::PciConfigurationAccess)?;
                let selected = data
                    .get(..available)
                    .ok_or(VirtioPciEndpointError::PciConfigurationAccess)?;
                return self.write_bar_bytes(target, selected);
            }
        }

        let msix_control = self.msix_cap_offset + 2;
        if ranges_overlap_u16(offset, data.len(), msix_control, 2) {
            let mut control = [0_u8; 2];
            self.configuration
                .read_config(msix_control, &mut control)
                .map_err(|_| VirtioPciEndpointError::PciConfigurationAccess)?;
            let messages = self.msix.set_message_control(u16::from_le_bytes(control));
            self.log_transport(LoggerTransportOutcome::MsiConfigurationSucceeded);
            return Ok(messages);
        }
        Ok(Vec::new())
    }

    fn refresh_pci_cfg_selector(&mut self) -> Result<(), VirtioPciEndpointError> {
        let mut bar = [0_u8; 1];
        self.configuration
            .read_config(self.pci_cfg_cap_offset + 4, &mut bar)
            .map_err(|_| VirtioPciEndpointError::PciConfigurationAccess)?;
        let mut offset = [0_u8; 4];
        self.configuration
            .read_config(self.pci_cfg_cap_offset + 8, &mut offset)
            .map_err(|_| VirtioPciEndpointError::PciConfigurationAccess)?;
        let mut length = [0_u8; 4];
        self.configuration
            .read_config(self.pci_cfg_cap_offset + 12, &mut length)
            .map_err(|_| VirtioPciEndpointError::PciConfigurationAccess)?;
        self.pci_cfg_bar = bar.first().copied().unwrap_or_default();
        self.pci_cfg_offset = u32::from_le_bytes(offset);
        self.pci_cfg_length = u32::from_le_bytes(length);
        Ok(())
    }

    fn read_bar_bytes(
        &mut self,
        offset: u64,
        data: &mut [u8],
    ) -> Result<(), VirtioPciEndpointError> {
        self.ensure_active()?;
        data.fill(0);
        if !access_within(offset, data.len(), 0, VIRTIO_PCI_CAPABILITY_BAR_SIZE) {
            return Err(VirtioPciEndpointError::OutsideCapabilityBar {
                offset,
                len: data.len(),
            });
        }
        if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_COMMON_CONFIG_OFFSET,
            VIRTIO_PCI_COMMON_CONFIG_SIZE,
        ) {
            self.read_common(offset - VIRTIO_PCI_COMMON_CONFIG_OFFSET, data);
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_ISR_CONFIG_OFFSET,
            VIRTIO_PCI_ISR_CONFIG_SIZE,
        ) {
            data.fill(0);
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_DEVICE_CONFIG_OFFSET,
            VIRTIO_PCI_DEVICE_CONFIG_SIZE,
        ) {
            let access = VirtioDeviceConfigAccess::from_transport_parts(
                MmioOperationKind::Read,
                offset - VIRTIO_PCI_DEVICE_CONFIG_OFFSET,
                data.len(),
            );
            let value = self
                .core
                .device_config
                .read_device_config(access)
                .map_err(|source| VirtioPciEndpointError::DeviceConfig { source })?;
            if value.len() != data.len() {
                return Err(VirtioPciEndpointError::DeviceConfigReadLength {
                    expected: data.len(),
                    actual: value.len(),
                });
            }
            data.copy_from_slice(value.as_slice());
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_NOTIFICATION_OFFSET,
            VIRTIO_PCI_NOTIFICATION_SIZE,
        ) {
            data.fill(0);
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_MSIX_TABLE_OFFSET,
            VIRTIO_PCI_MSIX_TABLE_SIZE,
        ) {
            self.msix
                .read_table(offset - VIRTIO_PCI_MSIX_TABLE_OFFSET, data);
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_MSIX_PBA_OFFSET,
            VIRTIO_PCI_MSIX_PBA_SIZE,
        ) {
            self.msix
                .read_pba(offset - VIRTIO_PCI_MSIX_PBA_OFFSET, data);
        }
        Ok(())
    }

    fn write_bar_bytes(
        &mut self,
        offset: u64,
        data: &[u8],
    ) -> Result<Vec<GuestMessage>, VirtioPciEndpointError> {
        self.ensure_active()?;
        if !access_within(offset, data.len(), 0, VIRTIO_PCI_CAPABILITY_BAR_SIZE) {
            return Err(VirtioPciEndpointError::OutsideCapabilityBar {
                offset,
                len: data.len(),
            });
        }
        if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_COMMON_CONFIG_OFFSET,
            VIRTIO_PCI_COMMON_CONFIG_SIZE,
        ) {
            self.write_common(offset - VIRTIO_PCI_COMMON_CONFIG_OFFSET, data)
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_ISR_CONFIG_OFFSET,
            VIRTIO_PCI_ISR_CONFIG_SIZE,
        ) {
            Ok(Vec::new())
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_DEVICE_CONFIG_OFFSET,
            VIRTIO_PCI_DEVICE_CONFIG_SIZE,
        ) {
            if self.core.requires_device_config_write_status
                && (self.core.device.status() & VIRTIO_DEVICE_STATUS_DRIVER) == 0
            {
                return Err(VirtioPciEndpointError::DeviceConfigNotWritable {
                    status: self.core.device.status(),
                });
            }
            let access = VirtioDeviceConfigAccess::from_transport_parts(
                MmioOperationKind::Write,
                offset - VIRTIO_PCI_DEVICE_CONFIG_OFFSET,
                data.len(),
            );
            let value = MmioAccessBytes::new(data)
                .map_err(|_| VirtioPciEndpointError::InvalidBarAccessWidth { len: data.len() })?;
            self.core
                .device_config
                .write_device_config(access, value)
                .map_err(|source| VirtioPciEndpointError::DeviceConfig { source })?;
            Ok(Vec::new())
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_NOTIFICATION_OFFSET,
            VIRTIO_PCI_NOTIFICATION_SIZE,
        ) {
            self.write_notification(offset - VIRTIO_PCI_NOTIFICATION_OFFSET, data)?;
            if self.queue_notification_success_logging_enabled {
                self.log_transport(LoggerTransportOutcome::QueueNotificationSucceeded);
            }
            Ok(Vec::new())
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_MSIX_TABLE_OFFSET,
            VIRTIO_PCI_MSIX_TABLE_SIZE,
        ) {
            let messages = self
                .msix
                .write_table(offset - VIRTIO_PCI_MSIX_TABLE_OFFSET, data)?;
            self.log_transport(LoggerTransportOutcome::MsiConfigurationSucceeded);
            Ok(messages)
        } else if access_within(
            offset,
            data.len(),
            VIRTIO_PCI_MSIX_PBA_OFFSET,
            VIRTIO_PCI_MSIX_PBA_SIZE,
        ) {
            // The pending-bit array is read-only.
            Ok(Vec::new())
        } else {
            Ok(Vec::new())
        }
    }

    fn read_common(&self, offset: u64, data: &mut [u8]) {
        match (offset, data.len()) {
            (VIRTIO_PCI_COMMON_DEVICE_STATUS, 1) => {
                if let Some(value) = data.first_mut() {
                    *value = self.core.device.status() as u8;
                }
            }
            (VIRTIO_PCI_COMMON_CONFIG_GENERATION, 1) => {
                if let Some(value) = data.first_mut() {
                    *value = self.core.device.config_generation() as u8;
                }
            }
            (VIRTIO_PCI_COMMON_MSIX_CONFIG, 2) => {
                data.copy_from_slice(&self.msix.config_vector().to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_NUM_QUEUES, 2) => {
                let count = u16::try_from(self.core.queues.queue_count()).unwrap_or(u16::MAX);
                data.copy_from_slice(&count.to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_QUEUE_SELECT, 2) => {
                data.copy_from_slice(&self.queue_select.to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_QUEUE_SIZE, 2) => {
                let value = self.selected_queue().map_or(0, |queue| {
                    if queue.size() == 0 {
                        queue.max_size()
                    } else {
                        queue.size()
                    }
                });
                data.copy_from_slice(&value.to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_QUEUE_MSIX_VECTOR, 2) => {
                data.copy_from_slice(&self.msix.queue_vector(self.queue_select).to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_QUEUE_ENABLE, 2) => {
                let value = u16::from(self.selected_queue().is_some_and(|queue| queue.ready()));
                data.copy_from_slice(&value.to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_QUEUE_NOTIFY_OFF, 2) => {
                data.copy_from_slice(&self.queue_select.to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT, 4) => {
                data.copy_from_slice(&self.device_feature_select.to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_DEVICE_FEATURE, 4) => {
                let value = feature_page(
                    self.core.device.device_features(),
                    self.device_feature_select,
                );
                data.copy_from_slice(&value.to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT, 4) => {
                data.copy_from_slice(&self.driver_feature_select.to_le_bytes());
            }
            (VIRTIO_PCI_COMMON_QUEUE_DESC_LO, 4) => {
                self.read_queue_address(data, |queue| queue.descriptor_table(), AddressHalf::Low)
            }
            (VIRTIO_PCI_COMMON_QUEUE_DESC_HI, 4) => {
                self.read_queue_address(data, |queue| queue.descriptor_table(), AddressHalf::High)
            }
            (VIRTIO_PCI_COMMON_QUEUE_AVAIL_LO, 4) => {
                self.read_queue_address(data, |queue| queue.driver_ring(), AddressHalf::Low);
            }
            (VIRTIO_PCI_COMMON_QUEUE_AVAIL_HI, 4) => {
                self.read_queue_address(data, |queue| queue.driver_ring(), AddressHalf::High);
            }
            (VIRTIO_PCI_COMMON_QUEUE_USED_LO, 4) => {
                self.read_queue_address(data, |queue| queue.device_ring(), AddressHalf::Low);
            }
            (VIRTIO_PCI_COMMON_QUEUE_USED_HI, 4) => {
                self.read_queue_address(data, |queue| queue.device_ring(), AddressHalf::High);
            }
            _ => data.fill(0),
        }
    }

    fn write_common(
        &mut self,
        offset: u64,
        data: &[u8],
    ) -> Result<Vec<GuestMessage>, VirtioPciEndpointError> {
        match (offset, data.len()) {
            (VIRTIO_PCI_COMMON_DEVICE_STATUS, 1) => {
                self.write_device_status(data.first().copied().unwrap_or_default())
            }
            (VIRTIO_PCI_COMMON_MSIX_CONFIG, 2) => {
                self.msix.set_config_vector(read_u16(data));
                self.log_transport(LoggerTransportOutcome::MsiConfigurationSucceeded);
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_SELECT, 2) => {
                self.queue_select = read_u16(data);
                if usize::from(self.queue_select) < self.core.queues.queue_count() {
                    self.core
                        .queues
                        .write_register(
                            VirtioMmioRegister::QueueSel,
                            u32::from(self.queue_select),
                            self.core.device.status(),
                        )
                        .map_err(|source| VirtioPciEndpointError::Queue { source })?;
                }
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_SIZE, 2) => {
                self.write_queue_register(VirtioMmioRegister::QueueNum, u32::from(read_u16(data)))?;
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_MSIX_VECTOR, 2) => {
                self.msix
                    .set_queue_vector(self.queue_select, read_u16(data));
                self.log_transport(LoggerTransportOutcome::MsiConfigurationSucceeded);
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_ENABLE, 2) => {
                self.write_queue_register(
                    VirtioMmioRegister::QueueReady,
                    u32::from(read_u16(data)),
                )?;
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT, 4) => {
                self.device_feature_select = read_u32(data);
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT, 4) => {
                self.driver_feature_select = read_u32(data);
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_DRIVER_FEATURE, 4) => {
                if self.driver_feature_select < 2
                    && self.core.device.status()
                        == VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER
                {
                    let requested = read_u32(data)
                        & feature_page(
                            self.core.device.device_features(),
                            self.driver_feature_select,
                        );
                    self.core
                        .device
                        .write_register(
                            VirtioMmioRegister::DriverFeaturesSel,
                            self.driver_feature_select,
                        )
                        .map_err(|source| VirtioPciEndpointError::DeviceRegisters { source })?;
                    self.core
                        .device
                        .write_register(VirtioMmioRegister::DriverFeatures, requested)
                        .map_err(|source| VirtioPciEndpointError::DeviceRegisters { source })?;
                }
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_DESC_LO, 4) => {
                self.write_queue_register(VirtioMmioRegister::QueueDescLow, read_u32(data))?;
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_DESC_HI, 4) => {
                self.write_queue_register(VirtioMmioRegister::QueueDescHigh, read_u32(data))?;
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_AVAIL_LO, 4) => {
                self.write_queue_register(VirtioMmioRegister::QueueDriverLow, read_u32(data))?;
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_AVAIL_HI, 4) => {
                self.write_queue_register(VirtioMmioRegister::QueueDriverHigh, read_u32(data))?;
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_USED_LO, 4) => {
                self.write_queue_register(VirtioMmioRegister::QueueDeviceLow, read_u32(data))?;
                Ok(Vec::new())
            }
            (VIRTIO_PCI_COMMON_QUEUE_USED_HI, 4) => {
                self.write_queue_register(VirtioMmioRegister::QueueDeviceHigh, read_u32(data))?;
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn write_device_status(
        &mut self,
        value: u8,
    ) -> Result<Vec<GuestMessage>, VirtioPciEndpointError> {
        if value != VIRTIO_DEVICE_STATUS_INIT as u8
            && (u32::from(value) & VIRTIO_DEVICE_STATUS_FAILED) == 0
            && self.core.device_activated
            && self.core.device.status() == VIRTIO_DEVICE_STATUS_INIT
        {
            // Firecracker keeps an unresettable backend active while exposing
            // status zero for the Linux reset poll. Reinitialization against
            // that still-live backend is rejected as a guest-visible no-op.
            return Ok(Vec::new());
        }
        match self.core.device.set_status(u32::from(value)) {
            Ok(()) => {}
            Err(VirtioMmioRegisterStateError::InvalidStatusTransition { .. }) => {
                // Firecracker leaves the previous status visible for an invalid
                // modern virtio-pci transition; the guest write is otherwise a
                // no-op rather than a fatal MMIO access.
                return Ok(Vec::new());
            }
            Err(source) => {
                return Err(VirtioPciEndpointError::DeviceRegisters { source });
            }
        }
        if value == VIRTIO_DEVICE_STATUS_INIT as u8 {
            let outcome = self
                .core
                .reset_common_state_with_outcome()
                .map_err(|source| VirtioPciEndpointError::DeviceReset { source })?;
            match outcome {
                VirtioDeviceResetOutcome::Reset => {
                    self.device_feature_select = 0;
                    self.driver_feature_select = 0;
                    self.queue_select = 0;
                    self.msix.reset_common_vectors();
                    self.log_transport(LoggerTransportOutcome::DeviceResetSucceeded);
                }
                VirtioDeviceResetOutcome::Unsupported => {
                    self.log_transport(LoggerTransportOutcome::DeviceResetUnsupported);
                }
            }
            return Ok(Vec::new());
        }
        if self.core.device.status() == VIRTIO_DRIVER_READY_STATUS && !self.core.device_activated {
            let core = &mut self.core;
            let activation = VirtioDeviceActivation::new(&core.device, &core.queues);
            match core.activation.activate(activation) {
                Ok(()) => {
                    core.device_activated = true;
                    self.log_transport(LoggerTransportOutcome::DeviceActivationSucceeded);
                }
                Err(_source) => {
                    core.device.mark_device_needs_reset();
                    self.log_transport(LoggerTransportOutcome::DeviceActivationFailed);
                    return self.msix.trigger(VirtioInterruptIntent::Configuration);
                }
            }
        }
        Ok(Vec::new())
    }

    fn selected_queue(&self) -> Option<&crate::virtio::VirtioQueueState> {
        self.core.queues.queue(u32::from(self.queue_select)).ok()
    }

    fn read_queue_address(
        &self,
        data: &mut [u8],
        address: impl FnOnce(&crate::virtio::VirtioQueueState) -> GuestAddress,
        half: AddressHalf,
    ) {
        let raw = self
            .selected_queue()
            .map(address)
            .map(GuestAddress::raw_value)
            .unwrap_or(0);
        let value = match half {
            AddressHalf::Low => raw as u32,
            AddressHalf::High => (raw >> 32) as u32,
        };
        data.copy_from_slice(&value.to_le_bytes());
    }

    fn write_queue_register(
        &mut self,
        register: VirtioMmioRegister,
        mut value: u32,
    ) -> Result<(), VirtioPciEndpointError> {
        if usize::from(self.queue_select) >= self.core.queues.queue_count() {
            return Ok(());
        }
        let status = self.core.device.status();
        if status
            != VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK
        {
            return Ok(());
        }
        if register == VirtioMmioRegister::QueueReady {
            if value == 0 {
                return Ok(());
            }
            value = u32::from(value == 1);
        }
        match self.core.queues.write_register(register, value, status) {
            Ok(()) => Ok(()),
            Err(
                VirtioMmioQueueRegisterError::InvalidQueueSize { .. }
                | VirtioMmioQueueRegisterError::InvalidQueueReadyValue { .. }
                | VirtioMmioQueueRegisterError::UnalignedQueueAddress { .. },
            ) => Ok(()),
            Err(source) => Err(VirtioPciEndpointError::Queue { source }),
        }
    }

    fn write_notification(
        &mut self,
        offset: u64,
        data: &[u8],
    ) -> Result<(), VirtioPciEndpointError> {
        if !matches!(data.len(), 2 | 4)
            || !offset.is_multiple_of(u64::from(VIRTIO_PCI_NOTIFICATION_MULTIPLIER))
        {
            return Ok(());
        }
        let Ok(queue_index) = u32::try_from(offset / u64::from(VIRTIO_PCI_NOTIFICATION_MULTIPLIER))
        else {
            return Ok(());
        };
        match self.core.queue_notifications.write_register(
            VirtioMmioRegister::QueueNotify,
            queue_index,
            self.core.device.status(),
        ) {
            Ok(())
            | Err(
                VirtioQueueNotificationError::InvalidQueueIndex { .. }
                | VirtioQueueNotificationError::QueueNotifyNotWritable { .. },
            ) => Ok(()),
            Err(source) => Err(VirtioPciEndpointError::QueueNotification { source }),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum AddressHalf {
    Low,
    High,
}

fn feature_page(features: u64, selector: u32) -> u32 {
    match selector {
        0 => features as u32,
        1 => (features >> 32) as u32,
        _ => 0,
    }
}

fn access_within(offset: u64, len: usize, start: u64, size: u64) -> bool {
    let Ok(len) = u64::try_from(len) else {
        return false;
    };
    let Some(end) = offset.checked_add(len) else {
        return false;
    };
    let Some(region_end) = start.checked_add(size) else {
        return false;
    };
    start <= offset && end <= region_end
}

fn access_within_u16(offset: u16, len: usize, start: u16, size: usize) -> bool {
    let offset = usize::from(offset);
    let start = usize::from(start);
    offset >= start
        && offset
            .checked_add(len)
            .is_some_and(|end| end <= start + size)
}

fn ranges_overlap_u16(offset: u16, len: usize, start: u16, size: usize) -> bool {
    let offset = usize::from(offset);
    let start = usize::from(start);
    offset < start + size && start < offset.saturating_add(len)
}

impl<C, A> PciConfigFunction for VirtioPciConfigFunction<C, A>
where
    C: VirtioDeviceConfigHandler + 'static,
    A: VirtioDeviceActivationHandler + 'static,
{
    fn read_config(&mut self, offset: u16, data: &mut [u8]) -> Result<(), PciConfigAccessError> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| PciConfigAccessError::Handler {
                message: VirtioPciEndpointError::StatePoisoned.to_string(),
            })?;
        let result = state
            .ensure_active()
            .and_then(|()| state.read_pci_config(offset, data));
        if let Err(error) = &result {
            state.log_endpoint_error(error);
        }
        result.map_err(pci_handler_error)
    }

    fn write_config(&mut self, offset: u16, data: &[u8]) -> Result<(), PciConfigAccessError> {
        let messages = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| PciConfigAccessError::Handler {
                    message: VirtioPciEndpointError::StatePoisoned.to_string(),
                })?;
            let result = state
                .ensure_active()
                .and_then(|()| state.write_pci_config(offset, data));
            if let Err(error) = &result {
                state.log_endpoint_error(error);
            }
            result.map_err(pci_handler_error)?
        };
        self.inner
            .signal_messages(messages)
            .map_err(pci_handler_error)
    }
}

impl<C, A> VirtioPciBarHandler<C, A>
where
    C: VirtioDeviceConfigHandler + 'static,
    A: VirtioDeviceActivationHandler + 'static,
{
    fn logging_context(&self) -> Option<(GuestLogger, LoggerDeviceKind)> {
        self.device_kind
            .map(|kind| (self.guest_logger.clone(), kind))
    }

    fn log_endpoint_error(&self, error: &VirtioPciEndpointError) {
        if let Some((logger, kind)) = self.logging_context() {
            logger.log_transport(transport_outcome_for_endpoint_error(kind, error));
        }
    }

    fn read_typed(&self, access: MmioAccess) -> Result<MmioAccessBytes, VirtioPciEndpointError> {
        let gate = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
            state.ensure_active()?;
            state.core.work_gate().clone()
        };
        let _work = gate
            .admit()
            .map_err(|source| VirtioPciEndpointError::WorkGate { source })?;
        let len = usize::try_from(access.range().size())
            .map_err(|_| VirtioPciEndpointError::InvalidBarAccessWidth { len: usize::MAX })?;
        if !matches!(len, 1 | 2 | 4 | 8) {
            return Err(VirtioPciEndpointError::InvalidBarAccessWidth { len });
        }
        let mut bytes = [0_u8; 8];
        let destination = bytes
            .get_mut(..len)
            .ok_or(VirtioPciEndpointError::InvalidBarAccessWidth { len })?;
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
            state.read_bar_bytes(access.offset(), destination)?;
        }
        MmioAccessBytes::new(destination)
            .map_err(|_| VirtioPciEndpointError::InvalidBarAccessWidth { len })
    }

    fn write_typed(
        &self,
        access: MmioAccess,
        data: MmioAccessBytes,
    ) -> Result<(), VirtioPciEndpointError> {
        let gate = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
            state.ensure_active()?;
            state.core.work_gate().clone()
        };
        let _work = gate
            .admit()
            .map_err(|source| VirtioPciEndpointError::WorkGate { source })?;
        let messages = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| VirtioPciEndpointError::StatePoisoned)?;
            state.write_bar_bytes(access.offset(), data.as_slice())?
        };
        self.inner.signal_messages(messages)
    }
}

impl<C, A> MmioHandler for VirtioPciBarHandler<C, A>
where
    C: VirtioDeviceConfigHandler + 'static,
    A: VirtioDeviceActivationHandler + 'static,
{
    fn read(&mut self, access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
        let result = self.read_typed(access);
        if let Err(error) = &result {
            self.log_endpoint_error(error);
        }
        result.map_err(|source| MmioHandlerError::new(source.to_string()))
    }

    fn write(&mut self, access: MmioAccess, data: MmioAccessBytes) -> Result<(), MmioHandlerError> {
        let result = self.write_typed(access, data);
        if let Err(error) = &result
            && !matches!(error, VirtioPciEndpointError::MessageRegistry { .. })
        {
            self.log_endpoint_error(error);
        }
        result.map_err(|source| MmioHandlerError::new(source.to_string()))
    }

    fn attach_guest_logger(&mut self, logger: GuestLogger) {
        if let Ok(mut state) = self.inner.state.lock() {
            let kind = LoggerDeviceKind::from_virtio_device_id(state.core.device.device_id());
            state.queue_notification_success_logging_enabled = kind.is_some_and(|kind| {
                logger.transport_outcome_enabled(
                    LoggerTransportOutcome::QueueNotificationSucceeded(kind),
                )
            });
            self.inner.interrupt_success_logging_enabled.store(
                kind.is_some_and(|kind| {
                    logger
                        .transport_outcome_enabled(LoggerTransportOutcome::InterruptDelivered(kind))
                }),
                Ordering::Release,
            );
            state.core.activation.attach_guest_logger(logger.clone());
            state.guest_logger = logger.clone();
        }
        self.guest_logger = logger;
    }

    fn owns_transport_access_logging(&self) -> bool {
        self.device_kind.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RetainedPublicationStage {
    Preflight,
    Bar,
    Function,
    Mmio,
}

impl<C, A> PreparedVirtioPciEndpoint<C, A>
where
    C: VirtioDeviceConfigHandler + 'static,
    A: VirtioDeviceActivationHandler + 'static,
{
    pub fn publish<I>(
        self,
        bar_allocator: &mut PciBarAllocator,
        segment: SharedPciSegment,
        dispatcher: &mut MmioDispatcher,
        interrupts: I,
    ) -> Result<PublishedVirtioPciEndpoint<C, A, I>, VirtioPciPublicationError>
    where
        I: GuestMessageInterruptResources,
    {
        self.endpoint.attach_guest_logger(dispatcher.guest_logger());
        let logging = self.endpoint.logging_context();
        let result =
            self.publish_with_hook(bar_allocator, segment, dispatcher, interrupts, |_| Ok(()));
        if let Some((logger, kind)) = logging {
            logger.log_transport(if result.is_ok() {
                LoggerTransportOutcome::PciFunctionPublished(kind)
            } else {
                LoggerTransportOutcome::PciFunctionPublicationFailed(kind)
            });
        }
        result
    }

    fn publish_with_hook<I>(
        self,
        bar_allocator: &mut PciBarAllocator,
        segment: SharedPciSegment,
        dispatcher: &mut MmioDispatcher,
        mut interrupts: I,
        mut hook: impl FnMut(RetainedPublicationStage) -> Result<(), VirtioPciPublicationError>,
    ) -> Result<PublishedVirtioPciEndpoint<C, A, I>, VirtioPciPublicationError>
    where
        I: GuestMessageInterruptResources,
    {
        let resource_registry = interrupts.registry();
        if !self
            .endpoint
            .inner
            .messages
            .is_same_registry(&resource_registry)
        {
            let primary = VirtioPciPublicationError::InterruptRegistryMismatch;
            let mut cleanup = String::new();
            if let Err(source) = interrupts.release() {
                append_cleanup(&mut cleanup, "interrupt resources", &source);
            }
            return Err(publication_rollback(primary, cleanup));
        }
        let retained = match self.endpoint.transport_state() {
            Ok(retained) => retained,
            Err(source) => {
                let primary = VirtioPciPublicationError::Endpoint { source };
                let cleanup = cleanup_prepared_endpoint(&self.endpoint, &mut interrupts);
                return Err(publication_rollback(primary, cleanup));
            }
        };
        if let Err(source) = validate_retained_msix(&retained, &resource_registry) {
            let primary = VirtioPciPublicationError::Endpoint { source };
            let cleanup = cleanup_prepared_endpoint(&self.endpoint, &mut interrupts);
            return Err(publication_rollback(primary, cleanup));
        }
        if bar_allocator.address_space() != PciBarAddressSpace::Memory64 {
            let primary = VirtioPciPublicationError::Endpoint {
                source: VirtioPciEndpointError::CapabilityBarAddressSpace {
                    actual: bar_allocator.address_space(),
                },
            };
            let cleanup = cleanup_prepared_endpoint(&self.endpoint, &mut interrupts);
            return Err(publication_rollback(primary, cleanup));
        }
        if let Err(source) = bar_allocator.validate_exact_reservation(self.bar_range) {
            let primary = VirtioPciPublicationError::BarAllocation { source };
            let cleanup = cleanup_prepared_endpoint(&self.endpoint, &mut interrupts);
            return Err(publication_rollback(primary, cleanup));
        }
        match segment.with_segment(|segment| segment.validate_function_at(self.sbdf)) {
            Ok(Ok(())) => {}
            Ok(Err(source)) => {
                let primary = VirtioPciPublicationError::SegmentAdd { source };
                let cleanup = cleanup_prepared_endpoint(&self.endpoint, &mut interrupts);
                return Err(publication_rollback(primary, cleanup));
            }
            Err(source) => {
                let primary = VirtioPciPublicationError::SegmentLock { source };
                let cleanup = cleanup_prepared_endpoint(&self.endpoint, &mut interrupts);
                return Err(publication_rollback(primary, cleanup));
            }
        }
        let regions = [MmioRegionRequest::new(
            self.bar_range.start(),
            self.bar_range.size(),
        )];
        if let Err(source) = dispatcher.validate_owned_handler(self.region_id, &regions) {
            let primary = VirtioPciPublicationError::MmioRegistration { source };
            let cleanup = cleanup_prepared_endpoint(&self.endpoint, &mut interrupts);
            return Err(publication_rollback(primary, cleanup));
        }
        if let Err(primary) = hook(RetainedPublicationStage::Preflight) {
            let cleanup = cleanup_prepared_endpoint(&self.endpoint, &mut interrupts);
            return Err(publication_rollback(primary, cleanup));
        }

        let bar_lease = match bar_allocator.reserve_exact(self.bar_range) {
            Ok(lease) => lease,
            Err(source) => {
                let primary = VirtioPciPublicationError::BarAllocation { source };
                let cleanup = cleanup_prepared_endpoint(&self.endpoint, &mut interrupts);
                return Err(publication_rollback(primary, cleanup));
            }
        };
        if let Err(primary) = hook(RetainedPublicationStage::Bar) {
            let cleanup = cleanup_retained_bar_and_endpoint(
                &self.endpoint,
                &mut interrupts,
                bar_allocator,
                &bar_lease,
            );
            return Err(publication_rollback(primary, cleanup));
        }

        let function_lease = match segment.with_segment(|segment| {
            segment.add_function_at(self.sbdf, self.endpoint.config_function())
        }) {
            Ok(Ok(lease)) => lease,
            Ok(Err(source)) => {
                let primary = VirtioPciPublicationError::SegmentAdd { source };
                let cleanup = cleanup_retained_bar_and_endpoint(
                    &self.endpoint,
                    &mut interrupts,
                    bar_allocator,
                    &bar_lease,
                );
                return Err(publication_rollback(primary, cleanup));
            }
            Err(source) => {
                let primary = VirtioPciPublicationError::SegmentLock { source };
                let cleanup = cleanup_retained_bar_and_endpoint(
                    &self.endpoint,
                    &mut interrupts,
                    bar_allocator,
                    &bar_lease,
                );
                return Err(publication_rollback(primary, cleanup));
            }
        };
        if let Err(primary) = hook(RetainedPublicationStage::Function) {
            let cleanup = cleanup_retained_function(
                &self.endpoint,
                &mut interrupts,
                bar_allocator,
                &bar_lease,
                &segment,
                &function_lease,
            );
            return Err(publication_rollback(primary, cleanup));
        }

        let owner = MmioRegistrationOwner::new();
        let mmio_lease = match dispatcher.register_owned_handler(
            &owner,
            self.region_id,
            &regions,
            self.endpoint.bar_handler(),
        ) {
            Ok(lease) => lease,
            Err(source) => {
                let primary = VirtioPciPublicationError::MmioRegistration { source };
                let cleanup = cleanup_retained_function(
                    &self.endpoint,
                    &mut interrupts,
                    bar_allocator,
                    &bar_lease,
                    &segment,
                    &function_lease,
                );
                return Err(publication_rollback(primary, cleanup));
            }
        };
        if let Err(primary) = hook(RetainedPublicationStage::Mmio) {
            let cleanup = cleanup_retained_mmio(
                &self.endpoint,
                &mut interrupts,
                bar_allocator,
                &bar_lease,
                &segment,
                &function_lease,
                dispatcher,
                &owner,
                &mmio_lease,
            );
            return Err(publication_rollback(primary, cleanup));
        }

        Ok(PublishedVirtioPciEndpoint {
            endpoint: self.endpoint,
            segment,
            owner,
            mmio_lease: Some(mmio_lease),
            mmio_published: true,
            function_lease: Some(function_lease),
            function_published: true,
            bar_lease: Some(bar_lease),
            interrupts,
            teardown_prepared: false,
            released: false,
        })
    }
}

/// Published endpoint resources retained until ordered teardown completes.
pub struct PublishedVirtioPciEndpoint<C, A, I> {
    endpoint: VirtioPciEndpoint<C, A>,
    segment: SharedPciSegment,
    owner: MmioRegistrationOwner,
    mmio_lease: Option<MmioRegistrationLease>,
    mmio_published: bool,
    function_lease: Option<PciFunctionLease>,
    function_published: bool,
    bar_lease: Option<PciBarLease>,
    interrupts: I,
    teardown_prepared: bool,
    released: bool,
}

impl<C, A, I> fmt::Debug for PublishedVirtioPciEndpoint<C, A, I> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublishedVirtioPciEndpoint")
            .field("endpoint", &"<redacted>")
            .field("published", &!self.released)
            .finish_non_exhaustive()
    }
}

impl<C, A, I> PublishedVirtioPciEndpoint<C, A, I>
where
    C: VirtioDeviceConfigHandler + 'static,
    A: VirtioDeviceActivationHandler + 'static,
    I: GuestMessageInterruptResources,
{
    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        identity: VirtioPciIdentity,
        queue_max_sizes: &[u16],
        device_config: C,
        activation: A,
        requires_device_config_write_status: bool,
        bar_allocator: &mut PciBarAllocator,
        segment: SharedPciSegment,
        dispatcher: &mut MmioDispatcher,
        region_id: MmioRegionId,
        mut interrupts: I,
    ) -> Result<Self, VirtioPciPublicationError> {
        let guest_logger = dispatcher.guest_logger();
        let logger_device_kind =
            LoggerDeviceKind::from_virtio_device_id(identity.device_type().raw_value());
        let result = (|| {
            let bar_lease = match bar_allocator.allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE) {
                Ok(lease) => lease,
                Err(source) => {
                    let primary = VirtioPciPublicationError::BarAllocation { source };
                    let mut cleanup = String::new();
                    if let Err(source) = interrupts.release() {
                        append_cleanup(&mut cleanup, "interrupt resources", &source);
                    }
                    return Err(publication_rollback(primary, cleanup));
                }
            };
            let endpoint = match VirtioPciEndpoint::new(
                identity,
                queue_max_sizes,
                device_config,
                activation,
                requires_device_config_write_status,
                &bar_lease,
                interrupts.registry(),
            ) {
                Ok(endpoint) => endpoint,
                Err(source) => {
                    let primary = VirtioPciPublicationError::Endpoint { source };
                    let mut cleanup = String::new();
                    if let Err(source) = interrupts.release() {
                        append_cleanup(&mut cleanup, "interrupt resources", &source);
                    }
                    if let Err(source) = bar_allocator.release(&bar_lease) {
                        append_cleanup(&mut cleanup, "BAR", &source);
                    }
                    return Err(publication_rollback(primary, cleanup));
                }
            };
            endpoint.attach_guest_logger(guest_logger.clone());

            let function_lease = match segment
                .with_segment(|segment| segment.add_function(endpoint.config_function()))
            {
                Ok(Ok(lease)) => lease,
                Ok(Err(source)) => {
                    let primary = VirtioPciPublicationError::SegmentAdd { source };
                    let cleanup = cleanup_unpublished_endpoint(
                        &endpoint,
                        &mut interrupts,
                        bar_allocator,
                        &bar_lease,
                    );
                    return Err(publication_rollback(primary, cleanup));
                }
                Err(source) => {
                    let primary = VirtioPciPublicationError::SegmentLock { source };
                    let cleanup = cleanup_unpublished_endpoint(
                        &endpoint,
                        &mut interrupts,
                        bar_allocator,
                        &bar_lease,
                    );
                    return Err(publication_rollback(primary, cleanup));
                }
            };

            let owner = MmioRegistrationOwner::new();
            let regions = [MmioRegionRequest::new(
                bar_lease.range().start(),
                bar_lease.range().size(),
            )];
            let mmio_lease = match dispatcher.register_owned_handler(
                &owner,
                region_id,
                &regions,
                endpoint.bar_handler(),
            ) {
                Ok(lease) => lease,
                Err(source) => {
                    let primary = VirtioPciPublicationError::MmioRegistration { source };
                    let mut cleanup = String::new();
                    match segment.with_segment(|segment| segment.remove_function(&function_lease)) {
                        Ok(Ok(())) => {}
                        Ok(Err(source)) => {
                            append_cleanup(&mut cleanup, "PCI function", &source);
                        }
                        Err(source) => append_cleanup(&mut cleanup, "PCI segment lock", &source),
                    }
                    let remainder = cleanup_unpublished_endpoint(
                        &endpoint,
                        &mut interrupts,
                        bar_allocator,
                        &bar_lease,
                    );
                    if !remainder.is_empty() {
                        if !cleanup.is_empty() {
                            cleanup.push_str("; ");
                        }
                        cleanup.push_str(&remainder);
                    }
                    return Err(publication_rollback(primary, cleanup));
                }
            };

            Ok(Self {
                endpoint,
                segment,
                owner,
                mmio_lease: Some(mmio_lease),
                mmio_published: true,
                function_lease: Some(function_lease),
                function_published: true,
                bar_lease: Some(bar_lease),
                interrupts,
                teardown_prepared: false,
                released: false,
            })
        })();
        if let Some(kind) = logger_device_kind {
            guest_logger.log_transport(if result.is_ok() {
                LoggerTransportOutcome::PciFunctionPublished(kind)
            } else {
                LoggerTransportOutcome::PciFunctionPublicationFailed(kind)
            });
        }
        result
    }

    pub const fn endpoint(&self) -> &VirtioPciEndpoint<C, A> {
        &self.endpoint
    }

    pub fn bar_range(&self) -> Option<crate::memory::GuestMemoryRange> {
        self.bar_lease.as_ref().map(PciBarLease::range)
    }

    pub fn sbdf(&self) -> Option<crate::pci::PciSbdf> {
        self.function_lease.as_ref().map(PciFunctionLease::sbdf)
    }

    pub const fn is_released(&self) -> bool {
        self.released
    }

    /// Suspends every guest-visible path and drains endpoint work while
    /// retaining the exact resource leases needed for rollback.
    pub fn prepare_teardown(
        &mut self,
        dispatcher: &mut MmioDispatcher,
    ) -> Result<(), VirtioPciPublicationError> {
        if self.released || self.teardown_prepared {
            return Ok(());
        }
        if self.mmio_published
            && let Some(lease) = self.mmio_lease.as_ref()
        {
            dispatcher
                .unpublish_owned_handler(&self.owner, lease)
                .map_err(|source| VirtioPciPublicationError::MmioRelease { source })?;
            self.mmio_published = false;
        }
        if self.function_published
            && let Some(lease) = self.function_lease.as_ref()
        {
            let result = self
                .segment
                .with_segment(|segment| segment.unpublish_function(lease))
                .map_err(|source| VirtioPciPublicationError::SegmentLock { source })?
                .map_err(|source| VirtioPciPublicationError::FunctionRelease { source });
            if let Err(primary) = result {
                let mut cleanup = String::new();
                if let Some(mmio_lease) = self.mmio_lease.as_ref() {
                    match dispatcher.republish_owned_handler(&self.owner, mmio_lease) {
                        Ok(()) => self.mmio_published = true,
                        Err(source) => append_cleanup(&mut cleanup, "MMIO registration", &source),
                    }
                }
                return Err(publication_rollback(primary, cleanup));
            }
            self.function_published = false;
        }
        if let Err(primary) = self.endpoint.begin_quiesce() {
            let mut cleanup = String::new();
            if let Some(lease) = self.function_lease.as_ref() {
                match self
                    .segment
                    .with_segment(|segment| segment.republish_function(lease))
                {
                    Ok(Ok(())) => self.function_published = true,
                    Ok(Err(source)) => append_cleanup(&mut cleanup, "PCI function", &source),
                    Err(source) => append_cleanup(&mut cleanup, "PCI segment lock", &source),
                }
            }
            if let Some(lease) = self.mmio_lease.as_ref() {
                match dispatcher.republish_owned_handler(&self.owner, lease) {
                    Ok(()) => self.mmio_published = true,
                    Err(source) => append_cleanup(&mut cleanup, "MMIO registration", &source),
                }
            }
            return Err(publication_rollback(
                VirtioPciPublicationError::EndpointRelease { source: primary },
                cleanup,
            ));
        }
        self.teardown_prepared = true;
        Ok(())
    }

    /// Restores the exact endpoint and guest paths retained by
    /// [`Self::prepare_teardown`].
    pub fn rollback_prepared_teardown(
        &mut self,
        dispatcher: &mut MmioDispatcher,
    ) -> Result<(), VirtioPciPublicationError> {
        if self.released || !self.teardown_prepared {
            return Ok(());
        }
        if !self.mmio_published
            && let Some(lease) = self.mmio_lease.as_ref()
        {
            dispatcher
                .republish_owned_handler(&self.owner, lease)
                .map_err(|source| VirtioPciPublicationError::MmioRelease { source })?;
            self.mmio_published = true;
        }
        if !self.function_published
            && let Some(lease) = self.function_lease.as_ref()
        {
            let republish = self
                .segment
                .with_segment(|segment| segment.republish_function(lease))
                .map_err(|source| VirtioPciPublicationError::SegmentLock { source })?
                .map_err(|source| VirtioPciPublicationError::FunctionRelease { source });
            if let Err(primary) = republish {
                let mut cleanup = String::new();
                if self.mmio_published
                    && let Some(mmio_lease) = self.mmio_lease.as_ref()
                {
                    match dispatcher.unpublish_owned_handler(&self.owner, mmio_lease) {
                        Ok(()) => self.mmio_published = false,
                        Err(source) => append_cleanup(&mut cleanup, "MMIO registration", &source),
                    }
                }
                return Err(publication_rollback(primary, cleanup));
            }
            self.function_published = true;
        }
        if let Err(source) = self.endpoint.resume() {
            let primary = VirtioPciPublicationError::EndpointRelease { source };
            let mut cleanup = String::new();
            if self.function_published
                && let Some(lease) = self.function_lease.as_ref()
            {
                match self
                    .segment
                    .with_segment(|segment| segment.unpublish_function(lease))
                {
                    Ok(Ok(())) => self.function_published = false,
                    Ok(Err(source)) => append_cleanup(&mut cleanup, "PCI function", &source),
                    Err(source) => append_cleanup(&mut cleanup, "PCI segment lock", &source),
                }
            }
            if self.mmio_published
                && let Some(lease) = self.mmio_lease.as_ref()
            {
                match dispatcher.unpublish_owned_handler(&self.owner, lease) {
                    Ok(()) => self.mmio_published = false,
                    Err(source) => append_cleanup(&mut cleanup, "MMIO registration", &source),
                }
            }
            return Err(publication_rollback(primary, cleanup));
        }
        self.teardown_prepared = false;
        Ok(())
    }

    /// Crosses the irreversible teardown boundary and returns every retained
    /// lease. Failures here indicate terminal resource-state corruption.
    pub fn commit_prepared_teardown(
        &mut self,
        dispatcher: &mut MmioDispatcher,
        bar_allocator: &mut PciBarAllocator,
    ) -> Result<(), VirtioPciPublicationError> {
        if self.released {
            return Ok(());
        }
        let logging = self.endpoint.logging_context();
        let result = (|| {
            if !self.teardown_prepared {
                return Err(VirtioPciPublicationError::TeardownNotPrepared);
            }
            self.endpoint
                .release()
                .map_err(|source| VirtioPciPublicationError::EndpointRelease { source })?;
            self.interrupts
                .release()
                .map_err(|source| VirtioPciPublicationError::InterruptRelease { source })?;
            if let Some(lease) = self.bar_lease.as_ref() {
                bar_allocator
                    .release(lease)
                    .map_err(|source| VirtioPciPublicationError::BarRelease { source })?;
                self.bar_lease = None;
            }
            if let Some(lease) = self.function_lease.as_ref() {
                self.segment
                    .with_segment(|segment| segment.release_function_lease(lease))
                    .map_err(|source| VirtioPciPublicationError::SegmentLock { source })?
                    .map_err(|source| VirtioPciPublicationError::FunctionRelease { source })?;
                self.function_lease = None;
            }
            if let Some(lease) = self.mmio_lease.as_ref() {
                dispatcher
                    .release_unpublished_handler(&self.owner, lease)
                    .map_err(|source| VirtioPciPublicationError::MmioRelease { source })?;
                self.mmio_lease = None;
            }
            self.teardown_prepared = false;
            self.released = true;
            Ok(())
        })();
        if let Some((logger, kind)) = logging {
            logger.log_transport(if result.is_ok() {
                LoggerTransportOutcome::PciFunctionRemoved(kind)
            } else {
                LoggerTransportOutcome::PciFunctionRemovalFailed(kind)
            });
        }
        result
    }

    /// Unpublish guest paths, drain work and messages, then return all leases.
    pub fn teardown(
        &mut self,
        dispatcher: &mut MmioDispatcher,
        bar_allocator: &mut PciBarAllocator,
    ) -> Result<(), VirtioPciPublicationError> {
        if self.released {
            return Ok(());
        }
        self.prepare_teardown(dispatcher)?;
        self.commit_prepared_teardown(dispatcher, bar_allocator)
    }
}

fn cleanup_unpublished_endpoint<C, A, I>(
    endpoint: &VirtioPciEndpoint<C, A>,
    interrupts: &mut I,
    bar_allocator: &mut PciBarAllocator,
    bar_lease: &PciBarLease,
) -> String
where
    C: VirtioDeviceConfigHandler,
    A: VirtioDeviceActivationHandler,
    I: GuestMessageInterruptResources,
{
    let mut cleanup = String::new();
    if let Err(source) = endpoint.release() {
        append_cleanup(&mut cleanup, "endpoint", &source);
    }
    if let Err(source) = interrupts.release() {
        append_cleanup(&mut cleanup, "interrupt resources", &source);
    }
    if let Err(source) = bar_allocator.release(bar_lease) {
        append_cleanup(&mut cleanup, "BAR", &source);
    }
    cleanup
}

fn cleanup_prepared_endpoint<C, A, I>(
    endpoint: &VirtioPciEndpoint<C, A>,
    interrupts: &mut I,
) -> String
where
    C: VirtioDeviceConfigHandler,
    A: VirtioDeviceActivationHandler,
    I: GuestMessageInterruptResources,
{
    let mut cleanup = String::new();
    if let Err(source) = endpoint.release() {
        append_cleanup(&mut cleanup, "endpoint", &source);
    }
    if let Err(source) = interrupts.release() {
        append_cleanup(&mut cleanup, "interrupt resources", &source);
    }
    cleanup
}

fn cleanup_retained_bar_and_endpoint<C, A, I>(
    endpoint: &VirtioPciEndpoint<C, A>,
    interrupts: &mut I,
    bar_allocator: &mut PciBarAllocator,
    bar_lease: &PciBarLease,
) -> String
where
    C: VirtioDeviceConfigHandler,
    A: VirtioDeviceActivationHandler,
    I: GuestMessageInterruptResources,
{
    let mut cleanup = String::new();
    if let Err(source) = bar_allocator.release(bar_lease) {
        append_cleanup(&mut cleanup, "BAR", &source);
    }
    if let Err(source) = endpoint.release() {
        append_cleanup(&mut cleanup, "endpoint", &source);
    }
    if let Err(source) = interrupts.release() {
        append_cleanup(&mut cleanup, "interrupt resources", &source);
    }
    cleanup
}

fn cleanup_retained_function<C, A, I>(
    endpoint: &VirtioPciEndpoint<C, A>,
    interrupts: &mut I,
    bar_allocator: &mut PciBarAllocator,
    bar_lease: &PciBarLease,
    segment: &SharedPciSegment,
    function_lease: &PciFunctionLease,
) -> String
where
    C: VirtioDeviceConfigHandler,
    A: VirtioDeviceActivationHandler,
    I: GuestMessageInterruptResources,
{
    let mut cleanup = String::new();
    match segment.with_segment(|segment| segment.remove_function(function_lease)) {
        Ok(Ok(())) => {}
        Ok(Err(source)) => append_cleanup(&mut cleanup, "PCI function", &source),
        Err(source) => append_cleanup(&mut cleanup, "PCI segment lock", &source),
    }
    let remainder =
        cleanup_retained_bar_and_endpoint(endpoint, interrupts, bar_allocator, bar_lease);
    if !remainder.is_empty() {
        if !cleanup.is_empty() {
            cleanup.push_str("; ");
        }
        cleanup.push_str(&remainder);
    }
    cleanup
}

#[allow(clippy::too_many_arguments)]
fn cleanup_retained_mmio<C, A, I>(
    endpoint: &VirtioPciEndpoint<C, A>,
    interrupts: &mut I,
    bar_allocator: &mut PciBarAllocator,
    bar_lease: &PciBarLease,
    segment: &SharedPciSegment,
    function_lease: &PciFunctionLease,
    dispatcher: &mut MmioDispatcher,
    owner: &MmioRegistrationOwner,
    mmio_lease: &MmioRegistrationLease,
) -> String
where
    C: VirtioDeviceConfigHandler,
    A: VirtioDeviceActivationHandler,
    I: GuestMessageInterruptResources,
{
    let mut cleanup = String::new();
    if let Err(source) = dispatcher.release_owned_handler(owner, mmio_lease) {
        append_cleanup(&mut cleanup, "MMIO registration", &source);
    }
    let remainder = cleanup_retained_function(
        endpoint,
        interrupts,
        bar_allocator,
        bar_lease,
        segment,
        function_lease,
    );
    if !remainder.is_empty() {
        if !cleanup.is_empty() {
            cleanup.push_str("; ");
        }
        cleanup.push_str(&remainder);
    }
    cleanup
}

fn append_cleanup(cleanup: &mut String, stage: &str, source: &impl fmt::Display) {
    if !cleanup.is_empty() {
        cleanup.push_str("; ");
    }
    use std::fmt::Write as _;
    let _ = write!(cleanup, "{stage}: {source}");
}

fn publication_rollback(
    primary: VirtioPciPublicationError,
    cleanup: String,
) -> VirtioPciPublicationError {
    if cleanup.is_empty() {
        primary
    } else {
        VirtioPciPublicationError::Rollback {
            primary: primary.to_string(),
            cleanup,
        }
    }
}

#[derive(Debug)]
pub enum VirtioPciPublicationError {
    TeardownNotPrepared,
    InterruptRegistryMismatch,
    #[cfg(test)]
    InjectedFailure {
        stage: &'static str,
    },
    BarAllocation {
        source: PciBarAllocationError,
    },
    Endpoint {
        source: VirtioPciEndpointError,
    },
    SegmentLock {
        source: PciSegmentLockError,
    },
    SegmentAdd {
        source: PciSegmentError,
    },
    MmioRegistration {
        source: MmioRegistrationError,
    },
    MmioRelease {
        source: MmioRegistrationReleaseError,
    },
    FunctionRelease {
        source: PciFunctionReleaseError,
    },
    EndpointRelease {
        source: VirtioPciEndpointError,
    },
    InterruptRelease {
        source: GuestMessageInterruptResourcesError,
    },
    BarRelease {
        source: PciBarReleaseError,
    },
    Rollback {
        primary: String,
        cleanup: String,
    },
}

impl fmt::Display for VirtioPciPublicationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TeardownNotPrepared => f.write_str("virtio-pci teardown was not prepared"),
            Self::InterruptRegistryMismatch => {
                f.write_str("virtio-pci interrupt resources do not own the prepared registry")
            }
            #[cfg(test)]
            Self::InjectedFailure { stage } => {
                write!(
                    f,
                    "injected retained virtio-pci publication failure after {stage}"
                )
            }
            Self::BarAllocation { source } => {
                write!(f, "failed to allocate virtio-pci BAR: {source}")
            }
            Self::Endpoint { source } => {
                write!(f, "failed to prepare virtio-pci endpoint: {source}")
            }
            Self::SegmentLock { source } => write!(f, "PCI segment lock failed: {source}"),
            Self::SegmentAdd { source } => {
                write!(f, "failed to publish virtio-pci function: {source}")
            }
            Self::MmioRegistration { source } => {
                write!(f, "failed to publish virtio-pci BAR handler: {source}")
            }
            Self::MmioRelease { source } => {
                write!(f, "failed to unpublish virtio-pci BAR handler: {source}")
            }
            Self::FunctionRelease { source } => {
                write!(f, "failed to unpublish virtio-pci function: {source}")
            }
            Self::EndpointRelease { source } => {
                write!(f, "failed to release virtio-pci endpoint: {source}")
            }
            Self::InterruptRelease { source } => {
                write!(f, "failed to release virtio-pci interrupts: {source}")
            }
            Self::BarRelease { source } => {
                write!(f, "failed to release virtio-pci BAR: {source}")
            }
            Self::Rollback { primary, cleanup } => {
                write!(f, "{primary}; publication rollback also failed: {cleanup}")
            }
        }
    }
}

impl std::error::Error for VirtioPciPublicationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TeardownNotPrepared | Self::InterruptRegistryMismatch => None,
            #[cfg(test)]
            Self::InjectedFailure { .. } => None,
            Self::BarAllocation { source } => Some(source),
            Self::Endpoint { source } => Some(source),
            Self::SegmentLock { source } => Some(source),
            Self::SegmentAdd { source } => Some(source),
            Self::MmioRegistration { source } => Some(source),
            Self::MmioRelease { source } => Some(source),
            Self::FunctionRelease { source } => Some(source),
            Self::EndpointRelease { source } => Some(source),
            Self::InterruptRelease { source } => Some(source),
            Self::BarRelease { source } => Some(source),
            Self::Rollback { .. } => None,
        }
    }
}

fn pci_handler_error(source: VirtioPciEndpointError) -> PciConfigAccessError {
    PciConfigAccessError::Handler {
        message: source.to_string(),
    }
}

fn transport_outcome_for_endpoint_error(
    kind: LoggerDeviceKind,
    error: &VirtioPciEndpointError,
) -> LoggerTransportOutcome {
    match error {
        VirtioPciEndpointError::DeviceConfigNotWritable { .. } => {
            LoggerTransportOutcome::DeviceConfigRejected(kind)
        }
        VirtioPciEndpointError::DeviceConfig { .. }
        | VirtioPciEndpointError::DeviceConfigReadLength { .. } => {
            LoggerTransportOutcome::DeviceConfigFailed(kind)
        }
        VirtioPciEndpointError::UnsupportedDeviceOperation => {
            LoggerTransportOutcome::DeviceResetUnsupported(kind)
        }
        VirtioPciEndpointError::DeviceReset { .. } => {
            LoggerTransportOutcome::DeviceResetFailed(kind)
        }
        VirtioPciEndpointError::QueueInitialization { .. }
        | VirtioPciEndpointError::Queue { .. }
        | VirtioPciEndpointError::InvalidQueueIndex { .. } => {
            LoggerTransportOutcome::QueueConfigurationRejected(kind)
        }
        VirtioPciEndpointError::QueueNotificationInitialization { .. }
        | VirtioPciEndpointError::QueueNotification { .. } => {
            LoggerTransportOutcome::QueueNotificationFailed(kind)
        }
        VirtioPciEndpointError::DeviceRegisters { .. } => {
            LoggerTransportOutcome::FeatureNegotiationRejected(kind)
        }
        VirtioPciEndpointError::PciConfigurationAccess => {
            LoggerTransportOutcome::PciConfigRejected(kind)
        }
        VirtioPciEndpointError::MessageRegistry { .. } => {
            LoggerTransportOutcome::InterruptDeliveryFailed(kind)
        }
        VirtioPciEndpointError::VectorCountOverflow
        | VirtioPciEndpointError::TooManyVectors { .. }
        | VirtioPciEndpointError::MessageRouteCount { .. }
        | VirtioPciEndpointError::InvalidVectorIndex { .. }
        | VirtioPciEndpointError::RetainedState {
            kind: VirtioPciRetainedStateError::Msix,
        } => LoggerTransportOutcome::MsiConfigurationFailed(kind),
        VirtioPciEndpointError::CapabilityBarSize { .. }
        | VirtioPciEndpointError::CapabilityBarAddressSpace { .. }
        | VirtioPciEndpointError::BarConfiguration { .. }
        | VirtioPciEndpointError::Capability { .. }
        | VirtioPciEndpointError::StatePoisoned
        | VirtioPciEndpointError::NotActive { .. }
        | VirtioPciEndpointError::WorkGate { .. }
        | VirtioPciEndpointError::OutsideCapabilityBar { .. }
        | VirtioPciEndpointError::InvalidBarAccessWidth { .. }
        | VirtioPciEndpointError::RetainedState { .. } => {
            LoggerTransportOutcome::MmioAccessFailed(Some(kind))
        }
    }
}

#[derive(Debug)]
pub enum VirtioPciEndpointError {
    QueueInitialization {
        source: VirtioQueueError,
    },
    QueueNotificationInitialization {
        source: VirtioQueueNotificationError,
    },
    VectorCountOverflow,
    TooManyVectors {
        vector_count: usize,
    },
    MessageRouteCount {
        expected: usize,
        actual: usize,
    },
    CapabilityBarSize {
        expected: u64,
        actual: u64,
    },
    CapabilityBarAddressSpace {
        actual: PciBarAddressSpace,
    },
    BarConfiguration {
        source: PciBarConfigurationError,
    },
    Capability {
        source: PciCapabilityError,
    },
    StatePoisoned,
    NotActive {
        phase: VirtioPciEndpointPhase,
    },
    WorkGate {
        source: crate::virtio::VirtioDeviceWorkGateError,
    },
    MessageRegistry {
        source: GuestMessageInterruptRegistryError,
    },
    InvalidQueueIndex {
        queue_index: u16,
        queue_count: usize,
    },
    InvalidVectorIndex {
        vector: u16,
        vector_count: usize,
    },
    PciConfigurationAccess,
    OutsideCapabilityBar {
        offset: u64,
        len: usize,
    },
    InvalidBarAccessWidth {
        len: usize,
    },
    DeviceConfig {
        source: VirtioDeviceConfigError,
    },
    DeviceConfigReadLength {
        expected: usize,
        actual: usize,
    },
    DeviceConfigNotWritable {
        status: u32,
    },
    UnsupportedDeviceOperation,
    DeviceReset {
        source: VirtioDeviceResetError,
    },
    Queue {
        source: VirtioMmioQueueRegisterError,
    },
    QueueNotification {
        source: VirtioQueueNotificationError,
    },
    DeviceRegisters {
        source: VirtioMmioRegisterStateError,
    },
    RetainedState {
        kind: VirtioPciRetainedStateError,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioPciRetainedStateError {
    Phase,
    DeviceIdentity,
    Features,
    Status,
    Activation,
    DeviceConfigPolicy,
    Queue,
    Notification,
    InterruptIntent,
    Sbdf,
    Bar,
    Configuration,
    Msix,
    Recapture,
}

impl fmt::Display for VirtioPciEndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueInitialization { source } => {
                write!(f, "failed to initialize virtio-pci queues: {source}")
            }
            Self::QueueNotificationInitialization { source } => {
                write!(f, "failed to initialize virtio-pci notifications: {source}")
            }
            Self::VectorCountOverflow => f.write_str("virtio-pci vector count overflowed"),
            Self::TooManyVectors { vector_count } => write!(
                f,
                "virtio-pci vector count {vector_count} exceeds {VIRTIO_PCI_MAX_MSIX_VECTORS}"
            ),
            Self::MessageRouteCount { expected, actual } => write!(
                f,
                "virtio-pci message route count {actual} is smaller than required vector count {expected}"
            ),
            Self::CapabilityBarSize { expected, actual } => write!(
                f,
                "virtio-pci capability BAR size 0x{actual:x} does not match 0x{expected:x}"
            ),
            Self::CapabilityBarAddressSpace { actual } => write!(
                f,
                "virtio-pci capability BAR requires 64-bit memory space, not {actual:?}"
            ),
            Self::BarConfiguration { source } => {
                write!(f, "failed to configure virtio-pci BAR: {source}")
            }
            Self::Capability { source } => {
                write!(f, "failed to configure virtio-pci capability: {source}")
            }
            Self::StatePoisoned => f.write_str("virtio-pci endpoint state is unavailable"),
            Self::NotActive { phase } => {
                write!(f, "virtio-pci endpoint is not active ({phase:?})")
            }
            Self::WorkGate { source } => write!(f, "virtio-pci work gate failed: {source}"),
            Self::MessageRegistry { source } => {
                write!(f, "virtio-pci message delivery failed: {source}")
            }
            Self::InvalidQueueIndex {
                queue_index,
                queue_count,
            } => write!(
                f,
                "virtio-pci queue index {queue_index} exceeds queue count {queue_count}"
            ),
            Self::InvalidVectorIndex {
                vector,
                vector_count,
            } => write!(
                f,
                "virtio-pci vector index {vector} exceeds vector count {vector_count}"
            ),
            Self::PciConfigurationAccess => {
                f.write_str("virtio-pci configuration access is invalid")
            }
            Self::OutsideCapabilityBar { offset, len } => write!(
                f,
                "virtio-pci BAR access at offset 0x{offset:x} with length {len} is outside the capability BAR"
            ),
            Self::InvalidBarAccessWidth { len } => {
                write!(f, "virtio-pci BAR access width {len} is unsupported")
            }
            Self::DeviceConfig { source } => {
                write!(f, "virtio-pci device configuration failed: {source}")
            }
            Self::DeviceConfigReadLength { expected, actual } => write!(
                f,
                "virtio-pci device configuration returned {actual} bytes; expected {expected}"
            ),
            Self::DeviceConfigNotWritable { status } => write!(
                f,
                "virtio-pci device configuration is not writable while status is 0x{status:x}"
            ),
            Self::UnsupportedDeviceOperation => {
                f.write_str("virtio-pci device operation is unsupported")
            }
            Self::DeviceReset { source } => write!(f, "virtio-pci device reset failed: {source}"),
            Self::Queue { source } => write!(f, "virtio-pci queue update failed: {source}"),
            Self::QueueNotification { source } => {
                write!(f, "virtio-pci queue notification failed: {source}")
            }
            Self::DeviceRegisters { source } => {
                write!(f, "virtio-pci device register update failed: {source}")
            }
            Self::RetainedState { kind } => {
                write!(f, "retained virtio-pci {kind} state is invalid")
            }
        }
    }
}

impl fmt::Display for VirtioPciRetainedStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Phase => "phase",
            Self::DeviceIdentity => "device identity",
            Self::Features => "feature",
            Self::Status => "device status",
            Self::Activation => "activation",
            Self::DeviceConfigPolicy => "device-config policy",
            Self::Queue => "queue",
            Self::Notification => "notification",
            Self::InterruptIntent => "interrupt-intent",
            Self::Sbdf => "SBDF",
            Self::Bar => "BAR",
            Self::Configuration => "PCI configuration",
            Self::Msix => "MSI-X",
            Self::Recapture => "recapture",
        };
        formatter.write_str(name)
    }
}

impl VirtioPciEndpointError {
    /// Reports whether a failed guest-message signal may already have reached
    /// the guest and therefore cannot be rolled back honestly.
    pub fn delivery_ambiguous(&self) -> bool {
        matches!(
            self,
            Self::MessageRegistry {
                source: GuestMessageInterruptRegistryError::Signal { source }
            } if source.delivery_ambiguous()
        )
    }
}

impl std::error::Error for VirtioPciEndpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueInitialization { source } => Some(source),
            Self::QueueNotificationInitialization { source } => Some(source),
            Self::BarConfiguration { source } => Some(source),
            Self::Capability { source } => Some(source),
            Self::WorkGate { source } => Some(source),
            Self::MessageRegistry { source } => Some(source),
            Self::DeviceConfig { source } => Some(source),
            Self::DeviceReset { source } => Some(source),
            Self::Queue { source } => Some(source),
            Self::QueueNotification { source } => Some(source),
            Self::DeviceRegisters { source } => Some(source),
            Self::RetainedState { .. } => None,
            Self::VectorCountOverflow
            | Self::TooManyVectors { .. }
            | Self::MessageRouteCount { .. }
            | Self::CapabilityBarSize { .. }
            | Self::CapabilityBarAddressSpace { .. }
            | Self::StatePoisoned
            | Self::NotActive { .. }
            | Self::InvalidQueueIndex { .. }
            | Self::InvalidVectorIndex { .. }
            | Self::PciConfigurationAccess
            | Self::OutsideCapabilityBar { .. }
            | Self::InvalidBarAccessWidth { .. }
            | Self::DeviceConfigReadLength { .. }
            | Self::DeviceConfigNotWritable { .. }
            | Self::UnsupportedDeviceOperation => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{self, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::mpsc::{self, TryRecvError};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::logger::{LoggerConfigInput, LoggerLevel, LoggerState};
    use crate::memory::{GuestMemory, GuestMemoryLayout, GuestMemoryRange};
    use crate::message_interrupt::{
        GuestMessageInterrupt, GuestMessageInterruptResources, GuestMessageInterruptResourcesError,
        GuestMessageInterruptSignalError, RegistryGuestMessageInterruptResources,
    };
    use crate::metrics::SharedVsockDeviceMetrics;
    use crate::mmio::{MmioBus, MmioHandler};
    use crate::pci::{PciBarAddressSpace, PciBarAllocator};
    use crate::virtio::{
        NoopVirtioDeviceActivation, UnsupportedVirtioDeviceConfig, VirtioDeviceActivationError,
        VirtioDeviceResetError,
    };
    use crate::virtio_queue::{VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE};
    use crate::vsock::{
        PreparedVsockDevice, SuppliedVsockListener, VIRTIO_FEATURE_VERSION_1,
        VIRTIO_RING_FEATURE_EVENT_IDX, VIRTIO_VSOCK_DEVICE_ID, VIRTIO_VSOCK_QUEUE_SIZES,
        VirtioVsockConfigSpace, VirtioVsockDevice, VirtioVsockReconstructionResource,
        VirtioVsockRestoredTransportResetSignal, VirtioVsockTransportResetAttempt,
        VsockBackendSelector, VsockConfig, VsockConfigInput, VsockGuestConnector,
    };

    const TEST_VSOCK_PCI_UDS_PATH: &str = "/tmp/bangbang-vsock-pci-capture.sock";

    #[derive(Clone)]
    struct RecordingLogWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for RecordingLogWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("logger output lock should succeed")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct RejectingVsockGuestConnector;

    impl VsockGuestConnector for RejectingVsockGuestConnector {
        fn connect(&mut self, _host_port: u32) -> std::io::Result<UnixStream> {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        }
    }

    type TestEndpoint =
        VirtioPciEndpoint<UnsupportedVirtioDeviceConfig, NoopVirtioDeviceActivation>;

    #[derive(Debug)]
    struct RecordingRoute {
        message: GuestMessage,
        signals: Arc<Mutex<Vec<GuestMessage>>>,
    }

    impl GuestMessageInterrupt for RecordingRoute {
        fn matches(&self, message: GuestMessage) -> bool {
            self.message == message
        }

        fn signal(&self, message: GuestMessage) -> Result<(), GuestMessageInterruptSignalError> {
            if !self.matches(message) {
                return Err(GuestMessageInterruptSignalError::new(
                    "recording route rejected a mismatched message",
                    false,
                ));
            }
            self.signals
                .lock()
                .expect("recording route mutex should be healthy")
                .push(message);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct CountingInterruptResources {
        registry: GuestMessageInterruptRegistry,
        releases: Arc<Mutex<usize>>,
    }

    impl GuestMessageInterruptResources for CountingInterruptResources {
        fn registry(&self) -> GuestMessageInterruptRegistry {
            self.registry.clone()
        }

        fn release(&mut self) -> Result<(), GuestMessageInterruptResourcesError> {
            *self
                .releases
                .lock()
                .expect("release counter should be healthy") += 1;
            self.registry
                .release()
                .map_err(|source| GuestMessageInterruptResourcesError::new(source.to_string()))
        }
    }

    struct TestFixture {
        endpoint: TestEndpoint,
        bar: PciBarLease,
        _allocator: PciBarAllocator,
        messages: Vec<GuestMessage>,
        signals: Vec<Arc<Mutex<Vec<GuestMessage>>>>,
    }

    struct VsockTestFixture {
        endpoint: VirtioPciEndpoint<VirtioVsockConfigSpace, VirtioVsockDevice>,
        bar: PciBarLease,
        _allocator: PciBarAllocator,
        messages: Vec<GuestMessage>,
        signals: Vec<Arc<Mutex<Vec<GuestMessage>>>>,
    }

    struct RetainedTransportFixture {
        state: VirtioPciTransportState,
        identity: VirtioPciIdentity,
        bar_range: GuestMemoryRange,
        sbdf: PciSbdf,
        region_id: MmioRegionId,
        messages: Vec<GuestMessage>,
    }

    struct CountingResourcesFixture {
        registry: GuestMessageInterruptRegistry,
        resources: CountingInterruptResources,
        signals: Vec<Arc<Mutex<Vec<GuestMessage>>>>,
        releases: Arc<Mutex<usize>>,
    }

    #[derive(Debug, Default)]
    struct ResetStatistics {
        activations: usize,
        reset_attempts: usize,
    }

    #[derive(Debug, Clone, Copy)]
    enum ResetMode {
        Unsupported,
        Failure,
    }

    #[derive(Debug, Clone)]
    struct ResetActivation {
        mode: ResetMode,
        statistics: Arc<Mutex<ResetStatistics>>,
    }

    impl VirtioDeviceActivationHandler for ResetActivation {
        fn activate(
            &mut self,
            _activation: VirtioDeviceActivation<'_>,
        ) -> Result<(), VirtioDeviceActivationError> {
            self.statistics
                .lock()
                .expect("reset statistics should be healthy")
                .activations += 1;
            Ok(())
        }

        fn reset_outcome(&mut self) -> Result<VirtioDeviceResetOutcome, VirtioDeviceResetError> {
            self.statistics
                .lock()
                .expect("reset statistics should be healthy")
                .reset_attempts += 1;
            match self.mode {
                ResetMode::Unsupported => Ok(VirtioDeviceResetOutcome::Unsupported),
                ResetMode::Failure => Err(MmioHandlerError::new("injected reset failure").into()),
            }
        }
    }

    fn fixture(queue_max_sizes: &[u16]) -> TestFixture {
        fixture_for_device_type(queue_max_sizes, 4)
    }

    fn fixture_for_device_type(queue_max_sizes: &[u16], device_type: u32) -> TestFixture {
        fixture_for_identity(queue_max_sizes, device_type, 0)
    }

    fn fixture_for_identity(
        queue_max_sizes: &[u16],
        device_type: u32,
        device_features: u64,
    ) -> TestFixture {
        let range = GuestMemoryRange::new(
            GuestAddress::new(0x1_0000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE * 2,
        )
        .expect("test BAR allocator range should validate");
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, range);
        let bar = allocator
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("test BAR should allocate");
        let vector_count = queue_max_sizes.len() + 1;
        let mut messages = Vec::new();
        let mut signals = Vec::new();
        let mut routes: Vec<Arc<dyn GuestMessageInterrupt>> = Vec::new();
        for index in 0..vector_count {
            let message = GuestMessage::new(
                0x0800_0040,
                u32::try_from(64 + index).expect("test vector should fit u32"),
            );
            let recording = Arc::new(Mutex::new(Vec::new()));
            routes.push(Arc::new(RecordingRoute {
                message,
                signals: Arc::clone(&recording),
            }));
            messages.push(message);
            signals.push(recording);
        }
        let registry = GuestMessageInterruptRegistry::new(routes)
            .expect("test message registry should validate");
        let endpoint = VirtioPciEndpoint::new(
            VirtioPciIdentity::new(
                VirtioDeviceType::new(device_type).expect("test virtio type should validate"),
                device_features,
            ),
            queue_max_sizes,
            UnsupportedVirtioDeviceConfig,
            NoopVirtioDeviceActivation,
            false,
            &bar,
            registry,
        )
        .expect("test endpoint should initialize");
        TestFixture {
            endpoint,
            bar,
            _allocator: allocator,
            messages,
            signals,
        }
    }

    fn vsock_fixture() -> VsockTestFixture {
        let range = GuestMemoryRange::new(
            GuestAddress::new(0x5_0000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE * 2,
        )
        .expect("vsock test BAR allocator range should validate");
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, range);
        let bar = allocator
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("vsock test BAR should allocate");
        let vector_count = VIRTIO_VSOCK_QUEUE_SIZES.len() + 1;
        let mut messages = Vec::new();
        let mut signals = Vec::new();
        let mut routes: Vec<Arc<dyn GuestMessageInterrupt>> = Vec::new();
        for index in 0..vector_count {
            let message = GuestMessage::new(
                0x0800_0080,
                u32::try_from(128 + index).expect("vsock test vector should fit"),
            );
            let recording = Arc::new(Mutex::new(Vec::new()));
            routes.push(Arc::new(RecordingRoute {
                message,
                signals: Arc::clone(&recording),
            }));
            messages.push(message);
            signals.push(recording);
        }
        let registry = GuestMessageInterruptRegistry::new(routes)
            .expect("vsock test message registry should validate");
        let prepared = PreparedVsockDevice::from_config(&vsock_pci_config());
        let (_, _, config, device) = prepared.into_parts();
        let endpoint = VirtioPciEndpoint::new(
            VirtioPciIdentity::new(
                VirtioDeviceType::new(VIRTIO_VSOCK_DEVICE_ID)
                    .expect("vsock device type should validate"),
                config.available_features(),
            ),
            &VIRTIO_VSOCK_QUEUE_SIZES,
            config,
            device,
            false,
            &bar,
            registry,
        )
        .expect("vsock endpoint should initialize");
        VsockTestFixture {
            endpoint,
            bar,
            _allocator: allocator,
            messages,
            signals,
        }
    }

    fn vsock_pci_config() -> VsockConfig {
        VsockConfigInput::new(42, TEST_VSOCK_PCI_UDS_PATH)
            .validate()
            .expect("PCI vsock test configuration should validate")
    }

    fn configure_vsock_endpoint(
        fixture: &VsockTestFixture,
        event_message: GuestMessage,
        masked: bool,
    ) {
        let bus = bar_bus_for_bar(&fixture.bar);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();
        let event_vector = 2_u16;
        let event_entry =
            VIRTIO_PCI_MSIX_TABLE_OFFSET + u64::from(event_vector) * MSIX_TABLE_ENTRY_SIZE;

        bar_write(
            &mut bar,
            &bus,
            base,
            event_entry,
            &event_message.address().to_le_bytes(),
        )
        .expect("event MSI-X address should write");
        let data_and_control =
            (u64::from(u32::from(masked)) << 32) | u64::from(event_message.data());
        bar_write(
            &mut bar,
            &bus,
            base,
            event_entry + 8,
            &data_and_control.to_le_bytes(),
        )
        .expect("event MSI-X data and mask should write");

        bar_write(&mut bar, &bus, base, 0x14, &[1]).expect("ACKNOWLEDGE should write");
        bar_write(&mut bar, &bus, base, 0x14, &[3]).expect("DRIVER should write");
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_DRIVER_FEATURE,
            &(1_u32 << VIRTIO_RING_FEATURE_EVENT_IDX).to_le_bytes(),
        )
        .expect("EVENT_IDX should negotiate");
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT,
            &1_u32.to_le_bytes(),
        )
        .expect("VERSION_1 feature page should select");
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_DRIVER_FEATURE,
            &(1_u32 << (VIRTIO_FEATURE_VERSION_1 - 32)).to_le_bytes(),
        )
        .expect("VERSION_1 should negotiate");
        bar_write(&mut bar, &bus, base, 0x14, &[11]).expect("FEATURES_OK should write");
        for queue_index in 0..VIRTIO_VSOCK_QUEUE_SIZES.len() {
            let queue_index_u16 = u16::try_from(queue_index).expect("vsock queue index should fit");
            let queue_base = 0x1000_u64
                + u64::try_from(queue_index).expect("vsock queue index should fit") * 0x1000;
            bar_write(&mut bar, &bus, base, 0x16, &queue_index_u16.to_le_bytes())
                .expect("queue select should write");
            bar_write(&mut bar, &bus, base, 0x18, &8_u16.to_le_bytes())
                .expect("queue size should write");
            bar_write(&mut bar, &bus, base, 0x1a, &queue_index_u16.to_le_bytes())
                .expect("queue vector should write");
            bar_write(
                &mut bar,
                &bus,
                base,
                0x20,
                &u32::try_from(queue_base)
                    .expect("descriptor address should fit")
                    .to_le_bytes(),
            )
            .expect("descriptor address should write");
            bar_write(
                &mut bar,
                &bus,
                base,
                0x28,
                &u32::try_from(queue_base + 0x200)
                    .expect("available address should fit")
                    .to_le_bytes(),
            )
            .expect("available address should write");
            bar_write(
                &mut bar,
                &bus,
                base,
                0x30,
                &u32::try_from(queue_base + 0x400)
                    .expect("used address should fit")
                    .to_le_bytes(),
            )
            .expect("used address should write");
            bar_write(&mut bar, &bus, base, 0x1c, &1_u16.to_le_bytes())
                .expect("queue enable should write");
        }
        bar_write(&mut bar, &bus, base, 0x14, &[15]).expect("DRIVER_OK should activate");
    }

    fn vsock_event_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x20_000)
                .expect("vsock test memory range should validate"),
        ])
        .expect("vsock test memory layout should validate");
        GuestMemory::allocate(&layout).expect("vsock test memory should allocate")
    }

    fn publish_vsock_event_descriptor(memory: &mut GuestMemory, descriptor_len: u32) {
        let mut descriptor = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        descriptor[..8].copy_from_slice(&0x9000_u64.to_le_bytes());
        descriptor[8..12].copy_from_slice(&descriptor_len.to_le_bytes());
        descriptor[12..14].copy_from_slice(&VIRTQUEUE_DESC_F_WRITE.to_le_bytes());
        memory
            .write_slice(&descriptor, GuestAddress::new(0x3000))
            .expect("event descriptor should write");
        memory
            .write_slice(&0_u16.to_le_bytes(), GuestAddress::new(0x3204))
            .expect("event available head should write");
        memory
            .write_slice(&1_u16.to_le_bytes(), GuestAddress::new(0x3202))
            .expect("event available index should write");
    }

    fn read_vsock_event_used_index(memory: &GuestMemory) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, GuestAddress::new(0x3402))
            .expect("event used index should read");
        u16::from_le_bytes(bytes)
    }

    fn registry_resources(vector_count: usize) -> RegistryGuestMessageInterruptResources {
        let routes = (0..vector_count)
            .map(|index| {
                let route: Arc<dyn GuestMessageInterrupt> = Arc::new(RecordingRoute {
                    message: GuestMessage::new(
                        0x0800_0040,
                        u32::try_from(96 + index).expect("test vector should fit u32"),
                    ),
                    signals: Arc::new(Mutex::new(Vec::new())),
                });
                route
            })
            .collect();
        RegistryGuestMessageInterruptResources::new(
            GuestMessageInterruptRegistry::new(routes)
                .expect("test publication registry should validate"),
        )
    }

    fn registry_for_messages(
        messages: &[GuestMessage],
    ) -> (
        GuestMessageInterruptRegistry,
        Vec<Arc<Mutex<Vec<GuestMessage>>>>,
    ) {
        let mut signals = Vec::with_capacity(messages.len());
        let routes = messages
            .iter()
            .copied()
            .map(|message| {
                let recording = Arc::new(Mutex::new(Vec::new()));
                signals.push(Arc::clone(&recording));
                let route: Arc<dyn GuestMessageInterrupt> = Arc::new(RecordingRoute {
                    message,
                    signals: recording,
                });
                route
            })
            .collect();
        (
            GuestMessageInterruptRegistry::new(routes)
                .expect("retained test message registry should validate"),
            signals,
        )
    }

    fn counting_resources_for_messages(messages: &[GuestMessage]) -> CountingResourcesFixture {
        let (registry, signals) = registry_for_messages(messages);
        let releases = Arc::new(Mutex::new(0));
        CountingResourcesFixture {
            registry: registry.clone(),
            resources: CountingInterruptResources {
                registry,
                releases: Arc::clone(&releases),
            },
            signals,
            releases,
        }
    }

    fn retained_transport_fixture() -> RetainedTransportFixture {
        let identity = VirtioPciIdentity::new(VirtioDeviceType::new(4).unwrap(), 0b0101);
        let fixture = fixture_for_identity(&[8], 4, 0b0101);
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();

        bar_write(&mut bar, &bus, base, VIRTIO_PCI_COMMON_DEVICE_STATUS, &[1]).unwrap();
        bar_write(&mut bar, &bus, base, VIRTIO_PCI_COMMON_DEVICE_STATUS, &[3]).unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT,
            &0_u32.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_DRIVER_FEATURE,
            &0b0101_u32.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT,
            &1_u32.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_DRIVER_FEATURE,
            &1_u32.to_le_bytes(),
        )
        .unwrap();
        bar_write(&mut bar, &bus, base, VIRTIO_PCI_COMMON_DEVICE_STATUS, &[11]).unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_QUEUE_SELECT,
            &0_u16.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_QUEUE_SIZE,
            &8_u16.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_QUEUE_DESC_LO,
            &0x1000_u32.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_QUEUE_AVAIL_LO,
            &0x2000_u32.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_QUEUE_USED_LO,
            &0x3000_u32.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_QUEUE_ENABLE,
            &1_u16.to_le_bytes(),
        )
        .unwrap();
        for (vector, message) in fixture.messages.iter().copied().enumerate() {
            let entry_offset = VIRTIO_PCI_MSIX_TABLE_OFFSET
                + u64::try_from(vector).expect("test vector should fit") * MSIX_TABLE_ENTRY_SIZE;
            bar_write(
                &mut bar,
                &bus,
                base,
                entry_offset,
                &message.address().to_le_bytes(),
            )
            .unwrap();
            bar_write(
                &mut bar,
                &bus,
                base,
                entry_offset + 8,
                &u64::from(message.data()).to_le_bytes(),
            )
            .unwrap();
        }
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_MSIX_CONFIG,
            &0_u16.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_QUEUE_MSIX_VECTOR,
            &1_u16.to_le_bytes(),
        )
        .unwrap();
        bar_write(&mut bar, &bus, base, VIRTIO_PCI_COMMON_DEVICE_STATUS, &[15]).unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_NOTIFICATION_OFFSET,
            &0_u16.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_DEVICE_FEATURE_SELECT,
            &0xfeed_beef_u32.to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_COMMON_DRIVER_FEATURE_SELECT,
            &0xdead_beef_u32.to_le_bytes(),
        )
        .unwrap();
        {
            let work = fixture.endpoint.admit_device_work().unwrap();
            work.with_core_mut(|core| {
                core.record_interrupt_intent(VirtioInterruptIntent::Configuration);
            })
            .unwrap();
        }
        let mut config = fixture.endpoint.config_function();
        config_write(&mut config, 4, &[0x07]);
        config_write(&mut config, 0x10, &[u8::MAX; 4]);

        assert!(
            fixture
                .signals
                .iter()
                .all(|signals| signals.lock().unwrap().is_empty()),
            "capturing retained state must not signal an interrupt"
        );
        RetainedTransportFixture {
            state: fixture.endpoint.transport_state().unwrap(),
            identity,
            bar_range: fixture.bar.range(),
            sbdf: PciSbdf::new(0, 0, 7, 0).unwrap(),
            region_id: MmioRegionId::new(107),
            messages: fixture.messages.clone(),
        }
    }

    fn prepare_retained_endpoint(
        fixture: &RetainedTransportFixture,
        state: &VirtioPciTransportState,
        messages: GuestMessageInterruptRegistry,
    ) -> Result<
        PreparedVirtioPciEndpoint<UnsupportedVirtioDeviceConfig, NoopVirtioDeviceActivation>,
        VirtioPciEndpointError,
    > {
        PreparedVirtioPciEndpoint::new(
            fixture.identity,
            &[8],
            UnsupportedVirtioDeviceConfig,
            NoopVirtioDeviceActivation,
            state.is_device_activated(),
            false,
            state,
            fixture.sbdf,
            fixture.bar_range,
            fixture.region_id,
            messages,
        )
    }

    fn fixture_with_reset_mode(
        mode: ResetMode,
    ) -> (
        VirtioPciEndpoint<UnsupportedVirtioDeviceConfig, ResetActivation>,
        PciBarLease,
        PciBarAllocator,
        Arc<Mutex<ResetStatistics>>,
    ) {
        let range = GuestMemoryRange::new(
            GuestAddress::new(0x4_0000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE * 2,
        )
        .expect("reset test BAR range should validate");
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, range);
        let bar = allocator
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("reset test BAR should allocate");
        let resources = registry_resources(2);
        let statistics = Arc::new(Mutex::new(ResetStatistics::default()));
        let endpoint = VirtioPciEndpoint::new(
            VirtioPciIdentity::new(VirtioDeviceType::new(4).unwrap(), 0),
            &[8],
            UnsupportedVirtioDeviceConfig,
            ResetActivation {
                mode,
                statistics: Arc::clone(&statistics),
            },
            false,
            &bar,
            resources.registry(),
        )
        .expect("reset test endpoint should initialize");
        (endpoint, bar, allocator, statistics)
    }

    fn config_read(config: &mut impl PciConfigFunction, offset: u16, len: usize) -> Vec<u8> {
        let mut data = vec![0; len];
        config
            .read_config(offset, &mut data)
            .expect("test config read should succeed");
        data
    }

    fn config_write(config: &mut impl PciConfigFunction, offset: u16, data: &[u8]) {
        config
            .write_config(offset, data)
            .expect("test config write should succeed");
    }

    fn bar_bus(fixture: &TestFixture) -> MmioBus {
        bar_bus_for_bar(&fixture.bar)
    }

    fn bar_bus_for_bar(bar: &PciBarLease) -> MmioBus {
        let mut bus = MmioBus::new();
        bus.insert(
            crate::mmio::MmioRegionId::new(77),
            bar.range().start(),
            bar.range().size(),
        )
        .expect("test BAR should register");
        bus
    }

    fn bar_read(
        handler: &mut impl MmioHandler,
        bus: &MmioBus,
        base: GuestAddress,
        offset: u64,
        len: usize,
    ) -> Vec<u8> {
        let address = base
            .checked_add(offset)
            .expect("test BAR read address should not overflow");
        let access = bus
            .lookup(
                address,
                u64::try_from(len).expect("test width should fit u64"),
            )
            .expect("test BAR read should resolve");
        handler
            .read(access)
            .expect("test BAR read should succeed")
            .as_slice()
            .to_vec()
    }

    fn bar_write(
        handler: &mut impl MmioHandler,
        bus: &MmioBus,
        base: GuestAddress,
        offset: u64,
        data: &[u8],
    ) -> Result<(), MmioHandlerError> {
        let address = base
            .checked_add(offset)
            .expect("test BAR write address should not overflow");
        let access = bus
            .lookup(
                address,
                u64::try_from(data.len()).expect("test width should fit u64"),
            )
            .expect("test BAR write should resolve");
        handler.write(
            access,
            MmioAccessBytes::new(data).expect("test BAR bytes should validate"),
        )
    }

    #[test]
    fn configuration_matches_pinned_identity_bar_and_capability_chain() {
        let fixture = fixture(&[8]);
        let mut config = fixture.endpoint.config_function();

        assert_eq!(
            u32::from_le_bytes(config_read(&mut config, 0, 4).try_into().unwrap()),
            0x1044_1af4
        );
        assert_eq!(
            u32::from_le_bytes(config_read(&mut config, 8, 4).try_into().unwrap()),
            0xffff_0001
        );
        assert_eq!(
            u32::from_le_bytes(config_read(&mut config, 0x2c, 4).try_into().unwrap()),
            0x1044_1af4
        );
        let bar_low = u32::from_le_bytes(config_read(&mut config, 0x10, 4).try_into().unwrap());
        let bar_high = u32::from_le_bytes(config_read(&mut config, 0x14, 4).try_into().unwrap());
        assert_eq!(bar_low & 0xf, 0x4);
        assert_eq!(
            (u64::from(bar_high) << 32) | u64::from(bar_low & !0xf),
            fixture.bar.range().start().raw_value()
        );

        assert_ne!(
            u16::from_le_bytes(config_read(&mut config, 0x06, 2).try_into().unwrap()) & 0x10,
            0
        );
        assert_eq!(config_read(&mut config, 0x34, 1), [0x40]);
        let expected = [
            (0x40, 0x09, 0x50, VIRTIO_PCI_CAP_COMMON),
            (0x50, 0x09, 0x60, VIRTIO_PCI_CAP_ISR),
            (0x60, 0x09, 0x70, VIRTIO_PCI_CAP_DEVICE),
            (0x70, 0x09, 0x84, VIRTIO_PCI_CAP_NOTIFY),
            (0x84, 0x09, 0x98, VIRTIO_PCI_CAP_PCI_CFG),
            (0x98, 0x11, 0x00, 0),
        ];
        for (offset, id, next, kind) in expected {
            assert_eq!(config_read(&mut config, offset, 1), [id]);
            assert_eq!(config_read(&mut config, offset + 1, 1), [next]);
            if id == 0x09 {
                assert_eq!(config_read(&mut config, offset + 3, 1), [kind]);
                assert_eq!(config_read(&mut config, offset + 4, 1), [0]);
            }
        }
        assert_eq!(config_read(&mut config, 0x42, 2), [16, 1]);
        assert_eq!(config_read(&mut config, 0x72, 2), [20, 2]);
        assert_eq!(config_read(&mut config, 0x80, 4), 4_u32.to_le_bytes());
        assert_eq!(config_read(&mut config, 0x9a, 2), 0x8001_u16.to_le_bytes());
        assert_eq!(
            config_read(&mut config, 0x9c, 4),
            (VIRTIO_PCI_MSIX_TABLE_OFFSET as u32).to_le_bytes()
        );
        assert_eq!(
            config_read(&mut config, 0xa0, 4),
            (VIRTIO_PCI_MSIX_PBA_OFFSET as u32).to_le_bytes()
        );
    }

    #[test]
    fn identity_matches_pinned_class_and_subclass_for_all_existing_device_types() {
        let cases = [
            (1, 0x1041_u16, PciClassCode::Network as u8, 0x00_u8),
            (2, 0x1042, PciClassCode::MassStorage as u8, 0x80),
            (4, 0x1044, PciClassCode::Unassigned as u8, 0xff),
            (5, 0x1045, PciClassCode::Unassigned as u8, 0xff),
            (19, 0x1053, PciClassCode::Unassigned as u8, 0xff),
            (24, 0x1058, PciClassCode::Unassigned as u8, 0xff),
            (27, 0x105b, PciClassCode::Unassigned as u8, 0xff),
        ];

        for (device_type, device_id, class, subclass) in cases {
            let fixture = fixture_for_device_type(&[8], device_type);
            let mut config = fixture.endpoint.config_function();
            assert_eq!(
                u16::from_le_bytes(config_read(&mut config, 2, 2).try_into().unwrap()),
                device_id
            );
            assert_eq!(config_read(&mut config, 0x0a, 2), [subclass, class]);
            assert_eq!(
                u32::from_le_bytes(config_read(&mut config, 0x2c, 4).try_into().unwrap()),
                (u32::from(device_id) << 16) | u32::from(VIRTIO_PCI_VENDOR_ID)
            );
        }
    }

    #[test]
    fn endpoint_preflight_rejects_invalid_queue_vector_bar_and_registry_shapes() {
        let range = GuestMemoryRange::new(
            GuestAddress::new(0x1_1000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE * 2,
        )
        .unwrap();
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, range);
        let bar = allocator.allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE).unwrap();
        let identity = VirtioPciIdentity::new(VirtioDeviceType::new(4).unwrap(), 0);

        assert!(matches!(
            VirtioPciEndpoint::new(
                identity,
                &[],
                UnsupportedVirtioDeviceConfig,
                NoopVirtioDeviceActivation,
                false,
                &bar,
                registry_resources(1).registry(),
            ),
            Err(VirtioPciEndpointError::QueueInitialization { .. })
        ));
        assert!(matches!(
            VirtioPciEndpoint::new(
                identity,
                &[8],
                UnsupportedVirtioDeviceConfig,
                NoopVirtioDeviceActivation,
                false,
                &bar,
                registry_resources(1).registry(),
            ),
            Err(VirtioPciEndpointError::MessageRouteCount {
                expected: 2,
                actual: 1
            })
        ));
        assert!(
            VirtioPciEndpoint::new(
                identity,
                &[8],
                UnsupportedVirtioDeviceConfig,
                NoopVirtioDeviceActivation,
                false,
                &bar,
                registry_resources(3).registry(),
            )
            .is_ok()
        );
        assert!(matches!(
            VirtioPciEndpoint::new(
                identity,
                &vec![1; VIRTIO_PCI_MAX_MSIX_VECTORS],
                UnsupportedVirtioDeviceConfig,
                NoopVirtioDeviceActivation,
                false,
                &bar,
                registry_resources(1).registry(),
            ),
            Err(VirtioPciEndpointError::TooManyVectors { vector_count })
                if vector_count == VIRTIO_PCI_MAX_MSIX_VECTORS + 1
        ));

        let small_bar = allocator
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE / 2)
            .unwrap();
        assert!(matches!(
            VirtioPciEndpoint::new(
                identity,
                &[8],
                UnsupportedVirtioDeviceConfig,
                NoopVirtioDeviceActivation,
                false,
                &small_bar,
                registry_resources(2).registry(),
            ),
            Err(VirtioPciEndpointError::CapabilityBarSize { .. })
        ));

        let range32 = GuestMemoryRange::new(
            GuestAddress::new(0x6000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE,
        )
        .unwrap();
        let mut allocator32 = PciBarAllocator::new(PciBarAddressSpace::Memory32, range32);
        let bar32 = allocator32
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .unwrap();
        assert!(matches!(
            VirtioPciEndpoint::new(
                identity,
                &[8],
                UnsupportedVirtioDeviceConfig,
                NoopVirtioDeviceActivation,
                false,
                &bar32,
                registry_resources(2).registry(),
            ),
            Err(VirtioPciEndpointError::CapabilityBarAddressSpace {
                actual: PciBarAddressSpace::Memory32
            })
        ));

        let released_registry = registry_resources(2).registry();
        released_registry.release().unwrap();
        assert!(matches!(
            VirtioPciEndpoint::new(
                identity,
                &[8],
                UnsupportedVirtioDeviceConfig,
                NoopVirtioDeviceActivation,
                false,
                &bar,
                released_registry,
            ),
            Err(VirtioPciEndpointError::MessageRegistry {
                source: GuestMessageInterruptRegistryError::NotActive {
                    phase: GuestMessageInterruptRegistryPhase::Released
                }
            })
        ));
    }

    #[test]
    fn common_config_negotiates_queues_activates_and_records_notifications() {
        let fixture = fixture(&[8]);
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();

        bar_write(&mut bar, &bus, base, 0x14, &[1]).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[3]).unwrap();
        bar_write(&mut bar, &bus, base, 0x08, &1_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x0c, &1_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[11]).unwrap();
        bar_write(&mut bar, &bus, base, 0x18, &8_u16.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x20, &0x1000_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x28, &0x2000_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x30, &0x3000_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x1c, &1_u16.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[15]).unwrap();

        assert_eq!(bar_read(&mut bar, &bus, base, 0x14, 1), [15]);
        assert_eq!(bar_read(&mut bar, &bus, base, 0x18, 2), 8_u16.to_le_bytes());
        assert_eq!(bar_read(&mut bar, &bus, base, 0x1c, 2), 1_u16.to_le_bytes());
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_NOTIFICATION_OFFSET,
            &0_u16.to_le_bytes(),
        )
        .unwrap();
        assert_eq!(fixture.endpoint.pending_queue_notifications().unwrap(), [0]);
        assert_eq!(
            fixture.endpoint.take_pending_queue_notifications().unwrap(),
            [0]
        );
        assert!(
            fixture
                .endpoint
                .pending_queue_notifications()
                .unwrap()
                .is_empty()
        );

        bar_write(&mut bar, &bus, base, 0x18, &4_u16.to_le_bytes())
            .expect("late queue configuration is a pinned no-op");
        assert_eq!(bar_read(&mut bar, &bus, base, 0x18, 2), 8_u16.to_le_bytes());

        bar_write(&mut bar, &bus, base, 0x14, &[0]).unwrap();
        assert_eq!(bar_read(&mut bar, &bus, base, 0x14, 1), [0]);
        assert_eq!(bar_read(&mut bar, &bus, base, 0x1c, 2), 0_u16.to_le_bytes());
        assert!(!fixture.endpoint.diagnostics().unwrap().device_activated);
        bar_write(&mut bar, &bus, base, 0x14, &[1]).unwrap();
        assert_eq!(bar_read(&mut bar, &bus, base, 0x14, 1), [1]);
    }

    #[test]
    fn attached_logger_observes_pci_transitions_without_changing_results() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut logger_state = LoggerState::default();
        logger_state.configure_test_writer(RecordingLogWriter(output.clone()));
        logger_state
            .configure(LoggerConfigInput::new().with_level(LoggerLevel::Debug))
            .expect("debug logger should configure");
        let logger = logger_state.guest_logger();

        let fixture = fixture(&[8]);
        fixture.endpoint.attach_guest_logger(logger.clone());
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();
        for status in [1_u8, 3, 11] {
            bar_write(&mut bar, &bus, base, 0x14, &[status]).expect("status setup should succeed");
        }
        bar_write(&mut bar, &bus, base, 0x18, &8_u16.to_le_bytes())
            .expect("queue size should configure");
        bar_write(&mut bar, &bus, base, 0x20, &0x1000_u32.to_le_bytes())
            .expect("descriptor address should configure");
        bar_write(&mut bar, &bus, base, 0x28, &0x2000_u32.to_le_bytes())
            .expect("available address should configure");
        bar_write(&mut bar, &bus, base, 0x30, &0x3000_u32.to_le_bytes())
            .expect("used address should configure");
        bar_write(&mut bar, &bus, base, 0x1c, &1_u16.to_le_bytes()).expect("queue should enable");
        bar_write(&mut bar, &bus, base, 0x14, &[15]).expect("device should activate");
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_NOTIFICATION_OFFSET,
            &0_u16.to_le_bytes(),
        )
        .expect("queue notification should succeed");
        bar_write(&mut bar, &bus, base, 0x14, &[0]).expect("device reset should succeed");
        assert!(!fixture.endpoint.diagnostics().unwrap().device_activated);

        let (failing, failing_bar, _allocator, statistics) =
            fixture_with_reset_mode(ResetMode::Failure);
        failing.attach_guest_logger(logger.clone());
        let failing_bus = bar_bus_for_bar(&failing_bar);
        let failing_base = failing_bar.range().start();
        let mut failing_handler = failing.bar_handler();
        for status in [1_u8, 3, 11, 15] {
            bar_write(
                &mut failing_handler,
                &failing_bus,
                failing_base,
                0x14,
                &[status],
            )
            .expect("failing fixture setup should succeed");
        }
        let error = bar_write(&mut failing_handler, &failing_bus, failing_base, 0x14, &[0])
            .expect_err("typed reset failure should remain visible");
        assert!(error.message().contains("injected reset failure"));
        assert_eq!(
            statistics
                .lock()
                .expect("reset statistics should be healthy")
                .reset_attempts,
            1
        );

        assert!(logger.wait_for_delivery_for_test());
        let log = String::from_utf8(
            output
                .lock()
                .expect("logger output lock should succeed")
                .clone(),
        )
        .expect("logger output should remain UTF-8");
        assert_eq!(
            log.matches("device-kind=entropy operation=device-activation outcome=succeeded\n")
                .count(),
            2
        );
        assert_eq!(
            log.matches("device-kind=entropy operation=queue-notification outcome=succeeded\n")
                .count(),
            1
        );
        assert_eq!(
            log.matches("device-kind=entropy operation=device-reset outcome=succeeded\n")
                .count(),
            1
        );
        assert_eq!(
            log.matches("device-kind=entropy operation=device-reset outcome=failed\n")
                .count(),
            1
        );
        assert!(!log.contains("injected reset failure"));
    }

    #[test]
    fn transport_capture_clones_complete_common_queue_interrupt_and_msix_state_redacted() {
        let fixture = fixture_for_identity(&[8], 4, 0b0101);
        let initial = fixture
            .endpoint
            .transport_state()
            .expect("initial PCI transport should capture");
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();

        bar_write(&mut bar, &bus, base, 0x14, &[1]).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[3]).unwrap();
        bar_write(&mut bar, &bus, base, 0x08, &0_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x0c, &0b0101_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[11]).unwrap();
        bar_write(&mut bar, &bus, base, 0x16, &0_u16.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x18, &8_u16.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x20, &0x1000_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x28, &0x2000_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x30, &0x3000_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x1c, &1_u16.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[15]).unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET,
            &0xdead_beef_u32.to_le_bytes(),
        )
        .expect("MSI-X table address should write");
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_NOTIFICATION_OFFSET,
            &0_u16.to_le_bytes(),
        )
        .expect("queue notification should write");
        {
            let work = fixture
                .endpoint
                .admit_device_work()
                .expect("device work should admit");
            work.with_core_mut(|core| {
                core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index: 0 });
            })
            .expect("interrupt intent should record");
        }

        let captured = fixture
            .endpoint
            .transport_state()
            .expect("programmed PCI transport should capture");
        assert_ne!(captured, initial);
        assert_eq!(captured.phase(), VirtioPciEndpointPhase::Active);
        assert_eq!(captured.driver_feature_select(), 0);
        assert_eq!(captured.queue_select(), 0);
        assert!(captured.is_device_activated());
        assert_eq!(captured.msix_vector_count(), 2);
        assert!(captured.msix_state().enabled());
        assert!(!captured.msix_state().function_masked());
        assert_eq!(captured.msix_state().config_vector(), VIRTIO_PCI_NO_VECTOR);
        assert_eq!(
            captured.msix_state().queue_vectors(),
            [VIRTIO_PCI_NO_VECTOR]
        );
        assert_eq!(captured.msix_state().pending_words(), [0]);
        assert!(!captured.msix_state().pending_transition_observed());
        assert_eq!(
            captured.msix_state().entries()[0].message_address_low(),
            0xdead_beef
        );
        assert_eq!(captured.msix_state().entries()[0].message_address_high(), 0);
        assert_eq!(captured.msix_state().entries()[0].message_data(), 0);
        assert_eq!(captured.msix_state().entries()[0].vector_control(), 1);
        assert_eq!(
            captured.interrupt_intents(),
            [VirtioInterruptIntent::Queue { queue_index: 0 }]
        );
        assert_eq!(
            captured.queue_notifications().pending_queue_notifications(),
            [0]
        );
        assert_eq!(captured.clone(), captured);
        let debug = format!("{captured:?}");
        assert_eq!(debug, "VirtioPciTransportState { state: \"<redacted>\" }");
        assert!(!debug.contains("dead_beef"));
        assert!(!debug.contains("3735928559"));
    }

    #[test]
    fn configuration_snapshot_errors_redact_private_detail() {
        let fixture = fixture_for_identity(&[8], 4, 0b0101);
        let state = fixture
            .endpoint
            .transport_state()
            .expect("test PCI transport should capture");
        let invalid_range = GuestMemoryRange::new(
            fixture.bar.range().start(),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE / 2,
        )
        .expect("test invalid-size BAR range should be structurally valid");
        let error = state
            .checked_configuration_guest_state(invalid_range)
            .expect_err("wrong BAR size should reject");
        assert_eq!(
            format!("{error:?}"),
            "VirtioPciConfigurationSnapshotError { kind: \"BarSize\", detail: \"<redacted>\" }"
        );
        assert!(!format!("{error:?}").contains("262144"));
        assert_eq!(
            error.to_string(),
            "virtio-pci snapshot capability BAR size is invalid"
        );
    }

    #[test]
    fn invalid_common_writes_are_noops_and_feature_ack_filters_unsupported_bits() {
        let fixture = fixture_for_identity(&[8], 4, 0b0101);
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();

        bar_write(&mut bar, &bus, base, 0x14, &[3])
            .expect("invalid status transition should be a no-op");
        assert_eq!(bar_read(&mut bar, &bus, base, 0x14, 1), [0]);

        bar_write(&mut bar, &bus, base, 0x14, &[1]).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[3]).unwrap();
        bar_write(&mut bar, &bus, base, 0x08, &0_u32.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x0c, &0b0111_u32.to_le_bytes()).unwrap();
        assert_eq!(
            fixture
                .endpoint
                .inner
                .state
                .lock()
                .unwrap()
                .core
                .device
                .driver_features(),
            0b0101
        );

        bar_write(&mut bar, &bus, base, 0x14, &[11]).unwrap();
        bar_write(&mut bar, &bus, base, 0x0c, &0_u32.to_le_bytes())
            .expect("late feature writes should be no-ops");
        assert_eq!(
            fixture
                .endpoint
                .inner
                .state
                .lock()
                .unwrap()
                .core
                .device
                .driver_features(),
            0b0101
        );
    }

    #[test]
    fn msix_routes_arbitrary_table_messages_and_delivers_pending_once_on_unmask() {
        let fixture = fixture(&[8]);
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();
        let message = fixture.messages[0];
        let entry_one = VIRTIO_PCI_MSIX_TABLE_OFFSET + MSIX_TABLE_ENTRY_SIZE;

        bar_write(
            &mut bar,
            &bus,
            base,
            entry_one,
            &message.address().to_le_bytes(),
        )
        .unwrap();
        let masked_data = (u64::from(1_u32) << 32) | u64::from(message.data());
        bar_write(
            &mut bar,
            &bus,
            base,
            entry_one + 8,
            &masked_data.to_le_bytes(),
        )
        .unwrap();
        bar_write(&mut bar, &bus, base, 0x1a, &1_u16.to_le_bytes()).unwrap();

        fixture
            .endpoint
            .trigger(VirtioInterruptIntent::Queue { queue_index: 0 })
            .unwrap();
        assert!(fixture.signals[0].lock().unwrap().is_empty());
        assert_eq!(
            u64::from_le_bytes(
                bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_PBA_OFFSET, 8)
                    .try_into()
                    .unwrap()
            ),
            2
        );

        bar_write(&mut bar, &bus, base, entry_one + 12, &0_u32.to_le_bytes()).unwrap();
        assert_eq!(fixture.signals[0].lock().unwrap().as_slice(), [message]);
        assert_eq!(
            u64::from_le_bytes(
                bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_PBA_OFFSET, 8)
                    .try_into()
                    .unwrap()
            ),
            0
        );
        bar_write(&mut bar, &bus, base, entry_one + 12, &0_u32.to_le_bytes()).unwrap();
        assert_eq!(fixture.signals[0].lock().unwrap().len(), 1);

        // A second table entry may legally resolve to the same live route.
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET,
            &message.address().to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET + 8,
            &u64::from(message.data()).to_le_bytes(),
        )
        .unwrap();
        bar_write(&mut bar, &bus, base, 0x10, &0_u16.to_le_bytes()).unwrap();
        fixture
            .endpoint
            .trigger(VirtioInterruptIntent::Configuration)
            .unwrap();
        assert_eq!(fixture.signals[0].lock().unwrap().len(), 2);
    }

    #[test]
    fn vsock_pci_capture_is_exact_for_inactive_and_masked_reset_state() {
        let fixture = vsock_fixture();
        let config = vsock_pci_config();
        let mut memory = vsock_event_memory();

        let (inactive_first, inactive_validation) = fixture
            .endpoint
            .capture_vsock_state(&config, &memory, VirtioVsockTransportResetAttempt::Inactive)
            .expect("inactive PCI vsock should capture under one endpoint lock");
        let (inactive_second, repeated_validation) = fixture
            .endpoint
            .capture_vsock_state(&config, &memory, VirtioVsockTransportResetAttempt::Inactive)
            .expect("repeat inactive PCI capture should be exact");
        assert_eq!(inactive_first, inactive_second);
        assert_eq!(inactive_validation, repeated_validation);
        assert!(!inactive_first.device().is_activated());
        assert!(!inactive_first.transport().is_device_activated());
        assert_eq!(
            inactive_first.device().backend_selector().path(),
            std::path::Path::new(TEST_VSOCK_PCI_UDS_PATH)
        );
        assert_eq!(
            format!("{inactive_first:?}"),
            "VirtioVsockPciCaptureState { state: \"<redacted>\" }"
        );
        assert!(!format!("{inactive_first:?}").contains(TEST_VSOCK_PCI_UDS_PATH));

        let event_message = fixture.messages[2];
        configure_vsock_endpoint(&fixture, event_message, true);
        publish_vsock_event_descriptor(&mut memory, 8);
        let reset_attempt = fixture
            .endpoint
            .prepare_vsock_transport_reset(&mut memory, &SharedVsockDeviceMetrics::default())
            .expect("masked PCI reset should publish before capture");
        let (active_first, active_validation) = fixture
            .endpoint
            .capture_vsock_state(&config, &memory, reset_attempt)
            .expect("active PCI vsock should capture device and transport atomically");
        let (active_second, repeated_validation) = fixture
            .endpoint
            .capture_vsock_state(&config, &memory, reset_attempt)
            .expect("repeat active PCI capture should be exact");

        assert_eq!(active_first, active_second);
        assert_eq!(active_validation, repeated_validation);
        assert_eq!(active_validation.reset_attempt(), reset_attempt);
        assert!(!active_validation.source_work().dropped_any_source_work());
        assert!(active_first.device().is_activated());
        assert!(active_first.transport().is_device_activated());
        let queues = active_first
            .device()
            .active_queues()
            .expect("active PCI capture should retain all queue cursors");
        assert_eq!(
            (queues.rx().next_available(), queues.rx().next_used()),
            (0, 0)
        );
        assert_eq!(
            (queues.tx().next_available(), queues.tx().next_used()),
            (0, 0)
        );
        assert_eq!(
            (queues.event().next_available(), queues.event().next_used()),
            (1, 1)
        );
        assert!(queues.rx().event_idx_enabled());
        assert!(queues.tx().event_idx_enabled());
        assert!(queues.event().event_idx_enabled());
        assert_ne!(
            active_first.transport().msix_state().pending_words()[0] & (1_u64 << 2),
            0,
            "masked reset interrupt must remain in canonical PCI state"
        );
        assert!(fixture.signals[2].lock().unwrap().is_empty());
        assert_eq!(read_vsock_event_used_index(&memory), 1);
        let (normalized, normalized_validation) = fixture
            .endpoint
            .capture_and_normalize_vsock_state(&config, &memory, reset_attempt)
            .expect("PCI capture should normalize source work under the endpoint lock");
        assert_eq!(normalized, active_first);
        assert_eq!(normalized_validation, active_validation);

        let listener_path = std::path::Path::new("/tmp").join(format!(
            "bb-pci-{}-{:x}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system time should be after the Unix epoch")
                .as_nanos()
        ));
        let listener =
            UnixListener::bind(&listener_path).expect("PCI reconstruction listener should bind");
        fs::remove_file(&listener_path)
            .expect("PCI reconstruction listener path should unlink after bind");
        let destination_path = std::path::Path::new("/tmp")
            .join(format!("bb-pci-destination-{}.sock", std::process::id()));
        let destination_selector = VsockBackendSelector::try_from_path(&destination_path)
            .expect("PCI destination selector should validate");
        let mut resource = VirtioVsockReconstructionResource::with_destination_selector(
            active_first.device().backend_selector().clone(),
            destination_selector,
            SuppliedVsockListener::new(listener).with_guest_connector(RejectingVsockGuestConnector),
        );
        let reconstructed = active_first
            .reconstruct_snapshot_device(&memory, &mut resource)
            .expect("PCI capture should rebuild device components without placement");
        assert!(resource.is_consumed());
        assert_eq!(reconstructed.guest_cid(), 42);
        assert_eq!(reconstructed.uds_path(), destination_path);
        assert!(reconstructed.device().is_activated());
        assert!(reconstructed.device().pending_event_ack());
        assert_eq!(
            reconstructed.device().host_local_port_cursor(),
            active_first.device().host_local_port_cursor()
        );
    }

    #[test]
    fn vsock_transport_reset_routes_event_queue_msix_and_preserves_masked_pending() {
        let fixture = vsock_fixture();
        let event_message = fixture.messages[2];
        configure_vsock_endpoint(&fixture, event_message, true);
        let mut memory = vsock_event_memory();
        let metrics = SharedVsockDeviceMetrics::default();
        publish_vsock_event_descriptor(&mut memory, 8);

        let attempt = fixture
            .endpoint
            .prepare_vsock_transport_reset(&mut memory, &metrics)
            .expect("masked transport reset should complete");

        assert!(matches!(
            attempt,
            VirtioVsockTransportResetAttempt::Published(_)
        ));
        assert_eq!(read_vsock_event_used_index(&memory), 1);
        assert!(
            fixture
                .endpoint
                .inner
                .state
                .lock()
                .expect("endpoint state should be healthy")
                .core
                .activation
                .pending_event_ack()
        );
        assert!(fixture.signals[2].lock().unwrap().is_empty());
        let bus = bar_bus_for_bar(&fixture.bar);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();
        assert_eq!(
            u64::from_le_bytes(
                bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_PBA_OFFSET, 8)
                    .try_into()
                    .expect("PBA word should fit")
            ),
            1_u64 << 2
        );

        let event_entry = VIRTIO_PCI_MSIX_TABLE_OFFSET + 2 * MSIX_TABLE_ENTRY_SIZE;
        bar_write(&mut bar, &bus, base, event_entry + 12, &0_u32.to_le_bytes())
            .expect("event vector should unmask");

        assert_eq!(
            fixture.signals[2]
                .lock()
                .expect("event signal list should be healthy")
                .as_slice(),
            [event_message]
        );
        assert_eq!(metrics.snapshot().ev_queue_event_fails(), 0);
    }

    #[test]
    fn vsock_restored_signal_routes_event_queue_msix_without_queue_mutation() {
        let fixture = vsock_fixture();
        let event_message = fixture.messages[2];
        configure_vsock_endpoint(&fixture, event_message, false);
        let memory = vsock_event_memory();
        let metrics = SharedVsockDeviceMetrics::default();

        let signal = fixture
            .endpoint
            .signal_restored_vsock_transport_reset(&metrics)
            .expect("restored reset should signal");

        assert_eq!(signal, VirtioVsockRestoredTransportResetSignal::Signaled);
        assert_eq!(read_vsock_event_used_index(&memory), 0);
        assert!(
            fixture
                .endpoint
                .inner
                .state
                .lock()
                .expect("endpoint state should be healthy")
                .core
                .activation
                .pending_event_ack()
        );
        assert_eq!(
            fixture.signals[2]
                .lock()
                .expect("event signal list should be healthy")
                .as_slice(),
            [event_message]
        );
        assert_eq!(metrics.snapshot().ev_queue_event_fails(), 0);
    }

    #[test]
    fn vsock_transport_reset_preserves_publication_when_msix_delivery_fails() {
        let fixture = vsock_fixture();
        let unknown_message = GuestMessage::new(0xdead_beef, 0x1234);
        configure_vsock_endpoint(&fixture, unknown_message, false);
        let mut memory = vsock_event_memory();
        let metrics = SharedVsockDeviceMetrics::default();
        publish_vsock_event_descriptor(&mut memory, 4);

        let error = fixture
            .endpoint
            .prepare_vsock_transport_reset(&mut memory, &metrics)
            .expect_err("unknown MSI-X tuple should fail after reset publication");

        assert!(matches!(
            error.completed_device_operation(),
            Some(VirtioVsockTransportResetAttempt::Published(_))
        ));
        assert!(matches!(
            error.endpoint_error(),
            Some(VirtioPciEndpointError::MessageRegistry {
                source: GuestMessageInterruptRegistryError::UnknownMessage
            })
        ));
        assert_eq!(read_vsock_event_used_index(&memory), 1);
        assert!(
            fixture
                .endpoint
                .inner
                .state
                .lock()
                .expect("endpoint state should be healthy")
                .core
                .activation
                .pending_event_ack()
        );
        assert_eq!(metrics.snapshot().ev_queue_event_fails(), 1);
        assert!(!format!("{error:?}").contains("deadbeef"));
    }

    #[test]
    fn msix_function_mask_and_disable_retain_pending_delivery_until_reenabled() {
        let fixture = fixture(&[8]);
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();
        let mut config = fixture.endpoint.config_function();
        let message = fixture.messages[0];
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET,
            &message.address().to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET + 8,
            &u64::from(message.data()).to_le_bytes(),
        )
        .unwrap();
        bar_write(&mut bar, &bus, base, 0x10, &0_u16.to_le_bytes()).unwrap();

        config_write(&mut config, 0x9a, &0xc001_u16.to_le_bytes());
        fixture
            .endpoint
            .trigger(VirtioInterruptIntent::Configuration)
            .unwrap();
        assert!(fixture.signals[0].lock().unwrap().is_empty());
        assert_eq!(
            u64::from_le_bytes(
                bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_PBA_OFFSET, 8)
                    .try_into()
                    .unwrap()
            ),
            1
        );

        config_write(&mut config, 0x9a, &0x8001_u16.to_le_bytes());
        assert_eq!(fixture.signals[0].lock().unwrap().as_slice(), [message]);
        assert_eq!(
            u64::from_le_bytes(
                bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_PBA_OFFSET, 8)
                    .try_into()
                    .unwrap()
            ),
            0
        );

        config_write(&mut config, 0x9a, &0x0001_u16.to_le_bytes());
        fixture
            .endpoint
            .trigger(VirtioInterruptIntent::Configuration)
            .unwrap();
        assert_eq!(fixture.signals[0].lock().unwrap().len(), 1);
        assert_eq!(
            u64::from_le_bytes(
                bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_PBA_OFFSET, 8)
                    .try_into()
                    .unwrap()
            ),
            1
        );
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_PBA_OFFSET,
            &u64::MAX.to_le_bytes(),
        )
        .expect("PBA writes should be ignored");
        assert_eq!(
            u64::from_le_bytes(
                bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_PBA_OFFSET, 8)
                    .try_into()
                    .unwrap()
            ),
            1
        );

        config_write(&mut config, 0x9a, &0x8001_u16.to_le_bytes());
        assert_eq!(fixture.signals[0].lock().unwrap().as_slice(), [message; 2]);
        assert_eq!(
            u64::from_le_bytes(
                bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_PBA_OFFSET, 8)
                    .try_into()
                    .unwrap()
            ),
            0
        );
    }

    #[test]
    fn malformed_msix_table_accesses_match_pinned_noop_and_all_ones_behavior() {
        let fixture = fixture(&[8]);
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();
        let before_low = bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_TABLE_OFFSET, 8);
        let before_high = bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_TABLE_OFFSET + 8, 8);

        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET + 2,
            &[0xaa, 0xbb],
        )
        .expect("invalid-width table write should be a no-op");
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET + 2,
            &0x1122_3344_u32.to_le_bytes(),
        )
        .expect("unaligned table write should be a no-op");
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET + 2 * MSIX_TABLE_ENTRY_SIZE,
            &0x5566_7788_u32.to_le_bytes(),
        )
        .expect("out-of-range table entry write should be a no-op");

        assert_eq!(
            bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_TABLE_OFFSET, 8,),
            before_low
        );
        assert_eq!(
            bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_TABLE_OFFSET + 8, 8,),
            before_high
        );
        assert_eq!(
            bar_read(&mut bar, &bus, base, VIRTIO_PCI_MSIX_TABLE_OFFSET + 2, 4,),
            [u8::MAX; 4]
        );
    }

    #[test]
    fn unsupported_reset_keeps_backend_active_and_blocks_reinitialization() {
        let (endpoint, bar_lease, _allocator, statistics) =
            fixture_with_reset_mode(ResetMode::Unsupported);
        let bus = bar_bus_for_bar(&bar_lease);
        let base = bar_lease.range().start();
        let mut bar = endpoint.bar_handler();

        bar_write(&mut bar, &bus, base, 0x14, &[1]).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[3]).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[11]).unwrap();
        bar_write(&mut bar, &bus, base, 0x18, &8_u16.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x1c, &1_u16.to_le_bytes()).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[15]).unwrap();
        assert!(endpoint.diagnostics().unwrap().device_activated);

        bar_write(&mut bar, &bus, base, 0x14, &[0]).unwrap();
        assert_eq!(bar_read(&mut bar, &bus, base, 0x14, 1), [0]);
        assert_eq!(bar_read(&mut bar, &bus, base, 0x1c, 2), 1_u16.to_le_bytes());
        assert!(endpoint.diagnostics().unwrap().device_activated);
        bar_write(&mut bar, &bus, base, 0x14, &[1])
            .expect("reinitialization against an active backend should be a no-op");
        assert_eq!(bar_read(&mut bar, &bus, base, 0x14, 1), [0]);
        bar_write(
            &mut bar,
            &bus,
            base,
            0x14,
            &[VIRTIO_DEVICE_STATUS_FAILED as u8],
        )
        .expect("the guest may still mark an unresettable device failed");
        assert_eq!(
            bar_read(&mut bar, &bus, base, 0x14, 1),
            [VIRTIO_DEVICE_STATUS_FAILED as u8]
        );

        let statistics = statistics.lock().unwrap();
        assert_eq!(statistics.activations, 1);
        assert_eq!(statistics.reset_attempts, 1);
    }

    #[test]
    fn reset_failure_is_typed_while_status_zero_still_blocks_reinitialization() {
        let (endpoint, bar_lease, _allocator, statistics) =
            fixture_with_reset_mode(ResetMode::Failure);
        let bus = bar_bus_for_bar(&bar_lease);
        let base = bar_lease.range().start();
        let mut bar = endpoint.bar_handler();

        bar_write(&mut bar, &bus, base, 0x14, &[1]).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[3]).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[11]).unwrap();
        bar_write(&mut bar, &bus, base, 0x14, &[15]).unwrap();
        let error = bar_write(&mut bar, &bus, base, 0x14, &[0])
            .expect_err("injected backend reset failure should be typed");
        assert!(error.to_string().contains("virtio-pci device reset failed"));
        assert_eq!(bar_read(&mut bar, &bus, base, 0x14, 1), [0]);
        assert!(endpoint.diagnostics().unwrap().device_activated);
        bar_write(&mut bar, &bus, base, 0x14, &[1])
            .expect("reinitialization after reset failure should be a no-op");
        assert_eq!(bar_read(&mut bar, &bus, base, 0x14, 1), [0]);

        let statistics = statistics.lock().unwrap();
        assert_eq!(statistics.activations, 1);
        assert_eq!(statistics.reset_attempts, 1);
    }

    #[test]
    fn pci_cfg_capability_proxies_bar_and_release_rejects_stale_handles() {
        let fixture = fixture(&[8]);
        let mut config = fixture.endpoint.config_function();
        config_write(&mut config, 0x88, &[0]);
        config_write(&mut config, 0x8c, &0x14_u32.to_le_bytes());
        config_write(&mut config, 0x90, &1_u32.to_le_bytes());
        assert_eq!(config_read(&mut config, 0x94, 4), [0, 0, 0, 0]);
        config_write(&mut config, 0x94, &[1, 0, 0, 0]);
        assert_eq!(config_read(&mut config, 0x94, 4), [1, 0, 0, 0]);

        fixture.endpoint.release().unwrap();
        assert_eq!(
            fixture.endpoint.phase().unwrap(),
            VirtioPciEndpointPhase::Released
        );
        let error = config
            .read_config(0, &mut [0; 4])
            .expect_err("released config handle must fail");
        assert!(error.to_string().contains("not active"));
        assert!(matches!(
            fixture
                .endpoint
                .trigger(VirtioInterruptIntent::Configuration),
            Err(VirtioPciEndpointError::NotActive {
                phase: VirtioPciEndpointPhase::Released
            })
        ));
    }

    #[test]
    fn admitted_core_operation_mutates_only_the_canonical_endpoint_state() {
        let fixture = fixture(&[8]);
        let work = fixture
            .endpoint
            .admit_device_work()
            .expect("active endpoint should admit canonical device work");

        let intents = work
            .with_core_mut(|core| {
                assert!(core.take_interrupt_intents().is_empty());
                core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index: 0 });
                core.take_interrupt_intents()
            })
            .expect("admitted canonical device work should succeed");

        assert_eq!(
            intents,
            vec![VirtioInterruptIntent::Queue { queue_index: 0 }]
        );
        assert!(
            fixture
                .endpoint
                .admit_device_work()
                .expect("a second work transaction should observe the same core")
                .with_core_mut(|core| core.take_interrupt_intents())
                .expect("canonical state should remain accessible")
                .is_empty()
        );
    }

    #[test]
    fn device_operation_error_preserves_device_and_endpoint_failures() {
        assert_eq!(
            VirtioPciDeviceOperationError::<&str, i32>::combine(Ok(42), Ok(()))
                .expect("two successful phases should preserve the value"),
            42
        );

        let device = VirtioPciDeviceOperationError::<&str, ()>::combine(Err("device"), Ok(()))
            .expect_err("device failure should be retained");
        assert_eq!(device.device_error(), Some(&"device"));
        assert!(device.completed_device_operation().is_none());
        assert!(device.endpoint_error().is_none());

        let endpoint = VirtioPciDeviceOperationError::<&str, i32>::combine(
            Ok(42),
            Err(VirtioPciEndpointError::StatePoisoned),
        )
        .expect_err("endpoint failure should be retained");
        assert!(endpoint.device_error().is_none());
        assert_eq!(endpoint.completed_device_operation(), Some(&42));
        assert!(matches!(
            endpoint.endpoint_error(),
            Some(VirtioPciEndpointError::StatePoisoned)
        ));

        let combined = VirtioPciDeviceOperationError::<&str, ()>::combine(
            Err("device"),
            Err(VirtioPciEndpointError::StatePoisoned),
        )
        .expect_err("both failures should be retained");
        assert_eq!(combined.device_error(), Some(&"device"));
        assert!(combined.completed_device_operation().is_none());
        assert!(matches!(
            combined.endpoint_error(),
            Some(VirtioPciEndpointError::StatePoisoned)
        ));
    }

    #[test]
    fn release_closes_new_admission_and_drains_an_admitted_interrupt() {
        let fixture = fixture(&[8]);
        let work = fixture
            .endpoint
            .admit_device_work()
            .expect("active endpoint should admit device work");
        let release_endpoint = fixture.endpoint.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();

        thread::scope(|scope| {
            scope.spawn(move || {
                started_tx
                    .send(())
                    .expect("release-start signal should be received");
                let result = release_endpoint.release();
                done_tx
                    .send(result)
                    .expect("release result should be received");
            });
            started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("release thread should start");

            let admission_closed = (0..10_000).any(|_| {
                let gate = fixture
                    .endpoint
                    .inner
                    .state
                    .lock()
                    .expect("endpoint state should be healthy")
                    .core
                    .work_gate()
                    .clone();
                let closed = !gate
                    .is_accepting()
                    .expect("work gate should remain healthy");
                if !closed {
                    thread::yield_now();
                }
                closed
            });
            assert!(admission_closed, "release must close work admission");
            assert!(matches!(done_rx.try_recv(), Err(TryRecvError::Empty)));

            let error = fixture
                .endpoint
                .trigger(VirtioInterruptIntent::Configuration)
                .expect_err("new interrupt work must be rejected while draining");
            assert!(matches!(
                error,
                VirtioPciEndpointError::WorkGate {
                    source: crate::virtio::VirtioDeviceWorkGateError::Quiescing
                }
            ));
            work.trigger(VirtioInterruptIntent::Configuration)
                .expect("already admitted interrupt work should finish");
            drop(work);

            done_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("release must finish after admitted work drains")
                .expect("release should succeed");
        });
        assert_eq!(
            fixture.endpoint.phase().unwrap(),
            VirtioPciEndpointPhase::Released
        );
    }

    #[test]
    fn invalid_vector_is_coerced_to_no_vector_and_unknown_tuple_is_rejected() {
        let fixture = fixture(&[8]);
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();

        bar_write(&mut bar, &bus, base, 0x1a, &9_u16.to_le_bytes()).unwrap();
        assert_eq!(
            bar_read(&mut bar, &bus, base, 0x1a, 2),
            u16::MAX.to_le_bytes()
        );

        let unknown = GuestMessage::new(0xdead_beef, 0x1234);
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET,
            &unknown.address().to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET + 8,
            &u64::from(unknown.data()).to_le_bytes(),
        )
        .unwrap();
        bar_write(&mut bar, &bus, base, 0x10, &0_u16.to_le_bytes()).unwrap();
        let error = fixture
            .endpoint
            .trigger(VirtioInterruptIntent::Configuration)
            .expect_err("unknown guest tuple must be rejected");
        assert!(matches!(
            error,
            VirtioPciEndpointError::MessageRegistry {
                source: GuestMessageInterruptRegistryError::UnknownMessage
            }
        ));
        assert!(!format!("{error:?}").contains("deadbeef"));
    }

    #[test]
    fn draining_interrupt_intents_preserves_valid_delivery_before_a_later_error() {
        let fixture = fixture(&[8]);
        let bus = bar_bus(&fixture);
        let base = fixture.bar.range().start();
        let mut bar = fixture.endpoint.bar_handler();
        let message = fixture.messages[0];
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET,
            &message.address().to_le_bytes(),
        )
        .unwrap();
        bar_write(
            &mut bar,
            &bus,
            base,
            VIRTIO_PCI_MSIX_TABLE_OFFSET + 8,
            &u64::from(message.data()).to_le_bytes(),
        )
        .unwrap();
        bar_write(&mut bar, &bus, base, 0x10, &0_u16.to_le_bytes()).unwrap();
        let mut config = fixture.endpoint.config_function();
        config_write(&mut config, 0x9a, &0x8001_u16.to_le_bytes());
        {
            let mut state = fixture.endpoint.inner.state.lock().unwrap();
            state
                .core
                .record_interrupt_intent(VirtioInterruptIntent::Configuration);
            state
                .core
                .record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index: 9 });
        }

        let error = fixture
            .endpoint
            .drain_interrupt_intents()
            .expect_err("invalid queue intent should remain a typed error");
        assert!(matches!(
            error,
            VirtioPciEndpointError::InvalidQueueIndex {
                queue_index: 9,
                queue_count: 1
            }
        ));
        assert_eq!(fixture.signals[0].lock().unwrap().as_slice(), [message]);
        fixture
            .endpoint
            .drain_interrupt_intents()
            .expect("drained intents should not repeat");
        assert_eq!(fixture.signals[0].lock().unwrap().len(), 1);
    }

    #[test]
    fn retained_endpoint_restores_exact_state_and_publication_leases() {
        let fixture = retained_transport_fixture();
        let CountingResourcesFixture {
            registry,
            resources,
            signals,
            releases,
        } = counting_resources_for_messages(&fixture.messages);
        let prepared = prepare_retained_endpoint(&fixture, &fixture.state, registry)
            .expect("valid retained endpoint should prepare");
        assert_eq!(prepared.sbdf(), fixture.sbdf);
        assert_eq!(prepared.bar_range(), fixture.bar_range);
        assert_eq!(prepared.region_id(), fixture.region_id);

        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, fixture.bar_range);
        let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
        let mut dispatcher = MmioDispatcher::new();
        let mut published = prepared
            .publish(&mut allocator, segment.clone(), &mut dispatcher, resources)
            .expect("exact retained endpoint publication should succeed");

        assert_eq!(published.sbdf(), Some(fixture.sbdf));
        assert_eq!(published.bar_range(), Some(fixture.bar_range));
        assert_eq!(
            published.endpoint().transport_state().unwrap(),
            fixture.state
        );
        assert!(matches!(
            allocator.validate_exact_reservation(fixture.bar_range),
            Err(PciBarAllocationError::ExactRangeOccupied)
        ));
        assert!(matches!(
            segment
                .with_segment(|segment| segment.validate_function_at(fixture.sbdf))
                .unwrap(),
            Err(PciSegmentError::DuplicateIdentity { sbdf }) if sbdf == fixture.sbdf
        ));
        assert!(dispatcher.lookup(fixture.bar_range.start(), 1).is_ok());
        assert!(
            signals
                .iter()
                .all(|signals| signals.lock().unwrap().is_empty()),
            "retained route validation and publication must not signal"
        );

        published
            .prepare_teardown(&mut dispatcher)
            .expect("retained teardown should prepare");
        assert_eq!(
            published.endpoint().phase().unwrap(),
            VirtioPciEndpointPhase::Quiescing
        );
        published
            .rollback_prepared_teardown(&mut dispatcher)
            .expect("retained teardown should roll back");
        assert_eq!(
            published.endpoint().transport_state().unwrap(),
            fixture.state
        );

        published
            .teardown(&mut dispatcher, &mut allocator)
            .expect("retained endpoint should tear down");
        assert_eq!(*releases.lock().unwrap(), 1);
        assert!(dispatcher.regions().is_empty());
        assert_eq!(
            segment
                .with_segment(|segment| segment.function_count())
                .unwrap(),
            1
        );
        let reused = allocator
            .reserve_exact(fixture.bar_range)
            .expect("exact retained BAR should be immediately reusable");
        allocator.release(&reused).unwrap();
    }

    #[test]
    fn retained_endpoint_rejects_every_hostile_state_family_before_publication() {
        let fixture = retained_transport_fixture();
        let assert_kind = |state: VirtioPciTransportState, expected| {
            let (registry, signals) = registry_for_messages(&fixture.messages);
            let observer = registry.clone();
            let error = match prepare_retained_endpoint(&fixture, &state, registry) {
                Ok(_) => panic!("hostile retained state should be rejected"),
                Err(error) => error,
            };
            assert!(matches!(
                error,
                VirtioPciEndpointError::RetainedState { kind } if kind == expected
            ));
            assert_eq!(
                observer.phase().unwrap(),
                GuestMessageInterruptRegistryPhase::Active
            );
            assert!(
                signals
                    .iter()
                    .all(|signals| signals.lock().unwrap().is_empty())
            );
        };

        let mut state = fixture.state.clone();
        state.phase = VirtioPciEndpointPhase::Quiescing;
        assert_kind(state, VirtioPciRetainedStateError::Phase);

        let mut state = fixture.state.clone();
        let device = state.device;
        state.device = VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(
            5,
            device.vendor_id(),
            device.device_features(),
            device.config_generation(),
        )
        .with_runtime_state(
            [
                device.device_features_select(),
                device.driver_features_select(),
            ],
            device.driver_features(),
            device.status(),
        );
        assert_kind(state, VirtioPciRetainedStateError::DeviceIdentity);

        let mut state = fixture.state.clone();
        let device = state.device;
        state.device = VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(
            device.device_id(),
            device.vendor_id(),
            device.device_features(),
            device.config_generation(),
        )
        .with_runtime_state([2, 0], device.driver_features(), device.status());
        assert_kind(state, VirtioPciRetainedStateError::Features);

        let mut state = fixture.state.clone();
        let device = state.device;
        state.device = VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(
            device.device_id(),
            device.vendor_id(),
            device.device_features(),
            device.config_generation(),
        )
        .with_runtime_state([0, 0], 0, 0x20);
        assert_kind(state, VirtioPciRetainedStateError::Status);

        let mut state = fixture.state.clone();
        state.device_activated = false;
        assert_kind(state, VirtioPciRetainedStateError::Activation);

        let mut state = fixture.state.clone();
        state.requires_device_config_write_status = true;
        assert_kind(state, VirtioPciRetainedStateError::DeviceConfigPolicy);

        let mut state = fixture.state.clone();
        state.queues = VirtioQueues::new(&[16]).unwrap();
        assert_kind(state, VirtioPciRetainedStateError::Queue);

        let mut state = fixture.state.clone();
        state.queue_notifications = VirtioQueueNotificationState::new(2).unwrap();
        assert_kind(state, VirtioPciRetainedStateError::Notification);

        let mut state = fixture.state.clone();
        state
            .interrupt_intents
            .push(VirtioInterruptIntent::Configuration);
        assert_kind(state, VirtioPciRetainedStateError::InterruptIntent);

        let mut state = fixture.state.clone();
        state.pci_cfg_cap_offset += 1;
        assert_kind(state, VirtioPciRetainedStateError::Configuration);

        let mut state = fixture.state.clone();
        state.msix.entries.pop();
        assert_kind(state, VirtioPciRetainedStateError::Msix);

        let invalid_sbdf = PciSbdf::new(0, 1, 7, 0).unwrap();
        let (registry, signals) = registry_for_messages(&fixture.messages);
        let observer = registry.clone();
        let error = match PreparedVirtioPciEndpoint::new(
            fixture.identity,
            &[8],
            UnsupportedVirtioDeviceConfig,
            NoopVirtioDeviceActivation,
            true,
            false,
            &fixture.state,
            invalid_sbdf,
            fixture.bar_range,
            fixture.region_id,
            registry,
        ) {
            Ok(_) => panic!("invalid retained SBDF should reject"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            VirtioPciEndpointError::RetainedState {
                kind: VirtioPciRetainedStateError::Sbdf
            }
        ));
        assert_eq!(
            observer.phase().unwrap(),
            GuestMessageInterruptRegistryPhase::Active
        );
        assert!(
            signals
                .iter()
                .all(|signals| signals.lock().unwrap().is_empty())
        );

        let invalid_bar = GuestMemoryRange::new(
            fixture.bar_range.start(),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE / 2,
        )
        .unwrap();
        let (registry, signals) = registry_for_messages(&fixture.messages);
        let observer = registry.clone();
        let error = match PreparedVirtioPciEndpoint::new(
            fixture.identity,
            &[8],
            UnsupportedVirtioDeviceConfig,
            NoopVirtioDeviceActivation,
            true,
            false,
            &fixture.state,
            fixture.sbdf,
            invalid_bar,
            fixture.region_id,
            registry,
        ) {
            Ok(_) => panic!("invalid retained BAR should reject"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            VirtioPciEndpointError::RetainedState {
                kind: VirtioPciRetainedStateError::Bar
            }
        ));
        assert_eq!(
            observer.phase().unwrap(),
            GuestMessageInterruptRegistryPhase::Active
        );
        assert!(
            signals
                .iter()
                .all(|signals| signals.lock().unwrap().is_empty())
        );
    }

    #[test]
    fn retained_endpoint_validates_exact_routes_without_signaling() {
        let fixture = retained_transport_fixture();
        let unknown_messages = [
            GuestMessage::new(0x0800_0080, 190),
            GuestMessage::new(0x0800_0080, 191),
        ];
        let (unknown_registry, unknown_signals) = registry_for_messages(&unknown_messages);
        let unknown = match prepare_retained_endpoint(&fixture, &fixture.state, unknown_registry) {
            Ok(_) => panic!("unknown retained route should reject"),
            Err(error) => error,
        };
        assert!(matches!(
            unknown,
            VirtioPciEndpointError::MessageRegistry {
                source: GuestMessageInterruptRegistryError::UnknownMessage
            }
        ));
        assert!(
            unknown_signals
                .iter()
                .all(|signals| signals.lock().unwrap().is_empty())
        );

        let duplicate_signals = [
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
        ];
        let duplicate_routes = duplicate_signals
            .iter()
            .map(|signals| {
                let route: Arc<dyn GuestMessageInterrupt> = Arc::new(RecordingRoute {
                    message: fixture.messages[0],
                    signals: Arc::clone(signals),
                });
                route
            })
            .collect();
        let ambiguous_registry = GuestMessageInterruptRegistry::new(duplicate_routes).unwrap();
        let ambiguous =
            match prepare_retained_endpoint(&fixture, &fixture.state, ambiguous_registry) {
                Ok(_) => panic!("ambiguous retained route should reject"),
                Err(error) => error,
            };
        assert!(matches!(
            ambiguous,
            VirtioPciEndpointError::MessageRegistry {
                source: GuestMessageInterruptRegistryError::AmbiguousMessage
            }
        ));
        assert!(
            duplicate_signals
                .iter()
                .all(|signals| signals.lock().unwrap().is_empty())
        );
    }

    #[test]
    fn retained_publication_rejects_interrupt_registry_owner_mismatch() {
        let fixture = retained_transport_fixture();
        let (prepared_registry, _) = registry_for_messages(&fixture.messages);
        let prepared_observer = prepared_registry.clone();
        let prepared =
            prepare_retained_endpoint(&fixture, &fixture.state, prepared_registry).unwrap();
        let CountingResourcesFixture {
            registry: resource_registry,
            resources,
            signals: _,
            releases,
        } = counting_resources_for_messages(&fixture.messages);
        let resource_observer = resource_registry.clone();
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, fixture.bar_range);
        let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
        let mut dispatcher = MmioDispatcher::new();

        let error = prepared
            .publish(&mut allocator, segment.clone(), &mut dispatcher, resources)
            .expect_err("foreign interrupt resources must reject publication");
        assert!(matches!(
            error,
            VirtioPciPublicationError::InterruptRegistryMismatch
        ));
        assert_eq!(*releases.lock().unwrap(), 1);
        assert_eq!(
            resource_observer.phase().unwrap(),
            GuestMessageInterruptRegistryPhase::Released
        );
        assert_eq!(
            prepared_observer.phase().unwrap(),
            GuestMessageInterruptRegistryPhase::Active
        );
        assert_eq!(allocator.available_ranges(), &[fixture.bar_range]);
        assert_eq!(
            segment
                .with_segment(|segment| segment.function_count())
                .unwrap(),
            1
        );
        assert!(dispatcher.regions().is_empty());
        prepared_observer.release().unwrap();
    }

    #[test]
    fn retained_publication_releases_interrupts_when_endpoint_state_is_poisoned() {
        let fixture = retained_transport_fixture();
        let CountingResourcesFixture {
            registry,
            resources,
            signals: _,
            releases,
        } = counting_resources_for_messages(&fixture.messages);
        let prepared = prepare_retained_endpoint(&fixture, &fixture.state, registry).unwrap();
        let poison = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = prepared.endpoint.inner.state.lock().unwrap();
            panic!("inject retained endpoint state poison");
        }));
        assert!(poison.is_err());
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, fixture.bar_range);
        let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
        let mut dispatcher = MmioDispatcher::new();

        let error = prepared
            .publish(&mut allocator, segment, &mut dispatcher, resources)
            .expect_err("poisoned retained endpoint must reject publication");

        assert!(matches!(error, VirtioPciPublicationError::Rollback { .. }));
        assert_eq!(*releases.lock().unwrap(), 1);
        assert_eq!(allocator.available_ranges(), &[fixture.bar_range]);
        assert!(dispatcher.regions().is_empty());
    }

    #[test]
    fn retained_publication_preflights_all_exact_resource_conflicts() {
        let fixture = retained_transport_fixture();

        {
            let CountingResourcesFixture {
                registry,
                resources,
                signals: _,
                releases,
            } = counting_resources_for_messages(&fixture.messages);
            let prepared = prepare_retained_endpoint(&fixture, &fixture.state, registry).unwrap();
            let mut allocator =
                PciBarAllocator::new(PciBarAddressSpace::Memory64, fixture.bar_range);
            let incumbent = allocator.reserve_exact(fixture.bar_range).unwrap();
            let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
            let mut dispatcher = MmioDispatcher::new();
            let error = prepared
                .publish(&mut allocator, segment.clone(), &mut dispatcher, resources)
                .expect_err("occupied exact BAR should reject");
            assert!(matches!(
                error,
                VirtioPciPublicationError::BarAllocation {
                    source: PciBarAllocationError::ExactRangeOccupied
                }
            ));
            assert_eq!(*releases.lock().unwrap(), 1);
            assert_eq!(
                segment
                    .with_segment(|segment| segment.function_count())
                    .unwrap(),
                1
            );
            assert!(dispatcher.regions().is_empty());
            allocator
                .release(&incumbent)
                .expect("incumbent BAR lease must survive preflight");
        }

        {
            let CountingResourcesFixture {
                registry,
                resources,
                signals: _,
                releases,
            } = counting_resources_for_messages(&fixture.messages);
            let prepared = prepare_retained_endpoint(&fixture, &fixture.state, registry).unwrap();
            let mut allocator =
                PciBarAllocator::new(PciBarAddressSpace::Memory64, fixture.bar_range);
            let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
            let incumbent = segment
                .with_segment(|segment| {
                    segment.add_function_at(
                        fixture.sbdf,
                        PciType0Configuration::new(
                            0x1af4,
                            0x1044,
                            1,
                            PciClassCode::Unclassified,
                            0,
                            0,
                            0x1af4,
                            0x1044,
                        ),
                    )
                })
                .unwrap()
                .unwrap();
            let mut dispatcher = MmioDispatcher::new();
            let error = prepared
                .publish(&mut allocator, segment.clone(), &mut dispatcher, resources)
                .expect_err("occupied exact SBDF should reject");
            assert!(matches!(
                error,
                VirtioPciPublicationError::SegmentAdd {
                    source: PciSegmentError::DuplicateIdentity { sbdf }
                } if sbdf == fixture.sbdf
            ));
            assert_eq!(*releases.lock().unwrap(), 1);
            assert_eq!(allocator.available_ranges(), &[fixture.bar_range]);
            assert_eq!(
                segment
                    .with_segment(|segment| segment.function_count())
                    .unwrap(),
                2
            );
            assert!(dispatcher.regions().is_empty());
            segment
                .with_segment(|segment| segment.remove_function(&incumbent))
                .unwrap()
                .expect("incumbent function lease must survive preflight");
        }

        {
            let CountingResourcesFixture {
                registry,
                resources,
                signals: _,
                releases,
            } = counting_resources_for_messages(&fixture.messages);
            let prepared = prepare_retained_endpoint(&fixture, &fixture.state, registry).unwrap();
            let mut allocator =
                PciBarAllocator::new(PciBarAddressSpace::Memory64, fixture.bar_range);
            let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
            let mut dispatcher = MmioDispatcher::new();
            let incumbent_id = MmioRegionId::new(1007);
            dispatcher
                .insert_region(
                    incumbent_id,
                    fixture.bar_range.start(),
                    fixture.bar_range.size(),
                )
                .unwrap();
            let error = prepared
                .publish(&mut allocator, segment.clone(), &mut dispatcher, resources)
                .expect_err("occupied exact MMIO range should reject");
            assert!(matches!(
                error,
                VirtioPciPublicationError::MmioRegistration {
                    source: MmioRegistrationError::Region { .. }
                }
            ));
            assert_eq!(*releases.lock().unwrap(), 1);
            assert_eq!(allocator.available_ranges(), &[fixture.bar_range]);
            assert_eq!(
                segment
                    .with_segment(|segment| segment.function_count())
                    .unwrap(),
                1
            );
            assert_eq!(dispatcher.regions().len(), 1);
            assert_eq!(dispatcher.regions()[0].id(), incumbent_id);
        }
    }

    #[test]
    fn retained_publication_rolls_back_each_commit_stage_and_retries_exactly() {
        let fixture = retained_transport_fixture();
        for (stage, name) in [
            (RetainedPublicationStage::Preflight, "preflight"),
            (RetainedPublicationStage::Bar, "BAR"),
            (RetainedPublicationStage::Function, "function"),
            (RetainedPublicationStage::Mmio, "MMIO"),
        ] {
            let CountingResourcesFixture {
                registry,
                resources,
                signals,
                releases,
            } = counting_resources_for_messages(&fixture.messages);
            let prepared = prepare_retained_endpoint(&fixture, &fixture.state, registry).unwrap();
            let mut allocator =
                PciBarAllocator::new(PciBarAddressSpace::Memory64, fixture.bar_range);
            let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
            let mut dispatcher = MmioDispatcher::new();

            let error = prepared
                .publish_with_hook(
                    &mut allocator,
                    segment.clone(),
                    &mut dispatcher,
                    resources,
                    |current| {
                        if current == stage {
                            Err(VirtioPciPublicationError::InjectedFailure { stage: name })
                        } else {
                            Ok(())
                        }
                    },
                )
                .expect_err("injected publication failure should be returned");
            assert!(matches!(
                error,
                VirtioPciPublicationError::InjectedFailure { stage: actual } if actual == name
            ));
            assert_eq!(*releases.lock().unwrap(), 1, "{name}");
            assert!(
                signals
                    .iter()
                    .all(|signals| signals.lock().unwrap().is_empty()),
                "{name}"
            );
            assert_eq!(allocator.available_ranges(), &[fixture.bar_range], "{name}");
            assert_eq!(
                segment
                    .with_segment(|segment| segment.function_count())
                    .unwrap(),
                1,
                "{name}"
            );
            assert!(dispatcher.regions().is_empty(), "{name}");

            let probe = allocator
                .reserve_exact(fixture.bar_range)
                .expect("rolled-back exact BAR should be reusable");
            allocator.release(&probe).unwrap();

            let CountingResourcesFixture {
                registry,
                resources,
                signals: retry_signals,
                releases: retry_releases,
            } = counting_resources_for_messages(&fixture.messages);
            let prepared = prepare_retained_endpoint(&fixture, &fixture.state, registry).unwrap();
            let mut published = prepared
                .publish(&mut allocator, segment.clone(), &mut dispatcher, resources)
                .expect("the identical retained plan should retry successfully");
            assert_eq!(
                published.endpoint().transport_state().unwrap(),
                fixture.state,
                "{name}"
            );
            assert!(
                retry_signals
                    .iter()
                    .all(|signals| signals.lock().unwrap().is_empty()),
                "{name}"
            );
            published
                .teardown(&mut dispatcher, &mut allocator)
                .expect("retried retained endpoint should tear down");
            assert_eq!(*retry_releases.lock().unwrap(), 1, "{name}");
            assert_eq!(allocator.available_ranges(), &[fixture.bar_range], "{name}");
            assert_eq!(
                segment
                    .with_segment(|segment| segment.function_count())
                    .unwrap(),
                1,
                "{name}"
            );
            assert!(dispatcher.regions().is_empty(), "{name}");
        }
    }

    #[test]
    fn publication_teardown_unpublishes_paths_before_reusing_leases() {
        let capacity = GuestMemoryRange::new(
            GuestAddress::new(0x2_0000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE,
        )
        .unwrap();
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, capacity);
        let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
        let mut dispatcher = MmioDispatcher::new();
        let mut published = PublishedVirtioPciEndpoint::publish(
            VirtioPciIdentity::new(VirtioDeviceType::new(4).unwrap(), 0),
            &[8],
            UnsupportedVirtioDeviceConfig,
            NoopVirtioDeviceActivation,
            false,
            &mut allocator,
            segment.clone(),
            &mut dispatcher,
            crate::mmio::MmioRegionId::new(91),
            registry_resources(2),
        )
        .expect("endpoint publication should succeed");
        let published_range = published.bar_range().expect("published BAR should exist");
        assert_eq!(dispatcher.regions().len(), 1);
        assert_eq!(
            segment
                .with_segment(|segment| segment.function_count())
                .unwrap(),
            2
        );

        published
            .prepare_teardown(&mut dispatcher)
            .expect("teardown preparation should suspend exact paths");
        assert!(dispatcher.regions().is_empty());
        assert_eq!(
            segment
                .with_segment(|segment| segment.function_count())
                .unwrap(),
            1
        );
        assert_eq!(
            published.endpoint().phase().expect("phase should read"),
            VirtioPciEndpointPhase::Quiescing
        );
        assert!(allocator.allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE).is_err());
        published
            .rollback_prepared_teardown(&mut dispatcher)
            .expect("recoverable teardown abort should restore exact paths");
        assert_eq!(dispatcher.regions().len(), 1);
        assert_eq!(
            segment
                .with_segment(|segment| segment.function_count())
                .unwrap(),
            2
        );
        assert_eq!(
            published.endpoint().phase().expect("phase should read"),
            VirtioPciEndpointPhase::Active
        );

        published
            .teardown(&mut dispatcher, &mut allocator)
            .expect("ordered teardown should succeed");
        assert!(published.is_released());
        assert!(dispatcher.regions().is_empty());
        assert_eq!(
            segment
                .with_segment(|segment| segment.function_count())
                .unwrap(),
            1
        );
        let reused = allocator
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("released BAR should be reusable");
        assert_eq!(reused.range(), published_range);
    }

    #[test]
    fn publication_teardown_reserves_the_slot_until_admitted_work_drains() {
        let capacity = GuestMemoryRange::new(
            GuestAddress::new(0x2_1000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE,
        )
        .unwrap();
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, capacity);
        let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
        let mut dispatcher = MmioDispatcher::new();
        let mut published = PublishedVirtioPciEndpoint::publish(
            VirtioPciIdentity::new(VirtioDeviceType::new(4).unwrap(), 0),
            &[8],
            UnsupportedVirtioDeviceConfig,
            NoopVirtioDeviceActivation,
            false,
            &mut allocator,
            segment.clone(),
            &mut dispatcher,
            crate::mmio::MmioRegionId::new(93),
            registry_resources(2),
        )
        .expect("endpoint publication should succeed");
        let sbdf = published
            .function_lease
            .as_ref()
            .expect("published endpoint should retain its function lease")
            .sbdf();
        let endpoint = published.endpoint().clone();
        let work = endpoint
            .admit_device_work()
            .expect("published endpoint should admit device work");

        thread::scope(|scope| {
            let teardown = scope.spawn(|| published.teardown(&mut dispatcher, &mut allocator));
            let deadline = Instant::now() + Duration::from_secs(1);
            let guest_path_unpublished = loop {
                let unpublished = segment
                    .with_segment(|segment| segment.function_count() == 1)
                    .expect("test PCI segment should remain available");
                if unpublished || Instant::now() >= deadline {
                    break unpublished;
                }
                thread::yield_now();
            };
            assert!(
                guest_path_unpublished,
                "teardown should remove the ECAM path before waiting for work"
            );

            let replacement = PciType0Configuration::new(
                0x0042,
                0,
                0,
                PciClassCode::Unclassified,
                0,
                0,
                0x0042,
                0,
            );
            assert!(matches!(
                segment
                    .with_segment(|segment| segment.add_function_at(sbdf, replacement))
                    .expect("test PCI segment should remain available"),
                Err(PciSegmentError::DuplicateIdentity { sbdf: duplicate }) if duplicate == sbdf
            ));
            assert!(
                !teardown.is_finished(),
                "teardown must wait for already admitted device work"
            );
            work.trigger(VirtioInterruptIntent::Configuration)
                .expect("already admitted work should complete after guest unpublication");
            drop(work);
            teardown
                .join()
                .expect("teardown thread should finish")
                .expect("teardown should succeed after work drains");
        });

        let replacement =
            PciType0Configuration::new(0x0042, 0, 0, PciClassCode::Unclassified, 0, 0, 0x0042, 0);
        let replacement_lease = segment
            .with_segment(|segment| segment.add_function_at(sbdf, replacement))
            .expect("test PCI segment should remain available")
            .expect("slot should become reusable only after teardown completes");
        segment
            .with_segment(|segment| segment.remove_function(&replacement_lease))
            .expect("test PCI segment should remain available")
            .expect("replacement slot should release");
    }

    #[test]
    fn failed_mmio_publication_rolls_back_function_bar_and_registry() {
        let capacity = GuestMemoryRange::new(
            GuestAddress::new(0x3_0000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE,
        )
        .unwrap();
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, capacity);
        let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(
                crate::mmio::MmioRegionId::new(90),
                capacity.start(),
                capacity.size(),
            )
            .expect("conflicting legacy region should register");

        let error = PublishedVirtioPciEndpoint::publish(
            VirtioPciIdentity::new(VirtioDeviceType::new(4).unwrap(), 0),
            &[8],
            UnsupportedVirtioDeviceConfig,
            NoopVirtioDeviceActivation,
            false,
            &mut allocator,
            segment.clone(),
            &mut dispatcher,
            crate::mmio::MmioRegionId::new(91),
            registry_resources(2),
        )
        .expect_err("overlapping BAR publication must fail");
        assert!(matches!(
            error,
            VirtioPciPublicationError::MmioRegistration { .. }
        ));
        assert_eq!(
            segment
                .with_segment(|segment| segment.function_count())
                .unwrap(),
            1
        );
        let reused = allocator
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("failed publication must return its BAR");
        assert_eq!(reused.range(), capacity);
    }

    #[test]
    fn failed_bar_allocation_releases_preallocated_interrupt_resources() {
        let capacity = GuestMemoryRange::new(
            GuestAddress::new(0x5_0000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE / 2,
        )
        .unwrap();
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, capacity);
        let segment = SharedPciSegment::new(crate::pci::PciSegment::new());
        let mut dispatcher = MmioDispatcher::new();
        let registry = registry_resources(2).registry();
        let observer = registry.clone();
        let releases = Arc::new(Mutex::new(0));

        let error = PublishedVirtioPciEndpoint::publish(
            VirtioPciIdentity::new(VirtioDeviceType::new(4).unwrap(), 0),
            &[8],
            UnsupportedVirtioDeviceConfig,
            NoopVirtioDeviceActivation,
            false,
            &mut allocator,
            segment.clone(),
            &mut dispatcher,
            crate::mmio::MmioRegionId::new(92),
            CountingInterruptResources {
                registry,
                releases: Arc::clone(&releases),
            },
        )
        .expect_err("undersized BAR space must reject publication");
        assert!(matches!(
            error,
            VirtioPciPublicationError::BarAllocation { .. }
        ));
        assert_eq!(*releases.lock().unwrap(), 1);
        assert_eq!(
            observer.phase().unwrap(),
            crate::message_interrupt::GuestMessageInterruptRegistryPhase::Released
        );
        assert_eq!(
            segment
                .with_segment(|segment| segment.function_count())
                .unwrap(),
            1
        );
        assert!(dispatcher.regions().is_empty());
    }
}
