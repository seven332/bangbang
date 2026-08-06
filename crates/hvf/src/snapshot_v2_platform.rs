//! Unpublished native-v2 multi-vCPU HVF platform reconstruction.

use std::fmt;
use std::io::{Seek, Write};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use device_tree::{DeviceTree, Node};

use bangbang_runtime::block::BlockMmioLayout;
use bangbang_runtime::boot::canonical_process_root_block_command_line;
use bangbang_runtime::fdt::{ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET, Arm64FdtPciHost};
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::logger::{GuestLogger, LoggerBackendOutcome, LoggerTimeIdentityOutcome};
use bangbang_runtime::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryRange, aarch64,
};
use bangbang_runtime::metrics::{SharedInterruptMetrics, SharedVcpuMetrics};
use bangbang_runtime::mmio::{MmioDispatcher, MmioRegion, MmioRegionId};
use bangbang_runtime::pci::{
    Arm64PciAddressPlan, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
    PCI_SEGMENT_ZERO, PciBarAddressSpace, PciBarPrefetchable, PciSbdf,
};
use bangbang_runtime::pvtime::{
    ARM64_PVTIME_STOLEN_TIME_OFFSET, ARM64_PVTIME_STRUCTURE_SIZE, Arm64PvTimeLayout,
    Arm64PvTimeStAbi,
};
use bangbang_runtime::rtc::{RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
use bangbang_runtime::serial::{
    SERIAL_MMIO_DEVICE_WINDOW_SIZE, SerialMmioDevice, SharedSerialOutput,
};
use bangbang_runtime::snapshot_device_v2::{
    SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind, SnapshotV2RootRestorePlan,
    SnapshotV2RootRestorePlanError,
};
use bangbang_runtime::snapshot_format_v2::NATIVE_V2_LEGACY_PLATFORM_VERSION;
use bangbang_runtime::snapshot_memory_v2::{
    SnapshotV2MemoryBinding, write_snapshot_v2_memory_image_with_compatibility_version,
};
use bangbang_runtime::startup::{
    Arm64BootResourceError, Arm64BootRtcDevice, Arm64BootSerialDevice, Arm64BootSerialDeviceConfig,
    Arm64BootVmClockDevice, Arm64BootVmGenIdDevice, PrepareArm64SnapshotTimeIdentityError,
    prepare_arm64_snapshot_time_identity, register_arm64_boot_restored_serial_mmio,
    register_arm64_boot_rtc_mmio, register_arm64_boot_serial_mmio, replace_arm64_boot_vmgenid,
};
use bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE;
use bangbang_runtime::virtio_pci::{
    VIRTIO_PCI_CAPABILITY_BAR_INDEX, VIRTIO_PCI_CAPABILITY_BAR_SIZE,
};
use bangbang_runtime::{BackendError, VmBackend};
use crc64::crc64;

use crate::backend::HvfBackend;
use crate::coordinator::{HvfVcpuRunControl, HvfVcpuRunCoordinatorError};
use crate::cpu_template::HvfArm64CpuTemplateError;
use crate::dirty::HvfDirtyWriteTrackerStartError;
use crate::gic::{
    HvfGicError, HvfGicInterruptLineAllocator, HvfGicMetadata, HvfGicMsiConfiguration,
    HvfGicMsiMetadata, HvfGicSpiSignalError, HvfGicSpiSignaler, HvfInterruptLineAllocationError,
};
use crate::memory::{
    HvfGuestMemoryMappingError, HvfMemoryPermissions, HvfSnapshotV2MemoryHotplugMappingPlan,
};
use crate::pvtime::HvfArm64PvTimeAccountingConfig;
use crate::runner::{HvfArm64SnapshotV2VcpuRestore, HvfVcpuRunStepOutcome, HvfVcpuRunnerError};
use crate::session_vcpu::{
    HvfArm64BootVcpuError, HvfArm64BootVcpuSession, HvfArm64StablePausedTopologyCaptureError,
    HvfArm64StablePausedTopologyImportError,
};
use crate::snapshot_bundle::HvfSnapshotV1CompatibilityState;
use crate::snapshot_v2::{
    HvfSnapshotV2GlobalState, HvfSnapshotV2MachineState, HvfSnapshotV2PlatformState,
    HvfSnapshotV2State, HvfSnapshotV2TimeState, HvfSnapshotV2VcpuState,
};
use crate::snapshot_v2_multi_block_platform::{
    HvfSnapshotV2MultiBlockMmioRecordPlan, HvfSnapshotV2MultiBlockPciPlan,
};
use crate::snapshot_v2_storage_platform::{
    HvfSnapshotV2StorageMmioRecordPlan, HvfSnapshotV2StoragePciHostPlan,
};
use crate::startup::{
    HvfArm64BootSnapshotV2CaptureError, HvfArm64BootSnapshotV2CaptureStage,
    HvfArm64BootVmClockRestoreError, HvfArm64BootVmGenIdRestoreError, PCI_ENDPOINT_SLOT_COUNT,
    backend_outcome_for_run_step_error, backend_outcome_for_run_step_outcome,
    capture_hvf_snapshot_v2_time_state, observe_pvtime_topology_capture, observe_vmclock_restore,
    observe_vmgenid_restore, pci_root_restore_bar_region_id,
    pci_root_restore_gic_msi_configuration, replace_vmgenid_and_signal_with,
    update_vmclock_and_signal_with,
};
use crate::topology::{HvfVcpuTopology, HvfVcpuTopologyError};
use crate::vcpu::HvfArm64VcpuIdentificationRegisterState;

const REDACTED: &str = "<redacted>";
pub(crate) const PROCESS_SERIAL_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_2000);
pub(crate) const PROCESS_SERIAL_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(20);
pub(crate) const PROCESS_RTC_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_1000);
pub(crate) const PROCESS_RTC_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(10);

/// Destination process policy needed to verify the exact root allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2RootProcessConfig {
    block_mmio_layout: BlockMmioLayout,
    pci_enabled: bool,
}

impl HvfSnapshotV2RootProcessConfig {
    /// Creates one closed process-policy input without optional-device overrides.
    pub const fn new(block_mmio_layout: BlockMmioLayout, pci_enabled: bool) -> Self {
        Self {
            block_mmio_layout,
            pci_enabled,
        }
    }

    /// Returns the configured root MMIO allocation sequence.
    pub const fn block_mmio_layout(self) -> BlockMmioLayout {
        self.block_mmio_layout
    }

    /// Returns whether the process selected the all-virtio PCI profile.
    pub const fn pci_enabled(self) -> bool {
        self.pci_enabled
    }
}

/// Exact product allocation selected for the root transport.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV2RootTransportPlan {
    /// First virtio-mmio block window and SPI in startup order.
    Mmio {
        /// Exact dispatcher region.
        region: MmioRegion,
        /// Exact GIC SPI line.
        interrupt_line: GuestInterruptLine,
    },
    /// First modern PCI endpoint and capability BAR.
    Pci {
        /// Exact bus/function identity.
        sbdf: PciSbdf,
        /// Destination dispatcher identity for the BAR owner.
        bar_region_id: MmioRegionId,
        /// Exact capability BAR range.
        bar_range: GuestMemoryRange,
        /// Retained GICv2m frame and route range.
        msi: crate::gic::HvfGicMsiMetadata,
    },
}

impl HvfSnapshotV2RootTransportPlan {
    /// Returns the selected transport profile.
    pub const fn kind(self) -> SnapshotV2DeviceTransportKind {
        match self {
            Self::Mmio { .. } => SnapshotV2DeviceTransportKind::Mmio,
            Self::Pci { .. } => SnapshotV2DeviceTransportKind::Pci,
        }
    }
}

impl fmt::Debug for HvfSnapshotV2RootTransportPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2RootTransportPlan")
            .field("kind", &self.kind())
            .field("resources", &REDACTED)
            .finish()
    }
}

/// Deterministic root, UART, VMGenID, and VMClock destination plan.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2RootResourcePlan {
    transport: HvfSnapshotV2RootTransportPlan,
    serial_interrupt: GuestInterruptLine,
    vmgenid_interrupt: GuestInterruptLine,
    vmclock_interrupt: GuestInterruptLine,
}

impl HvfSnapshotV2RootResourcePlan {
    /// Returns the exact root transport allocation.
    pub const fn transport(self) -> HvfSnapshotV2RootTransportPlan {
        self.transport
    }

    /// Returns the canonical process UART interrupt.
    pub const fn serial_interrupt(self) -> GuestInterruptLine {
        self.serial_interrupt
    }

    /// Returns the canonical VMGenID interrupt.
    pub const fn vmgenid_interrupt(self) -> GuestInterruptLine {
        self.vmgenid_interrupt
    }

    /// Returns the canonical VMClock interrupt.
    pub const fn vmclock_interrupt(self) -> GuestInterruptLine {
        self.vmclock_interrupt
    }
}

impl fmt::Debug for HvfSnapshotV2RootResourcePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2RootResourcePlan")
            .field("transport", &self.transport.kind())
            .field("resources", &REDACTED)
            .finish()
    }
}

/// Fully validated, pre-HVF root preparation with owned loaded memory.
pub struct PreparedHvfSnapshotV2RootPlan {
    platform: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    root: SnapshotV2RootRestorePlan,
    resources: HvfSnapshotV2RootResourcePlan,
}

impl PreparedHvfSnapshotV2RootPlan {
    /// Returns the inert root selector for the destination authority layer.
    pub fn selector(&self) -> &str {
        self.root.selector()
    }

    /// Returns the path-shaped-data-free product allocation.
    pub const fn resources(&self) -> HvfSnapshotV2RootResourcePlan {
        self.resources
    }

    /// Returns the validated runtime root plan.
    pub const fn root(&self) -> &SnapshotV2RootRestorePlan {
        &self.root
    }

    /// Consumes the proof into still-unpublished platform resources.
    pub fn into_parts(
        self,
    ) -> (
        HvfSnapshotV2PlatformState,
        GuestMemory,
        SnapshotV2RootRestorePlan,
        HvfSnapshotV2RootResourcePlan,
    ) {
        (self.platform, self.memory, self.root, self.resources)
    }
}

impl fmt::Debug for PreparedHvfSnapshotV2RootPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedHvfSnapshotV2RootPlan")
            .field("transport", &self.resources.transport.kind())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Redacted rejection from the pre-HVF root preparation boundary.
pub enum PrepareHvfSnapshotV2RootPlanError {
    /// Loaded memory does not exactly match the artifact binding.
    MemoryTopology,
    /// FDT bytes cannot be read or do not match their retained checksum.
    Fdt(Box<HvfSnapshotV2PlatformRestoreFailure>),
    /// Device-graph continuation does not agree with loaded memory.
    Root(SnapshotV2RootRestorePlanError),
    /// Process transport selection disagrees with the root graph.
    TransportPolicy,
    /// Retained placement is not the exact fresh product allocation.
    ResourcePlan,
    /// The retained GIC cannot supply the deterministic SPI sequence.
    Interrupt(HvfInterruptLineAllocationError),
    /// Source-profile or root metadata cannot reconstruct the exact product shell.
    ProcessFdt {
        /// Value-free mismatch category.
        mismatch: HvfSnapshotV2ProcessFdtMismatch,
    },
    /// Portable time/identity memory failed preflight.
    TimeIdentity(Box<HvfSnapshotV2PlatformRestoreFailure>),
}

impl fmt::Debug for PrepareHvfSnapshotV2RootPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::MemoryTopology => "memory topology",
            Self::Fdt(_) => "FDT identity",
            Self::Root(_) => "root continuation",
            Self::TransportPolicy => "transport policy",
            Self::ResourcePlan => "resource plan",
            Self::Interrupt(_) => "interrupt plan",
            Self::ProcessFdt { .. } => "process FDT",
            Self::TimeIdentity(_) => "time identity",
        };
        let mismatch = match self {
            Self::ProcessFdt { mismatch } => Some(mismatch),
            _ => None,
        };
        formatter
            .debug_struct("PrepareHvfSnapshotV2RootPlanError")
            .field("category", &category)
            .field("mismatch", &mismatch)
            .field("source", &REDACTED)
            .finish()
    }
}

impl fmt::Display for PrepareHvfSnapshotV2RootPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::MemoryTopology => "memory topology",
            Self::Fdt(_) => "FDT identity",
            Self::Root(_) => "root continuation",
            Self::TransportPolicy => "transport policy",
            Self::ResourcePlan => "resource plan",
            Self::Interrupt(_) => "interrupt plan",
            Self::ProcessFdt { .. } => "process FDT",
            Self::TimeIdentity(_) => "time identity",
        };
        if let Self::ProcessFdt { mismatch } = self {
            return write!(
                formatter,
                "native-v2 root preparation {category} ({mismatch}) failed"
            );
        }
        write!(formatter, "native-v2 root preparation {category} failed")
    }
}

impl std::error::Error for PrepareHvfSnapshotV2RootPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fdt(source) | Self::TimeIdentity(source) => Some(source.as_ref()),
            Self::Root(source) => Some(source),
            Self::Interrupt(source) => Some(source),
            Self::MemoryTopology
            | Self::TransportPolicy
            | Self::ResourcePlan
            | Self::ProcessFdt { .. } => None,
        }
    }
}

/// Closed fresh-output shell accepted by native-v2 process reconstruction.
pub struct HvfSnapshotV2DefaultProcessShell {
    serial_output: SharedSerialOutput,
    guest_logger: GuestLogger,
}

impl HvfSnapshotV2DefaultProcessShell {
    /// Bind one fresh destination output to the canonical process UART.
    pub fn new(serial_output: SharedSerialOutput) -> Self {
        Self {
            serial_output,
            guest_logger: GuestLogger::default(),
        }
    }

    pub fn with_guest_logger(mut self, logger: GuestLogger) -> Self {
        self.guest_logger = logger;
        self
    }
}

impl fmt::Debug for HvfSnapshotV2DefaultProcessShell {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2DefaultProcessShell")
            .field("profile", &"default-uart")
            .field("output", &REDACTED)
            .finish()
    }
}

/// Closed complete-UART shell accepted only by exact-2.7 reconstruction.
#[doc(hidden)]
pub struct HvfSnapshotV2RestoredSerialShell {
    serial: SerialMmioDevice<SharedSerialOutput>,
    guest_logger: GuestLogger,
}

impl HvfSnapshotV2RestoredSerialShell {
    /// Bind one complete restored UART to destination platform placement.
    pub fn new(serial: SerialMmioDevice<SharedSerialOutput>) -> Self {
        Self {
            serial,
            guest_logger: GuestLogger::default(),
        }
    }

    pub fn with_guest_logger(mut self, logger: GuestLogger) -> Self {
        self.guest_logger = logger;
        self
    }
}

impl fmt::Debug for HvfSnapshotV2RestoredSerialShell {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfSnapshotV2RestoredSerialShell")
            .field("profile", &"restored-uart")
            .field("state", &REDACTED)
            .finish()
    }
}

/// Destination product policy for an exact-2.7 serial-only platform.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2SerialOnlyProcessConfig {
    pci_enabled: bool,
    exact_pci_msi_interrupt_count: Option<u32>,
}

impl HvfSnapshotV2SerialOnlyProcessConfig {
    /// Select the exact destination process PCI policy.
    pub const fn new(pci_enabled: bool) -> Self {
        Self {
            pci_enabled,
            exact_pci_msi_interrupt_count: None,
        }
    }

    pub(crate) const fn with_exact_pci_msi_interrupt_count(
        exact_pci_msi_interrupt_count: u32,
    ) -> Self {
        Self {
            pci_enabled: true,
            exact_pci_msi_interrupt_count: Some(exact_pci_msi_interrupt_count),
        }
    }

    /// Returns whether the destination selected the product PCI host.
    pub const fn pci_enabled(self) -> bool {
        self.pci_enabled
    }
}

enum HvfSnapshotV2ProcessSerialShell {
    Default(HvfSnapshotV2DefaultProcessShell),
    Restored(HvfSnapshotV2RestoredSerialShell),
}

impl HvfSnapshotV2ProcessSerialShell {
    fn guest_logger(&self) -> GuestLogger {
        match self {
            Self::Default(shell) => shell.guest_logger.clone(),
            Self::Restored(shell) => shell.guest_logger.clone(),
        }
    }
}

impl From<HvfSnapshotV2DefaultProcessShell> for HvfSnapshotV2ProcessSerialShell {
    fn from(shell: HvfSnapshotV2DefaultProcessShell) -> Self {
        Self::Default(shell)
    }
}

impl From<HvfSnapshotV2RestoredSerialShell> for HvfSnapshotV2ProcessSerialShell {
    fn from(shell: HvfSnapshotV2RestoredSerialShell) -> Self {
        Self::Restored(shell)
    }
}

pub(crate) struct HvfSnapshotV2MultiBlockMmioShellPlan<'a> {
    pub(crate) command_line: &'a str,
    pub(crate) records: &'a [HvfSnapshotV2MultiBlockMmioRecordPlan],
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2MultiBlockPciShellPlan<'a> {
    pub(crate) command_line: &'a str,
    pub(crate) pci: &'a HvfSnapshotV2MultiBlockPciPlan,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2StorageMmioShellPlan<'a> {
    pub(crate) command_line: &'a str,
    pub(crate) block_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
    pub(crate) pmem_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2BalloonMmioShellPlan<'a> {
    pub(crate) balloon_interrupt: GuestInterruptLine,
    pub(crate) command_line: Option<&'a str>,
    pub(crate) block_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
    pub(crate) pmem_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
    pub(crate) entropy_interrupt: Option<GuestInterruptLine>,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2MemoryHotplugMmioShellPlan<'a> {
    pub(crate) balloon_interrupt: Option<GuestInterruptLine>,
    pub(crate) command_line: Option<&'a str>,
    pub(crate) block_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
    pub(crate) pmem_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
    pub(crate) entropy_interrupt: Option<GuestInterruptLine>,
    pub(crate) memory_hotplug_interrupt: GuestInterruptLine,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2NetworkMmioShellPlan<'a> {
    pub(crate) balloon_interrupt: Option<GuestInterruptLine>,
    pub(crate) command_line: Option<&'a str>,
    pub(crate) block_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
    pub(crate) network_interrupts: &'a [GuestInterruptLine],
    pub(crate) pmem_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
    pub(crate) following_interrupt: Option<GuestInterruptLine>,
    pub(crate) entropy_interrupt: Option<GuestInterruptLine>,
    pub(crate) memory_hotplug_interrupt: Option<GuestInterruptLine>,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2StoragePciShellPlan<'a> {
    pub(crate) command_line: &'a str,
    pub(crate) pci: &'a HvfSnapshotV2StoragePciHostPlan,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

pub(crate) struct HvfSnapshotV2NetworkPciShellPlan<'a> {
    pub(crate) storage: Option<HvfSnapshotV2StoragePciShellPlan<'a>>,
    pub(crate) host: Arm64FdtPciHost,
    pub(crate) msi: HvfGicMsiMetadata,
    pub(crate) endpoint_count: usize,
    pub(crate) route_demand: usize,
    pub(crate) memory_hotplug: bool,
    pub(crate) serial_interrupt: GuestInterruptLine,
    pub(crate) vmgenid_interrupt: GuestInterruptLine,
    pub(crate) vmclock_interrupt: GuestInterruptLine,
}

enum HvfSnapshotV2ProcessShellRestore<'a> {
    DeviceFree(HvfSnapshotV2ProcessSerialShell),
    SerialOnly {
        shell: HvfSnapshotV2ProcessSerialShell,
        process: HvfSnapshotV2SerialOnlyProcessConfig,
    },
    SerialEntropyMmio {
        shell: HvfSnapshotV2ProcessSerialShell,
        entropy_interrupt: GuestInterruptLine,
    },
    Root {
        shell: HvfSnapshotV2ProcessSerialShell,
        resources: HvfSnapshotV2RootResourcePlan,
        partuuid: Option<String>,
    },
    MultiBlockMmio {
        shell: HvfSnapshotV2ProcessSerialShell,
        plan: HvfSnapshotV2MultiBlockMmioShellPlan<'a>,
    },
    MultiBlockPci {
        shell: HvfSnapshotV2ProcessSerialShell,
        plan: HvfSnapshotV2MultiBlockPciShellPlan<'a>,
    },
    StorageMmio {
        shell: HvfSnapshotV2ProcessSerialShell,
        plan: HvfSnapshotV2StorageMmioShellPlan<'a>,
    },
    StorageEntropyMmio {
        shell: HvfSnapshotV2ProcessSerialShell,
        plan: HvfSnapshotV2StorageMmioShellPlan<'a>,
        entropy_interrupt: GuestInterruptLine,
    },
    BalloonMmio {
        shell: HvfSnapshotV2ProcessSerialShell,
        plan: HvfSnapshotV2BalloonMmioShellPlan<'a>,
    },
    MemoryHotplugMmio {
        shell: HvfSnapshotV2ProcessSerialShell,
        plan: HvfSnapshotV2MemoryHotplugMmioShellPlan<'a>,
    },
    NetworkMmio {
        shell: HvfSnapshotV2ProcessSerialShell,
        plan: HvfSnapshotV2NetworkMmioShellPlan<'a>,
    },
    NetworkPci {
        shell: HvfSnapshotV2ProcessSerialShell,
        plan: HvfSnapshotV2NetworkPciShellPlan<'a>,
    },
    StoragePci {
        shell: HvfSnapshotV2ProcessSerialShell,
        plan: HvfSnapshotV2StoragePciShellPlan<'a>,
    },
}

impl HvfSnapshotV2ProcessShellRestore<'_> {
    fn guest_logger(&self) -> GuestLogger {
        match self {
            Self::DeviceFree(shell)
            | Self::SerialOnly { shell, .. }
            | Self::SerialEntropyMmio { shell, .. }
            | Self::Root { shell, .. }
            | Self::MultiBlockMmio { shell, .. }
            | Self::MultiBlockPci { shell, .. }
            | Self::StorageMmio { shell, .. }
            | Self::StorageEntropyMmio { shell, .. }
            | Self::BalloonMmio { shell, .. }
            | Self::MemoryHotplugMmio { shell, .. }
            | Self::NetworkMmio { shell, .. }
            | Self::NetworkPci { shell, .. }
            | Self::StoragePci { shell, .. } => shell.guest_logger(),
        }
    }
}

enum HvfSnapshotV2ProcessBlockFdtPlan<'a> {
    None,
    Root {
        partuuid: Option<String>,
        transport: HvfSnapshotV2RootTransportPlan,
    },
    MultiBlockMmio {
        command_line: &'a str,
        records: &'a [HvfSnapshotV2MultiBlockMmioRecordPlan],
    },
    MultiBlockPci {
        command_line: &'a str,
        pci: &'a HvfSnapshotV2MultiBlockPciPlan,
    },
    StorageMmio {
        command_line: &'a str,
        block_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
        pmem_records: &'a [HvfSnapshotV2StorageMmioRecordPlan],
    },
    StoragePci {
        command_line: &'a str,
        pci: &'a HvfSnapshotV2StoragePciHostPlan,
    },
}

/// Ordered reconstruction stage for the unpublished native-v2 platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2PlatformRestoreStage {
    /// Validate supplied memory, FDT identity, cache identity, and cleanup storage.
    Preflight,
    /// Validate and install the exact fresh destination process shell.
    ProcessShell,
    /// Create the empty Hypervisor.framework VM.
    Vm,
    /// Register guest memory and optional dirty tracking.
    Memory,
    /// Create and exactly validate the destination GIC.
    Gic,
    /// Create and validate the complete never-run vCPU topology.
    Topology,
    /// Verify that the complete created topology has the prepared identities.
    TopologyIdentity,
    /// Replay retained effective CPU-template targets.
    CpuTemplate,
    /// Validate one destination member's common processor identity.
    Compatibility { index: usize },
    /// Restore the singular VM-global GIC state.
    GlobalGic,
    /// Restore one complete per-vCPU component.
    Vcpu { index: usize },
    /// Install the fresh destination-time PL031 MMIO handler.
    Rtc,
    /// Configure one never-run vCPU's restored PVTime accumulator.
    PvTime { index: usize },
    /// Validate all runners and identity notification lines before mutation.
    TimeIdentityPreflight,
    /// Replace and notify the destination VMGenID.
    VmGenId,
    /// Atomically update and notify the destination VMClock.
    VmClock,
    /// Import offline/runnable/suspended state into a coordinator born paused.
    Lifecycle,
    /// Publish the completed paused owner without exposing a raw topology.
    Publication,
}

impl fmt::Display for HvfSnapshotV2PlatformRestoreStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Preflight => f.write_str("preflight"),
            Self::ProcessShell => f.write_str("default process shell"),
            Self::Vm => f.write_str("VM creation"),
            Self::Memory => f.write_str("guest memory"),
            Self::Gic => f.write_str("GIC creation"),
            Self::Topology => f.write_str("vCPU topology"),
            Self::TopologyIdentity => f.write_str("vCPU topology identity"),
            Self::CpuTemplate => f.write_str("CPU-template replay"),
            Self::Compatibility { index } => {
                write!(f, "vCPU {index} compatibility")
            }
            Self::GlobalGic => f.write_str("global GIC restore"),
            Self::Vcpu { index } => write!(f, "vCPU {index} restore"),
            Self::Rtc => f.write_str("destination PL031 installation"),
            Self::PvTime { index } => write!(f, "vCPU {index} PVTime restore"),
            Self::TimeIdentityPreflight => f.write_str("time/identity preflight"),
            Self::VmGenId => f.write_str("VMGenID replacement and notification"),
            Self::VmClock => f.write_str("VMClock update and notification"),
            Self::Lifecycle => f.write_str("paused lifecycle import"),
            Self::Publication => f.write_str("paused platform publication"),
        }
    }
}

/// Value-free reason that a retained FDT is not the canonical focused process shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2ProcessFdtMismatch {
    /// The retained FDT does not describe the focused process profile.
    Profile,
    /// The retained FDT cannot be parsed.
    Parse,
    /// The retained FDT has an unexpected root-node inventory.
    RootInventory,
    /// The retained FDT has an unexpected CPU inventory or topology.
    CpuInventory,
    /// The retained FDT memory range does not match the admitted binding.
    Memory,
    /// The retained FDT boot metadata does not match the admitted state.
    Boot,
    /// The retained FDT GIC description does not match the admitted state.
    Gic,
    /// The retained FDT timer description does not match the admitted state.
    Timer,
    /// The retained FDT RTC description does not match the focused shell.
    Rtc,
    /// The retained FDT UART description does not match the focused shell.
    Serial,
    /// The retained FDT VM generation ID description does not match the admitted state.
    VmGenId,
    /// The retained FDT VM clock description does not match the admitted state.
    VmClock,
    /// Root selection or the virtio-mmio node is inconsistent.
    Root,
    /// The PCI host or GICv2m child is inconsistent.
    Pci,
}

impl fmt::Display for HvfSnapshotV2ProcessFdtMismatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Profile => "profile",
            Self::Parse => "parse",
            Self::RootInventory => "root inventory",
            Self::CpuInventory => "CPU inventory",
            Self::Memory => "memory",
            Self::Boot => "boot",
            Self::Gic => "GIC",
            Self::Timer => "timer",
            Self::Rtc => "RTC",
            Self::Serial => "serial",
            Self::VmGenId => "VMGenID",
            Self::VmClock => "VMClock",
            Self::Root => "root",
            Self::Pci => "PCI",
        })
    }
}

/// Typed primary failure retained by a native-v2 platform restore error.
pub enum HvfSnapshotV2PlatformRestoreFailure {
    /// Cleanup evidence could not be reserved before construction.
    Allocation,
    /// Supplied guest-memory ranges differ from the prepared binding.
    MemoryTopology,
    /// FDT verification storage could not be reserved.
    FdtAllocation,
    /// The prepared FDT range could not be read from supplied memory.
    FdtRead(GuestMemoryAccessError),
    /// Supplied memory does not contain the prepared FDT identity.
    FdtIdentity,
    /// Retained legacy FDT or current source-profile evidence is not exact.
    ProcessShellFdt {
        mismatch: HvfSnapshotV2ProcessFdtMismatch,
    },
    /// Deterministic minimal-profile interrupt allocation failed or disagreed.
    ProcessShellInterrupt(HvfInterruptLineAllocationError),
    /// Retained identity lines differ from the minimal serial-first allocation.
    ProcessShellInterruptIdentity,
    /// Fresh destination UART installation failed.
    ProcessShellSerial(Arm64BootResourceError),
    /// Querying the destination cache profile failed.
    CacheQuery(BackendError),
    /// Destination cache facts differ from the prepared compatibility state.
    CacheMismatch,
    /// Portable identity devices disagree with supplied guest memory.
    TimePreparation(PrepareArm64SnapshotTimeIdentityError),
    /// One supplied PVTime record could not be read.
    PvTimeRead(GuestMemoryAccessError),
    /// One supplied PVTime record is invalid or differs from portable state.
    PvTimeMismatch,
    /// Empty VM construction failed.
    CreateVm(BackendError),
    /// Guest-memory registration failed.
    MapMemory(HvfGuestMemoryMappingError),
    /// Dirty-tracking setup requested by the machine state failed.
    DirtyTracking(HvfDirtyWriteTrackerStartError),
    /// GIC construction failed.
    CreateGic(HvfGicError),
    /// The created GIC metadata differs from the prepared graph.
    GicMetadata,
    /// Ordered never-run topology construction failed.
    Topology(HvfVcpuTopologyError),
    /// Destination topology MPIDRs differ from the prepared graph.
    TopologyIdentity,
    /// Retained effective CPU-template replay failed.
    CpuTemplate(HvfArm64CpuTemplateError),
    /// Reading one destination member's compatibility facts failed.
    CompatibilityRead(HvfVcpuRunnerError),
    /// One destination member's compatibility facts differ.
    CompatibilityMismatch,
    /// Singular VM-global GIC restoration failed.
    GlobalGic(HvfVcpuRunnerError),
    /// One complete per-vCPU restoration failed.
    Vcpu(HvfVcpuRunnerError),
    /// Fresh destination PL031 installation failed.
    Rtc(Arm64BootResourceError),
    /// A restored PVTime publisher address overflowed.
    PvTimeAddress,
    /// A restored PVTime atomic publisher could not be created.
    PvTimePublisher(GuestMemoryAccessError),
    /// Mapped guest memory became unavailable while preparing PVTime.
    PvTimeMemory(HvfGuestMemoryMappingError),
    /// One never-run vCPU rejected PVTime configuration.
    PvTime(HvfVcpuRunnerError),
    /// One destination runner was no longer available for identity restore.
    TimeIdentityRunner(HvfVcpuRunnerError),
    /// Identity interrupt signaling could not be prepared.
    TimeIdentitySignaler(HvfGicSpiSignalError),
    /// Mapped guest memory became unavailable before identity mutation.
    TimeIdentityMemory(HvfGuestMemoryMappingError),
    /// VMGenID replacement or notification failed.
    VmGenId(HvfArm64BootVmGenIdRestoreError),
    /// VMClock update or notification failed.
    VmClock(HvfArm64BootVmClockRestoreError),
    /// Stable paused lifecycle import failed and consumed the raw topology.
    Lifecycle(HvfArm64StablePausedTopologyImportError),
}

impl HvfSnapshotV2PlatformRestoreFailure {
    fn category(&self) -> &'static str {
        match self {
            Self::Allocation => "allocation",
            Self::MemoryTopology => "memory topology",
            Self::FdtAllocation => "FDT allocation",
            Self::FdtRead(_) => "FDT read",
            Self::FdtIdentity => "FDT identity",
            Self::ProcessShellFdt { .. } => "default process FDT shell",
            Self::ProcessShellInterrupt(_) => "default process interrupt shell",
            Self::ProcessShellInterruptIdentity => "default process interrupt identity",
            Self::ProcessShellSerial(_) => "default process serial shell",
            Self::CacheQuery(_) => "cache query",
            Self::CacheMismatch => "cache compatibility",
            Self::TimePreparation(_) => "time identity preparation",
            Self::PvTimeRead(_) => "PVTime preflight read",
            Self::PvTimeMismatch => "PVTime preflight agreement",
            Self::CreateVm(_) => "VM creation",
            Self::MapMemory(_) => "memory mapping",
            Self::DirtyTracking(_) => "dirty tracking",
            Self::CreateGic(_) => "GIC creation",
            Self::GicMetadata => "GIC metadata",
            Self::Topology(_) => "topology construction",
            Self::TopologyIdentity => "topology identity",
            Self::CpuTemplate(_) => "CPU-template replay",
            Self::CompatibilityRead(_) => "compatibility read",
            Self::CompatibilityMismatch => "compatibility mismatch",
            Self::GlobalGic(_) => "global GIC restore",
            Self::Vcpu(_) => "vCPU restore",
            Self::Rtc(_) => "PL031 installation",
            Self::PvTimeAddress => "PVTime publisher address",
            Self::PvTimePublisher(_) => "PVTime publisher",
            Self::PvTimeMemory(_) => "PVTime guest memory",
            Self::PvTime(_) => "PVTime restore",
            Self::TimeIdentityRunner(_) => "time identity runner preflight",
            Self::TimeIdentitySignaler(_) => "time identity signaler preflight",
            Self::TimeIdentityMemory(_) => "time identity guest memory",
            Self::VmGenId(_) => "VMGenID restore",
            Self::VmClock(_) => "VMClock restore",
            Self::Lifecycle(_) => "lifecycle import",
        }
    }
}

impl fmt::Debug for HvfSnapshotV2PlatformRestoreFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mismatch = match self {
            Self::ProcessShellFdt { mismatch } => Some(mismatch),
            _ => None,
        };
        f.debug_struct("HvfSnapshotV2PlatformRestoreFailure")
            .field("category", &self.category())
            .field("mismatch", &mismatch)
            .field("source", &REDACTED)
            .finish()
    }
}

impl fmt::Display for HvfSnapshotV2PlatformRestoreFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native-v2 platform {} failed", self.category())?;
        if let Self::ProcessShellFdt { mismatch } = self {
            write!(f, " ({mismatch})")?;
        }
        Ok(())
    }
}

impl std::error::Error for HvfSnapshotV2PlatformRestoreFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FdtRead(source) => Some(source),
            Self::ProcessShellInterrupt(source) => Some(source),
            Self::ProcessShellSerial(source) => Some(source),
            Self::CacheQuery(source) | Self::CreateVm(source) => Some(source),
            Self::TimePreparation(source) => Some(source),
            Self::PvTimeRead(source) => Some(source),
            Self::MapMemory(source) => Some(source),
            Self::DirtyTracking(source) => Some(source),
            Self::CreateGic(source) => Some(source),
            Self::Topology(source) => Some(source),
            Self::CpuTemplate(source) => Some(source),
            Self::CompatibilityRead(source) | Self::GlobalGic(source) | Self::Vcpu(source) => {
                Some(source)
            }
            Self::Rtc(source) => Some(source),
            Self::PvTimePublisher(source) => Some(source),
            Self::PvTimeMemory(source) => Some(source),
            Self::PvTime(source) | Self::TimeIdentityRunner(source) => Some(source),
            Self::TimeIdentitySignaler(source) => Some(source),
            Self::TimeIdentityMemory(source) => Some(source),
            Self::VmGenId(source) => Some(source),
            Self::VmClock(source) => Some(source),
            Self::Lifecycle(source) => Some(source),
            Self::Allocation
            | Self::MemoryTopology
            | Self::FdtAllocation
            | Self::FdtIdentity
            | Self::ProcessShellFdt { .. }
            | Self::ProcessShellInterruptIdentity
            | Self::CacheMismatch
            | Self::PvTimeMismatch
            | Self::GicMetadata
            | Self::TopologyIdentity
            | Self::CompatibilityMismatch
            | Self::PvTimeAddress => None,
        }
    }
}

/// Reverse cleanup operation attempted after reconstruction failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfSnapshotV2PlatformCleanupStage {
    /// Shut down a lifecycle session that was already born paused.
    Session,
    /// Shut down every raw vCPU owner in reverse topology order.
    Topology,
    /// Unmap guest memory and destroy the destination VM.
    Backend,
}

impl fmt::Display for HvfSnapshotV2PlatformCleanupStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session => f.write_str("paused session shutdown"),
            Self::Topology => f.write_str("topology shutdown"),
            Self::Backend => f.write_str("backend destruction"),
        }
    }
}

/// One redacted cleanup failure retained in reverse attempt order.
pub struct HvfSnapshotV2PlatformCleanupFailure {
    stage: HvfSnapshotV2PlatformCleanupStage,
    source: Box<dyn std::error::Error + Send + Sync + 'static>,
}

impl HvfSnapshotV2PlatformCleanupFailure {
    /// Return the failed cleanup operation.
    pub const fn stage(&self) -> HvfSnapshotV2PlatformCleanupStage {
        self.stage
    }

    /// Return the detailed cleanup source to trusted callers.
    pub fn source_error(&self) -> &(dyn std::error::Error + 'static) {
        self.source.as_ref()
    }
}

impl fmt::Debug for HvfSnapshotV2PlatformCleanupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2PlatformCleanupFailure")
            .field("stage", &self.stage)
            .field("source", &REDACTED)
            .finish()
    }
}

impl fmt::Display for HvfSnapshotV2PlatformCleanupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native-v2 cleanup failed at {}", self.stage)
    }
}

impl std::error::Error for HvfSnapshotV2PlatformCleanupFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.source.as_ref())
    }
}

/// Primary restore failure plus every later reverse-cleanup failure.
pub struct HvfSnapshotV2PlatformRestoreError {
    stage: HvfSnapshotV2PlatformRestoreStage,
    committed: bool,
    failure: Box<HvfSnapshotV2PlatformRestoreFailure>,
    cleanup: Vec<HvfSnapshotV2PlatformCleanupFailure>,
}

impl HvfSnapshotV2PlatformRestoreError {
    fn new(
        stage: HvfSnapshotV2PlatformRestoreStage,
        failure: HvfSnapshotV2PlatformRestoreFailure,
        cleanup: Vec<HvfSnapshotV2PlatformCleanupFailure>,
    ) -> Self {
        let committed = restore_failure_is_committed(stage, &failure);
        Self {
            stage,
            committed,
            failure: Box::new(failure),
            cleanup,
        }
    }

    /// Return the value-free primary reconstruction stage.
    pub const fn stage(&self) -> HvfSnapshotV2PlatformRestoreStage {
        self.stage
    }

    /// Return whether guest-visible clone identity had already committed.
    pub const fn is_committed(&self) -> bool {
        self.committed
    }

    /// Return the typed primary failure.
    pub fn primary_failure(&self) -> &HvfSnapshotV2PlatformRestoreFailure {
        self.failure.as_ref()
    }

    /// Return reverse cleanup failures in attempt order.
    pub fn cleanup_failures(&self) -> &[HvfSnapshotV2PlatformCleanupFailure] {
        &self.cleanup
    }
}

impl fmt::Debug for HvfSnapshotV2PlatformRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2PlatformRestoreError")
            .field("stage", &self.stage)
            .field("committed", &self.committed)
            .field("failure", &self.failure)
            .field("cleanup", &self.cleanup)
            .finish()
    }
}

impl fmt::Display for HvfSnapshotV2PlatformRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native-v2 platform restore failed at {}", self.stage)?;
        if self.committed {
            f.write_str(" after destination identity committed")?;
        }
        if !self.cleanup.is_empty() {
            write!(
                f,
                "; {} cleanup operation(s) also failed",
                self.cleanup.len()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for HvfSnapshotV2PlatformRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.failure.as_ref())
    }
}

fn restore_failure_is_committed(
    stage: HvfSnapshotV2PlatformRestoreStage,
    failure: &HvfSnapshotV2PlatformRestoreFailure,
) -> bool {
    match (stage, failure) {
        (
            HvfSnapshotV2PlatformRestoreStage::VmGenId,
            HvfSnapshotV2PlatformRestoreFailure::VmGenId(source),
        ) => source.is_committed(),
        (
            HvfSnapshotV2PlatformRestoreStage::VmClock,
            HvfSnapshotV2PlatformRestoreFailure::VmClock(_),
        )
        | (
            HvfSnapshotV2PlatformRestoreStage::Lifecycle,
            HvfSnapshotV2PlatformRestoreFailure::Lifecycle(_),
        )
        | (HvfSnapshotV2PlatformRestoreStage::Publication, _) => true,
        _ => false,
    }
}

/// Failure while explicitly shutting down a published native-v2 platform.
#[derive(Debug)]
pub enum HvfSnapshotV2PlatformShutdownError {
    /// One or more vCPU owners did not shut down.
    Vcpu(HvfVcpuRunCoordinatorError),
    /// Guest-memory teardown or VM destruction failed.
    Backend(BackendError),
    /// Both independent reverse-cleanup operations failed.
    VcpuAndBackend {
        vcpu: HvfVcpuRunCoordinatorError,
        backend: BackendError,
    },
}

impl fmt::Display for HvfSnapshotV2PlatformShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vcpu(_) => f.write_str("native-v2 vCPU topology shutdown failed"),
            Self::Backend(_) => f.write_str("native-v2 backend shutdown failed"),
            Self::VcpuAndBackend { vcpu, backend } => {
                let _sources = (vcpu, backend);
                f.write_str("native-v2 vCPU and backend shutdown both failed")
            }
        }
    }
}

impl std::error::Error for HvfSnapshotV2PlatformShutdownError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Vcpu(source) => Some(source),
            Self::Backend(source) => Some(source),
            Self::VcpuAndBackend { vcpu, .. } => Some(vcpu),
        }
    }
}

/// Focused unpublished destination whose first observable lifecycle is paused.
///
/// This owner includes the destination-owned time/identity resources required
/// before execution, but intentionally excludes general devices, public
/// actions, and path-based loading. The vCPU session is declared before the
/// backend so owners are dropped before VM memory.
pub struct RestoredHvfSnapshotV2Platform {
    parts: Option<RestoredHvfSnapshotV2PlatformParts>,
}

/// Crate-private consuming handoff into the complete product owner.
pub(crate) struct RestoredHvfSnapshotV2PlatformParts {
    pub(crate) runner: HvfArm64BootVcpuSession<'static>,
    pub(crate) backend: HvfBackend,
    pub(crate) mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
    pub(crate) memory_binding: SnapshotV2MemoryBinding,
    pub(crate) machine: HvfSnapshotV2MachineState,
    pub(crate) compatibility: HvfSnapshotV1CompatibilityState,
    pub(crate) rtc_device: Arm64BootRtcDevice,
    pub(crate) serial_device: Option<Arm64BootSerialDevice>,
    pub(crate) vmgenid_device: Arm64BootVmGenIdDevice,
    pub(crate) vmclock_device: Arm64BootVmClockDevice,
    pub(crate) pvtime_layout: Arm64PvTimeLayout,
}

impl RestoredHvfSnapshotV2Platform {
    fn parts(&self) -> &RestoredHvfSnapshotV2PlatformParts {
        match self.parts.as_ref() {
            Some(parts) => parts,
            None => std::process::abort(),
        }
    }

    fn parts_mut(&mut self) -> &mut RestoredHvfSnapshotV2PlatformParts {
        match self.parts.as_mut() {
            Some(parts) => parts,
            None => std::process::abort(),
        }
    }

    /// Return the complete destination vCPU count.
    pub fn vcpu_count(&self) -> usize {
        self.parts().runner.member_count()
    }

    /// Return owner-thread-verified canonical MPIDRs.
    pub fn vcpu_mpidrs(&self) -> &[u64] {
        self.parts().runner.mpidrs()
    }

    pub fn shared_vcpu_metrics(&self) -> SharedVcpuMetrics {
        self.parts().runner.shared_vcpu_metrics()
    }

    pub fn shared_interrupt_metrics(&self) -> SharedInterruptMetrics {
        self.parts().runner.shared_interrupt_metrics()
    }

    /// Return the retained exact memory-image binding.
    pub fn memory_binding(&self) -> &SnapshotV2MemoryBinding {
        &self.parts().memory_binding
    }

    /// Return retained logical machine, boot, FDT, and CPU-template facts.
    pub fn machine(&self) -> &HvfSnapshotV2MachineState {
        &self.parts().machine
    }

    /// Return the destination-validated common compatibility facts.
    pub fn compatibility(&self) -> &HvfSnapshotV1CompatibilityState {
        &self.parts().compatibility
    }

    /// Borrow the fresh process UART output when this owner was reconstructed
    /// through the closed process shell.
    #[doc(hidden)]
    pub fn serial_output(&self) -> Option<&SharedSerialOutput> {
        self.parts()
            .serial_device
            .as_ref()
            .map(|device| &device.output)
    }

    /// Reobserve the complete paused lifecycle graph.
    pub fn capture_stable_paused_topology(
        &mut self,
    ) -> Result<
        crate::paused_topology::HvfArm64StablePausedTopologyState,
        HvfArm64StablePausedTopologyCaptureError,
    > {
        self.parts_mut().runner.capture_stable_paused_topology()
    }

    /// Recapture the complete reconstructed platform while it remains paused.
    ///
    /// This creates a fresh binding only after PVTime publication, reuses inert
    /// machine metadata, verifies the mapped FDT identity, and reobserves every
    /// owner-thread vCPU component without opening a path.
    pub fn capture_snapshot_v2_platform<W: Write + Seek>(
        &mut self,
        memory_writer: &mut W,
    ) -> Result<HvfSnapshotV2PlatformState, HvfArm64BootSnapshotV2CaptureError> {
        let parts = self.parts_mut();
        let guest_logger = parts.backend.guest_logger();
        let topology_result = parts.runner.capture_arm64_snapshot_v2_topology();
        observe_pvtime_topology_capture(&guest_logger, &topology_result);
        let (stable, captures, pvtime_capture) = topology_result
            .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Topology { source })?;
        let memory = parts
            .backend
            .mapped_guest_memory()
            .map_err(|source| HvfArm64BootSnapshotV2CaptureError::GuestMemory { source })?;
        verify_capture_fdt_identity(memory, &parts.machine)?;
        if captures.len() != stable.members().len() {
            return Err(HvfArm64BootSnapshotV2CaptureError::CompatibilityMismatch {
                index: captures.len(),
            });
        }

        let expected_identification = parts.compatibility.identification();
        let expected_optional_identification =
            parts.compatibility.optional_sve_sme_identification();
        let mut vcpus = Vec::new();
        vcpus
            .try_reserve_exact(captures.len())
            .map_err(|_| HvfArm64BootSnapshotV2CaptureError::Allocation)?;
        let mut global_gic = None;
        for (capture, member) in captures.into_iter().zip(stable.members()) {
            let (
                identification,
                optional_identification,
                mandatory,
                timer,
                pending_interrupts,
                captured_global_gic,
                gic_icc,
                reviewed_optional,
            ) = capture.into_parts();
            if identification != identification_with_mpidr(expected_identification, member.mpidr())
                || optional_identification != expected_optional_identification
            {
                return Err(HvfArm64BootSnapshotV2CaptureError::CompatibilityMismatch {
                    index: member.index(),
                });
            }
            if (member.index() == 0) != captured_global_gic.is_some() {
                return Err(HvfArm64BootSnapshotV2CaptureError::GlobalGicShape {
                    index: member.index(),
                });
            }
            if member.index() == 0 {
                global_gic = captured_global_gic;
            }
            let index = u32::try_from(member.index()).map_err(|_| {
                HvfArm64BootSnapshotV2CaptureError::CompatibilityMismatch {
                    index: member.index(),
                }
            })?;
            let vcpu = HvfSnapshotV2VcpuState::try_new(
                index,
                member.mpidr(),
                mandatory,
                timer,
                pending_interrupts,
                gic_icc,
                reviewed_optional,
            )
            .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Build {
                stage: HvfArm64BootSnapshotV2CaptureStage::Vcpu {
                    index: member.index(),
                },
                source,
            })?;
            vcpus.push(vcpu);
        }
        let global_gic =
            global_gic.ok_or(HvfArm64BootSnapshotV2CaptureError::GlobalGicShape { index: 0 })?;
        let global = HvfSnapshotV2GlobalState::try_new(parts.compatibility.clone(), global_gic)
            .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Build {
                stage: HvfArm64BootSnapshotV2CaptureStage::GlobalGic,
                source,
            })?;
        let rtc_layout = RtcMmioLayout::new(
            parts.rtc_device.region.range().start(),
            parts.rtc_device.region.id(),
        );
        let time = capture_hvf_snapshot_v2_time_state(
            memory,
            rtc_layout,
            &parts.vmgenid_device,
            &parts.vmclock_device,
            Some(&parts.pvtime_layout),
            &pvtime_capture,
        )
        .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Time { source })?;
        let memory_binding = write_snapshot_v2_memory_image_with_compatibility_version(
            memory,
            memory_writer,
            NATIVE_V2_LEGACY_PLATFORM_VERSION,
        )
        .map_err(|source| HvfArm64BootSnapshotV2CaptureError::MemoryImage { source })?;
        HvfSnapshotV2PlatformState::try_new(
            memory_binding,
            parts.machine.clone(),
            global,
            stable,
            vcpus,
            time,
        )
        .map_err(|source| HvfArm64BootSnapshotV2CaptureError::Build {
            stage: HvfArm64BootSnapshotV2CaptureStage::Platform,
            source,
        })
    }

    /// Begin execution only after the fully restored platform was published.
    #[doc(hidden)]
    pub fn resume(&mut self) -> Result<(), HvfVcpuRunCoordinatorError> {
        self.parts_mut().runner.resume()
    }

    /// Return the post-publication topology control capability.
    #[doc(hidden)]
    pub fn control(&self) -> HvfVcpuRunControl {
        self.parts().runner.control()
    }

    /// Run one post-publication boot-session step.
    #[doc(hidden)]
    pub fn run_step(
        &mut self,
        entry_is_valid: impl FnMut(u64) -> bool,
    ) -> Result<HvfVcpuRunStepOutcome, HvfArm64BootVcpuError> {
        let result = self.parts_mut().runner.run_step(entry_is_valid);
        let logger_outcome = match &result {
            Ok(outcome) => backend_outcome_for_run_step_outcome(outcome),
            Err(error) => backend_outcome_for_run_step_error(error),
        };
        if let Some(outcome) = logger_outcome {
            self.parts().backend.guest_logger().log_backend(outcome);
        }
        result
    }

    /// Set the last stepped member's PPI pending bit.
    #[doc(hidden)]
    pub fn set_last_step_ppi_pending(&self, intid: u32) -> Result<(), HvfArm64BootVcpuError> {
        let result = self.parts().runner.set_last_step_ppi_pending(intid);
        if result.is_err() {
            self.parts()
                .backend
                .guest_logger()
                .log_backend(LoggerBackendOutcome::VirtualTimerFailed);
        }
        result
    }

    /// Borrow already-authorized destination guest memory.
    #[doc(hidden)]
    pub fn guest_memory(&self) -> Result<&GuestMemory, HvfGuestMemoryMappingError> {
        self.parts().backend.mapped_guest_memory_for_public_access()
    }

    pub(crate) fn guest_memory_mut(
        &mut self,
    ) -> Result<&mut GuestMemory, HvfGuestMemoryMappingError> {
        self.parts_mut().backend.mapped_guest_memory_mut()
    }

    pub(crate) fn backend(&self) -> &HvfBackend {
        &self.parts().backend
    }

    pub(crate) fn backend_mut(&mut self) -> &mut HvfBackend {
        &mut self.parts_mut().backend
    }

    pub(crate) fn mmio_dispatcher(&self) -> &Arc<Mutex<MmioDispatcher>> {
        &self.parts().mmio_dispatcher
    }

    /// Transfers the complete unpublished platform into a root-bearing owner.
    pub(crate) fn into_parts(mut self) -> RestoredHvfSnapshotV2PlatformParts {
        match self.parts.take() {
            Some(parts) => parts,
            None => std::process::abort(),
        }
    }

    /// Shut down vCPU owners before unmapping memory and destroying the VM.
    pub fn shutdown(&mut self) -> Result<(), HvfSnapshotV2PlatformShutdownError> {
        debug_assert_eq!(
            cleanup_sequence(RestoreOwnership::Session),
            [
                HvfSnapshotV2PlatformCleanupStage::Session,
                HvfSnapshotV2PlatformCleanupStage::Backend,
            ]
        );
        let Some(parts) = self.parts.as_mut() else {
            return Ok(());
        };
        let vcpu = parts.runner.shutdown().err();
        let backend = <HvfBackend as VmBackend>::destroy_vm(&mut parts.backend).err();
        match (vcpu, backend) {
            (None, None) => Ok(()),
            (Some(source), None) => Err(HvfSnapshotV2PlatformShutdownError::Vcpu(source)),
            (None, Some(source)) => Err(HvfSnapshotV2PlatformShutdownError::Backend(source)),
            (Some(vcpu), Some(backend)) => {
                Err(HvfSnapshotV2PlatformShutdownError::VcpuAndBackend { vcpu, backend })
            }
        }
    }
}

impl fmt::Debug for RestoredHvfSnapshotV2Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RestoredHvfSnapshotV2Platform")
            .field("vcpu_count", &self.vcpu_count())
            .field("state", &REDACTED)
            .finish()
    }
}

impl Drop for RestoredHvfSnapshotV2Platform {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

/// Proves an exact 2.4 root graph and product shell before backing or HVF access.
///
/// This boundary constructs no backend, VM, dispatcher, transport endpoint, or
/// scheduler. It integrity-binds the retained live FDT bytes but derives the
/// product shell from source-profile evidence and typed state because a booted
/// guest may already have consumed or reclaimed those bytes. The returned owner
/// still contains the inert selector solely so the destination authority layer
/// can resolve one read-only backing.
pub fn prepare_hvf_snapshot_v2_root_plan(
    state: HvfSnapshotV2State,
    memory: GuestMemory,
    process: HvfSnapshotV2RootProcessConfig,
    now: Instant,
) -> Result<PreparedHvfSnapshotV2RootPlan, PrepareHvfSnapshotV2RootPlanError> {
    let (platform, graph) = state.into_parts();
    if !memory_matches_binding(&memory, platform.memory()) {
        return Err(PrepareHvfSnapshotV2RootPlanError::MemoryTopology);
    }
    verified_fdt_bytes(&memory, platform.machine())
        .map_err(|source| PrepareHvfSnapshotV2RootPlanError::Fdt(Box::new(source)))?;
    let root = SnapshotV2RootRestorePlan::prepare(graph, &memory, now)
        .map_err(PrepareHvfSnapshotV2RootPlanError::Root)?;
    let resources = prepare_root_resource_plan(&platform, &root, process)?;
    validate_root_process_profile(&platform, &root, resources)
        .map_err(|mismatch| PrepareHvfSnapshotV2RootPlanError::ProcessFdt { mismatch })?;

    prepare_arm64_snapshot_time_identity(
        &memory,
        platform.time().vmgenid(),
        platform.time().vmclock(),
        platform.time().vmclock_abi(),
    )
    .map_err(|source| {
        PrepareHvfSnapshotV2RootPlanError::TimeIdentity(Box::new(
            HvfSnapshotV2PlatformRestoreFailure::TimePreparation(source),
        ))
    })?;
    verify_snapshot_v2_pvtime_memory(&memory, platform.time())
        .map_err(|source| PrepareHvfSnapshotV2RootPlanError::TimeIdentity(Box::new(source)))?;

    Ok(PreparedHvfSnapshotV2RootPlan {
        platform,
        memory,
        root,
        resources,
    })
}

/// Reconstruct one unpublished, initially paused native-v2 HVF platform.
///
/// `state` must already have passed native-v2 decoding/cross-validation and
/// `memory` must come from an already-authorized loader. This function opens no
/// path and performs all supplied-memory checks before creating an HVF VM.
pub fn restore_hvf_snapshot_v2_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(state, memory, None)
}

/// Reconstruct one native-v2 platform with the exact default process UART.
///
/// The retained live FDT identity, source process profile, minimal interrupt
/// sequence, and fresh output owner are validated and installed before
/// Hypervisor.framework VM construction.
pub fn restore_hvf_snapshot_v2_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2DefaultProcessShell,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::DeviceFree(shell.into())),
    )
}

/// Reconstruct one unpublished exact-2.7 serial-only process platform.
///
/// The complete restored UART is installed at the validated fixed product
/// placement while the vCPU topology remains never-run and Paused.
#[doc(hidden)]
pub fn restore_hvf_snapshot_v2_serial_only_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    process: HvfSnapshotV2SerialOnlyProcessConfig,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::SerialOnly {
            shell: shell.into(),
            process,
        }),
    )
}

/// Reconstruct one unpublished exact-2.8 serial and MMIO-entropy process
/// platform with the source interrupt-allocation order.
#[doc(hidden)]
pub(crate) fn restore_hvf_snapshot_v2_serial_entropy_mmio_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    entropy_interrupt: GuestInterruptLine,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::SerialEntropyMmio {
            shell: shell.into(),
            entropy_interrupt,
        }),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_root_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2DefaultProcessShell,
    resources: HvfSnapshotV2RootResourcePlan,
    partuuid: Option<String>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::Root {
            shell: shell.into(),
            resources,
            partuuid,
        }),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_multi_block_mmio_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2DefaultProcessShell,
    plan: HvfSnapshotV2MultiBlockMmioShellPlan<'_>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::MultiBlockMmio {
            shell: shell.into(),
            plan,
        }),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_multi_block_pci_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2DefaultProcessShell,
    plan: HvfSnapshotV2MultiBlockPciShellPlan<'_>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::MultiBlockPci {
            shell: shell.into(),
            plan,
        }),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_storage_mmio_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2DefaultProcessShell,
    plan: HvfSnapshotV2StorageMmioShellPlan<'_>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::StorageMmio {
            shell: shell.into(),
            plan,
        }),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_storage_pci_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2DefaultProcessShell,
    plan: HvfSnapshotV2StoragePciShellPlan<'_>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::StoragePci {
            shell: shell.into(),
            plan,
        }),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_serial_storage_mmio_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    plan: HvfSnapshotV2StorageMmioShellPlan<'_>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::StorageMmio {
            shell: shell.into(),
            plan,
        }),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_serial_storage_entropy_mmio_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    plan: HvfSnapshotV2StorageMmioShellPlan<'_>,
    entropy_interrupt: GuestInterruptLine,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::StorageEntropyMmio {
            shell: shell.into(),
            plan,
            entropy_interrupt,
        }),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_serial_balloon_mmio_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    plan: HvfSnapshotV2BalloonMmioShellPlan<'_>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::BalloonMmio {
            shell: shell.into(),
            plan,
        }),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_serial_storage_pci_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    plan: HvfSnapshotV2StoragePciShellPlan<'_>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::StoragePci {
            shell: shell.into(),
            plan,
        }),
    )
}

fn restore_hvf_snapshot_v2_platform_with_shell(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    process_shell: Option<HvfSnapshotV2ProcessShellRestore<'_>>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell_and_mapping(
        state,
        memory,
        process_shell,
        HvfSnapshotV2MemoryMappingRestore::Ordinary,
    )
}

pub(crate) fn restore_hvf_snapshot_v2_serial_memory_hotplug_mmio_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    plan: HvfSnapshotV2MemoryHotplugMmioShellPlan<'_>,
    mapping: &HvfSnapshotV2MemoryHotplugMappingPlan,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell_and_mapping(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::MemoryHotplugMmio {
            shell: shell.into(),
            plan,
        }),
        HvfSnapshotV2MemoryMappingRestore::MemoryHotplug(mapping),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_serial_network_mmio_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    plan: HvfSnapshotV2NetworkMmioShellPlan<'_>,
    mapping: Option<&HvfSnapshotV2MemoryHotplugMappingPlan>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    if mapping.is_some() != plan.memory_hotplug_interrupt.is_some() {
        return Err(HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
            },
            Vec::new(),
        ));
    }
    restore_hvf_snapshot_v2_platform_with_shell_and_mapping(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::NetworkMmio {
            shell: shell.into(),
            plan,
        }),
        mapping.map_or(
            HvfSnapshotV2MemoryMappingRestore::Ordinary,
            HvfSnapshotV2MemoryMappingRestore::MemoryHotplug,
        ),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_serial_network_pci_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    plan: HvfSnapshotV2NetworkPciShellPlan<'_>,
    mapping: Option<&HvfSnapshotV2MemoryHotplugMappingPlan>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    if mapping.is_some() != plan.memory_hotplug {
        return Err(HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
            },
            Vec::new(),
        ));
    }
    restore_hvf_snapshot_v2_platform_with_shell_and_mapping(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::NetworkPci {
            shell: shell.into(),
            plan,
        }),
        mapping.map_or(
            HvfSnapshotV2MemoryMappingRestore::Ordinary,
            HvfSnapshotV2MemoryMappingRestore::MemoryHotplug,
        ),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_serial_memory_hotplug_pci_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    process: HvfSnapshotV2SerialOnlyProcessConfig,
    mapping: &HvfSnapshotV2MemoryHotplugMappingPlan,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell_and_mapping(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::SerialOnly {
            shell: shell.into(),
            process,
        }),
        HvfSnapshotV2MemoryMappingRestore::MemoryHotplug(mapping),
    )
}

pub(crate) fn restore_hvf_snapshot_v2_serial_storage_memory_hotplug_pci_process_platform(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    shell: HvfSnapshotV2RestoredSerialShell,
    plan: HvfSnapshotV2StoragePciShellPlan<'_>,
    mapping: &HvfSnapshotV2MemoryHotplugMappingPlan,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    restore_hvf_snapshot_v2_platform_with_shell_and_mapping(
        state,
        memory,
        Some(HvfSnapshotV2ProcessShellRestore::StoragePci {
            shell: shell.into(),
            plan,
        }),
        HvfSnapshotV2MemoryMappingRestore::MemoryHotplug(mapping),
    )
}

#[derive(Clone, Copy)]
enum HvfSnapshotV2MemoryMappingRestore<'a> {
    Ordinary,
    MemoryHotplug(&'a HvfSnapshotV2MemoryHotplugMappingPlan),
}

fn restore_hvf_snapshot_v2_platform_with_shell_and_mapping(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    process_shell: Option<HvfSnapshotV2ProcessShellRestore<'_>>,
    mapping: HvfSnapshotV2MemoryMappingRestore<'_>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    let guest_logger = process_shell
        .as_ref()
        .map(HvfSnapshotV2ProcessShellRestore::guest_logger)
        .unwrap_or_default();
    let result = restore_hvf_snapshot_v2_platform_with_shell_and_mapping_inner(
        state,
        memory,
        process_shell,
        mapping,
    );
    guest_logger.log_time_identity(if result.is_ok() {
        LoggerTimeIdentityOutcome::PlatformPublicationSucceeded
    } else {
        LoggerTimeIdentityOutcome::PlatformPublicationFailed
    });
    result
}

fn restore_hvf_snapshot_v2_platform_with_shell_and_mapping_inner(
    state: HvfSnapshotV2PlatformState,
    memory: GuestMemory,
    process_shell: Option<HvfSnapshotV2ProcessShellRestore<'_>>,
    mapping: HvfSnapshotV2MemoryMappingRestore<'_>,
) -> Result<RestoredHvfSnapshotV2Platform, HvfSnapshotV2PlatformRestoreError> {
    let guest_logger = process_shell
        .as_ref()
        .map(HvfSnapshotV2ProcessShellRestore::guest_logger)
        .unwrap_or_default();
    debug_assert!(cleanup_sequence(RestoreOwnership::Empty).is_empty());
    let mut cleanup = Vec::new();
    if cleanup.try_reserve_exact(2).is_err() {
        return Err(HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::Allocation,
            cleanup,
        ));
    }
    let memory_matches = match mapping {
        HvfSnapshotV2MemoryMappingRestore::Ordinary => {
            memory_matches_binding(&memory, state.memory())
        }
        HvfSnapshotV2MemoryMappingRestore::MemoryHotplug(_) => {
            memory_covers_binding(&memory, state.memory())
        }
    };
    if !memory_matches {
        return Err(HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::MemoryTopology,
            cleanup,
        ));
    }
    let fdt_bytes = match verified_fdt_bytes(&memory, state.machine()) {
        Ok(bytes) => bytes,
        Err(failure) => {
            return Err(HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Preflight,
                failure,
                cleanup,
            ));
        }
    };
    let (mut dispatcher, serial_device) =
        match prepare_process_shell(process_shell, &state, &fdt_bytes) {
            Ok(prepared) => prepared,
            Err(failure) => {
                return Err(HvfSnapshotV2PlatformRestoreError::new(
                    HvfSnapshotV2PlatformRestoreStage::ProcessShell,
                    failure,
                    cleanup,
                ));
            }
        };
    let prepared_time_identity = match prepare_arm64_snapshot_time_identity(
        &memory,
        state.time().vmgenid(),
        state.time().vmclock(),
        state.time().vmclock_abi(),
    ) {
        Ok(prepared) => prepared,
        Err(source) => {
            return Err(HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Preflight,
                HvfSnapshotV2PlatformRestoreFailure::TimePreparation(source),
                cleanup,
            ));
        }
    };
    let pvtime_layout = match verify_snapshot_v2_pvtime_memory(&memory, state.time()) {
        Ok(layout) => layout,
        Err(failure) => {
            return Err(HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Preflight,
                failure,
                cleanup,
            ));
        }
    };
    let destination_cache = match HvfBackend::arm64_vcpu_cache_manifest() {
        Ok(destination_cache) => destination_cache,
        Err(source) => {
            guest_logger.log_backend(LoggerBackendOutcome::CacheConfigurationFailed);
            return Err(HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Preflight,
                HvfSnapshotV2PlatformRestoreFailure::CacheQuery(source),
                cleanup,
            ));
        }
    };
    if destination_cache != state.global().compatibility().cache_manifest() {
        return Err(HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::CacheMismatch,
            cleanup,
        ));
    }

    let (memory_binding, machine, global, stable, vcpus, time) = state.into_parts();
    let (compatibility, global_gic) = global.into_parts();
    let (mut vmgenid_device, mut vmclock_device) = prepared_time_identity.into_parts();
    let mut backend = HvfBackend::new();
    backend.attach_guest_logger(guest_logger);
    let mut topology: Option<HvfVcpuTopology<'static>> = None;

    if let Err(source) = <HvfBackend as VmBackend>::create_vm(&mut backend) {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Vm,
            HvfSnapshotV2PlatformRestoreFailure::CreateVm(source),
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }
    let mapped = match mapping {
        HvfSnapshotV2MemoryMappingRestore::Ordinary => {
            backend.map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        }
        HvfSnapshotV2MemoryMappingRestore::MemoryHotplug(plan) => {
            backend.map_snapshot_v2_memory_hotplug(memory, plan, HvfMemoryPermissions::GUEST_RAM)
        }
    };
    if let Err(source) = mapped {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Memory,
            HvfSnapshotV2PlatformRestoreFailure::MapMemory(source),
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }
    if machine.machine().track_dirty_pages()
        && let Err(source) = backend.start_dirty_write_tracking()
    {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Memory,
            HvfSnapshotV2PlatformRestoreFailure::DirtyTracking(source),
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }

    let expected_gic = compatibility.gic_metadata();
    let created_gic = match expected_gic.msi {
        Some(msi) => {
            let Some(interrupt_count) = NonZeroU32::new(msi.interrupt_range.count) else {
                return Err(failed_restore(
                    HvfSnapshotV2PlatformRestoreStage::Gic,
                    HvfSnapshotV2PlatformRestoreFailure::GicMetadata,
                    &mut topology,
                    &mut backend,
                    cleanup,
                ));
            };
            backend.create_gic_with_msi(HvfGicMsiConfiguration::new(interrupt_count))
        }
        None => backend.create_gic(),
    };
    let created_gic = match created_gic {
        Ok(metadata) => *metadata,
        Err(source) => {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Gic,
                HvfSnapshotV2PlatformRestoreFailure::CreateGic(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };
    if created_gic != expected_gic {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Gic,
            HvfSnapshotV2PlatformRestoreFailure::GicMetadata,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }

    topology = match backend.start_session_vcpu_topology(machine.machine().vcpu_count()) {
        Ok(created) => Some(created),
        Err(source) => {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::Topology(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };
    let Some(created_topology) = topology.as_ref() else {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Topology,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    };
    if created_topology.mpidrs().len() != stable.members().len()
        || created_topology
            .mpidrs()
            .iter()
            .zip(stable.members())
            .any(|(actual, expected)| *actual != expected.mpidr())
    {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::TopologyIdentity,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }
    if let Some(cpu_template) = machine.cpu_template() {
        let Some(created_topology) = topology.as_ref() else {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        };
        if let Err(source) = created_topology.apply_retained_arm64_cpu_template_state(cpu_template)
        {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::CpuTemplate,
                HvfSnapshotV2PlatformRestoreFailure::CpuTemplate(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }

    let source_identification = compatibility.identification();
    let source_optional_identification = compatibility.optional_sve_sme_identification();
    for member in stable.members() {
        let index = member.index();
        let Some(created_topology) = topology.as_ref() else {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        };
        let destination =
            created_topology.capture_arm64_snapshot_v2_destination_compatibility(index);
        let (destination_identification, destination_optional_identification) = match destination {
            Ok(destination) => destination,
            Err(source) => {
                return Err(failed_restore(
                    HvfSnapshotV2PlatformRestoreStage::Compatibility { index },
                    HvfSnapshotV2PlatformRestoreFailure::CompatibilityRead(source),
                    &mut topology,
                    &mut backend,
                    cleanup,
                ));
            }
        };
        if destination_identification
            != identification_with_mpidr(source_identification, member.mpidr())
            || destination_optional_identification != source_optional_identification
        {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Compatibility { index },
                HvfSnapshotV2PlatformRestoreFailure::CompatibilityMismatch,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }

    let Some(created_topology) = topology.as_ref() else {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Topology,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    };
    if let Err(source) = created_topology.restore_arm64_snapshot_v2_global_gic(&global_gic) {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::GlobalGic,
            HvfSnapshotV2PlatformRestoreFailure::GlobalGic(source),
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }
    for vcpu in vcpus {
        let (index, mpidr, mandatory, timer, pending_interrupts, gic_icc, reviewed_optional) =
            vcpu.into_parts();
        let Ok(index) = usize::try_from(index) else {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        };
        let restore = HvfArm64SnapshotV2VcpuRestore::new(
            identification_with_mpidr(source_identification, mpidr),
            source_optional_identification,
            mpidr,
            (
                mandatory,
                timer,
                pending_interrupts,
                gic_icc,
                reviewed_optional,
            ),
        );
        let Some(created_topology) = topology.as_ref() else {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Topology,
                HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        };
        if let Err(source) = created_topology.restore_arm64_snapshot_v2_vcpu(index, restore) {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Vcpu { index },
                HvfSnapshotV2PlatformRestoreFailure::Vcpu(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }

    let rtc_device = match register_arm64_boot_rtc_mmio(&mut dispatcher, time.rtc_layout()) {
        Ok(device) => {
            backend
                .guest_logger()
                .log_time_identity(LoggerTimeIdentityOutcome::RtcRestoreSucceeded);
            device
        }
        Err(source) => {
            backend
                .guest_logger()
                .log_time_identity(LoggerTimeIdentityOutcome::RtcRestoreFailed);
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Rtc,
                HvfSnapshotV2PlatformRestoreFailure::Rtc(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };

    let pvtime_configs = match prepare_snapshot_v2_pvtime_configs(&backend, &time) {
        Ok(configs) => configs,
        Err(failure) => {
            backend
                .guest_logger()
                .log_time_identity(LoggerTimeIdentityOutcome::PvTimeInitializationFailed);
            let (index, failure) = *failure;
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::PvTime { index },
                failure,
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };
    let Some(created_topology) = topology.as_ref() else {
        backend
            .guest_logger()
            .log_time_identity(LoggerTimeIdentityOutcome::PvTimeInitializationFailed);
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Topology,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    };
    for index in 0..pvtime_configs.len() {
        if let Err(source) = created_topology.ensure_snapshot_restore_available(index) {
            backend
                .guest_logger()
                .log_time_identity(LoggerTimeIdentityOutcome::PvTimeInitializationFailed);
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::PvTime { index },
                HvfSnapshotV2PlatformRestoreFailure::PvTime(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }
    for (index, config) in pvtime_configs.into_iter().enumerate() {
        if let Err(source) = created_topology.configure_arm64_snapshot_v2_pvtime(index, config) {
            backend
                .guest_logger()
                .log_time_identity(LoggerTimeIdentityOutcome::PvTimeInitializationFailed);
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::PvTime { index },
                HvfSnapshotV2PlatformRestoreFailure::PvTime(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    }
    backend
        .guest_logger()
        .log_time_identity(LoggerTimeIdentityOutcome::PvTimeInitializationSucceeded);

    if let Err(failure) = restore_snapshot_v2_time_identity(
        created_topology,
        &mut backend,
        expected_gic,
        &time,
        &mut vmgenid_device,
        &mut vmclock_device,
    ) {
        let (stage, failure) = *failure;
        return Err(failed_restore(
            stage,
            failure,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    }

    let dispatcher = Arc::new(Mutex::new(dispatcher));
    let Some(raw_topology) = topology.take() else {
        return Err(failed_restore(
            HvfSnapshotV2PlatformRestoreStage::Topology,
            HvfSnapshotV2PlatformRestoreFailure::TopologyIdentity,
            &mut topology,
            &mut backend,
            cleanup,
        ));
    };
    let runner = match HvfArm64BootVcpuSession::from_stable_paused_topology(
        raw_topology,
        &stable,
        Arc::clone(&dispatcher),
        stable.virtual_timer_intid(),
    ) {
        Ok(runner) => runner,
        Err(source) => {
            return Err(failed_restore(
                HvfSnapshotV2PlatformRestoreStage::Lifecycle,
                HvfSnapshotV2PlatformRestoreFailure::Lifecycle(source),
                &mut topology,
                &mut backend,
                cleanup,
            ));
        }
    };

    Ok(RestoredHvfSnapshotV2Platform {
        parts: Some(RestoredHvfSnapshotV2PlatformParts {
            runner,
            backend,
            mmio_dispatcher: dispatcher,
            memory_binding,
            machine,
            compatibility,
            rtc_device,
            serial_device,
            vmgenid_device,
            vmclock_device,
            pvtime_layout,
        }),
    })
}

fn verify_snapshot_v2_pvtime_memory(
    memory: &GuestMemory,
    time: &HvfSnapshotV2TimeState,
) -> Result<Arm64PvTimeLayout, HvfSnapshotV2PlatformRestoreFailure> {
    let arena_size = time
        .vmgenid()
        .range()
        .start()
        .raw_value()
        .checked_sub(aarch64::SYSTEM_MEM_START)
        .ok_or(HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
    let arena = GuestMemoryRange::new(GuestAddress::new(aarch64::SYSTEM_MEM_START), arena_size)
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
    let count = u8::try_from(time.pvtime_vcpus().len())
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
    let layout = Arm64PvTimeLayout::plan(count, arena)
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
    for (expected, captured) in layout.records().iter().zip(time.pvtime_vcpus()) {
        if expected.start() != captured.record_ipa() {
            return Err(HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch);
        }
        let mut bytes = [0; ARM64_PVTIME_STRUCTURE_SIZE];
        memory
            .read_slice(&mut bytes, expected.start())
            .map_err(HvfSnapshotV2PlatformRestoreFailure::PvTimeRead)?;
        let observed = Arm64PvTimeStAbi::from_bytes(bytes)
            .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch)?;
        if observed.stolen_time_ns() != captured.stolen_time_ns() {
            return Err(HvfSnapshotV2PlatformRestoreFailure::PvTimeMismatch);
        }
    }
    Ok(layout)
}

fn prepare_snapshot_v2_pvtime_configs(
    backend: &HvfBackend,
    time: &HvfSnapshotV2TimeState,
) -> Result<Vec<HvfArm64PvTimeAccountingConfig>, Box<(usize, HvfSnapshotV2PlatformRestoreFailure)>>
{
    let memory = backend.mapped_guest_memory().map_err(|source| {
        Box::new((0, HvfSnapshotV2PlatformRestoreFailure::PvTimeMemory(source)))
    })?;
    let mut configs = Vec::new();
    configs
        .try_reserve_exact(time.pvtime_vcpus().len())
        .map_err(|_| Box::new((0, HvfSnapshotV2PlatformRestoreFailure::Allocation)))?;
    for captured in time.pvtime_vcpus() {
        let index = usize::try_from(captured.index())
            .map_err(|_| Box::new((0, HvfSnapshotV2PlatformRestoreFailure::PvTimeAddress)))?;
        let address = captured
            .record_ipa()
            .checked_add(ARM64_PVTIME_STOLEN_TIME_OFFSET as u64)
            .ok_or_else(|| Box::new((index, HvfSnapshotV2PlatformRestoreFailure::PvTimeAddress)))?;
        let publisher = memory.atomic_u64(address).map_err(|source| {
            Box::new((
                index,
                HvfSnapshotV2PlatformRestoreFailure::PvTimePublisher(source),
            ))
        })?;
        configs.push(HvfArm64PvTimeAccountingConfig::new(
            captured.record_ipa().raw_value(),
            publisher,
            captured.stolen_time_ns(),
            None,
        ));
    }
    Ok(configs)
}

fn restore_snapshot_v2_time_identity(
    topology: &HvfVcpuTopology<'_>,
    backend: &mut HvfBackend,
    gic: HvfGicMetadata,
    time: &HvfSnapshotV2TimeState,
    vmgenid: &mut Arm64BootVmGenIdDevice,
    vmclock: &mut Arm64BootVmClockDevice,
) -> Result<
    (),
    Box<(
        HvfSnapshotV2PlatformRestoreStage,
        HvfSnapshotV2PlatformRestoreFailure,
    )>,
> {
    let guest_logger = backend.guest_logger();
    for index in 0..time.pvtime_vcpus().len() {
        topology
            .ensure_snapshot_restore_available(index)
            .map_err(|source| {
                guest_logger.log_time_identity(LoggerTimeIdentityOutcome::OrderedRestoreFailed);
                Box::new((
                    HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
                    HvfSnapshotV2PlatformRestoreFailure::TimeIdentityRunner(source),
                ))
            })?;
    }
    let signaler = HvfGicSpiSignaler::from_metadata(&gic).map_err(|source| {
        guest_logger.log_time_identity(LoggerTimeIdentityOutcome::OrderedRestoreFailed);
        Box::new((
            HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
            HvfSnapshotV2PlatformRestoreFailure::TimeIdentitySignaler(source),
        ))
    })?;
    for line in [
        time.vmgenid().interrupt_line(),
        time.vmclock().interrupt_line(),
    ] {
        signaler.validate_line(line).map_err(|source| {
            guest_logger.log_time_identity(LoggerTimeIdentityOutcome::OrderedRestoreFailed);
            Box::new((
                HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
                HvfSnapshotV2PlatformRestoreFailure::TimeIdentitySignaler(source),
            ))
        })?;
    }
    let memory = backend.mapped_guest_memory_mut().map_err(|source| {
        guest_logger.log_time_identity(LoggerTimeIdentityOutcome::OrderedRestoreFailed);
        Box::new((
            HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
            HvfSnapshotV2PlatformRestoreFailure::TimeIdentityMemory(source),
        ))
    })?;
    let vmgenid_result =
        replace_vmgenid_and_signal_with(memory, vmgenid, replace_arm64_boot_vmgenid, || {
            signaler.set_level(time.vmgenid().interrupt_line(), true)
        });
    observe_vmgenid_restore(&guest_logger, &vmgenid_result);
    if let Err(source) = vmgenid_result {
        guest_logger.log_time_identity(if source.is_committed() {
            LoggerTimeIdentityOutcome::OrderedRestorePartiallyCommitted
        } else {
            LoggerTimeIdentityOutcome::OrderedRestoreFailed
        });
        return Err(Box::new((
            HvfSnapshotV2PlatformRestoreStage::VmGenId,
            HvfSnapshotV2PlatformRestoreFailure::VmGenId(source),
        )));
    }
    let vmclock_result = update_vmclock_and_signal_with(
        memory,
        vmclock,
        |memory, device| device.abi.update_after_restore(memory, device.range),
        || signaler.set_level(time.vmclock().interrupt_line(), true),
    );
    observe_vmclock_restore(&guest_logger, &vmclock_result);
    if let Err(source) = vmclock_result {
        guest_logger.log_time_identity(LoggerTimeIdentityOutcome::OrderedRestorePartiallyCommitted);
        return Err(Box::new((
            HvfSnapshotV2PlatformRestoreStage::VmClock,
            HvfSnapshotV2PlatformRestoreFailure::VmClock(source),
        )));
    }
    guest_logger.log_time_identity(LoggerTimeIdentityOutcome::OrderedRestoreSucceeded);
    Ok(())
}

fn failed_restore(
    stage: HvfSnapshotV2PlatformRestoreStage,
    failure: HvfSnapshotV2PlatformRestoreFailure,
    topology: &mut Option<HvfVcpuTopology<'static>>,
    backend: &mut HvfBackend,
    mut cleanup: Vec<HvfSnapshotV2PlatformCleanupFailure>,
) -> HvfSnapshotV2PlatformRestoreError {
    let ownership = restore_ownership_before_failure(stage);
    debug_assert_eq!(
        topology.is_some(),
        matches!(ownership, RestoreOwnership::Topology)
    );
    for cleanup_stage in cleanup_sequence(ownership) {
        match cleanup_stage {
            HvfSnapshotV2PlatformCleanupStage::Topology => {
                if let Some(created) = topology.take()
                    && let Err(source) = created.shutdown()
                {
                    cleanup.push(HvfSnapshotV2PlatformCleanupFailure {
                        stage: *cleanup_stage,
                        source: Box::new(source),
                    });
                }
            }
            HvfSnapshotV2PlatformCleanupStage::Backend => {
                if let Err(source) = <HvfBackend as VmBackend>::destroy_vm(backend) {
                    cleanup.push(HvfSnapshotV2PlatformCleanupFailure {
                        stage: *cleanup_stage,
                        source: Box::new(source),
                    });
                }
            }
            HvfSnapshotV2PlatformCleanupStage::Session => {
                debug_assert!(false, "raw restore failure cannot own a published session");
            }
        }
    }
    HvfSnapshotV2PlatformRestoreError::new(stage, failure, cleanup)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestoreOwnership {
    Empty,
    Backend,
    Topology,
    Session,
}

fn cleanup_sequence(ownership: RestoreOwnership) -> &'static [HvfSnapshotV2PlatformCleanupStage] {
    match ownership {
        RestoreOwnership::Empty => &[],
        RestoreOwnership::Backend => &[HvfSnapshotV2PlatformCleanupStage::Backend],
        RestoreOwnership::Topology => &[
            HvfSnapshotV2PlatformCleanupStage::Topology,
            HvfSnapshotV2PlatformCleanupStage::Backend,
        ],
        RestoreOwnership::Session => &[
            HvfSnapshotV2PlatformCleanupStage::Session,
            HvfSnapshotV2PlatformCleanupStage::Backend,
        ],
    }
}

fn restore_ownership_before_failure(stage: HvfSnapshotV2PlatformRestoreStage) -> RestoreOwnership {
    match stage {
        HvfSnapshotV2PlatformRestoreStage::Preflight
        | HvfSnapshotV2PlatformRestoreStage::ProcessShell => RestoreOwnership::Empty,
        HvfSnapshotV2PlatformRestoreStage::Vm
        | HvfSnapshotV2PlatformRestoreStage::Memory
        | HvfSnapshotV2PlatformRestoreStage::Gic
        | HvfSnapshotV2PlatformRestoreStage::Topology
        | HvfSnapshotV2PlatformRestoreStage::Lifecycle => RestoreOwnership::Backend,
        HvfSnapshotV2PlatformRestoreStage::TopologyIdentity
        | HvfSnapshotV2PlatformRestoreStage::CpuTemplate
        | HvfSnapshotV2PlatformRestoreStage::Compatibility { .. }
        | HvfSnapshotV2PlatformRestoreStage::GlobalGic
        | HvfSnapshotV2PlatformRestoreStage::Vcpu { .. }
        | HvfSnapshotV2PlatformRestoreStage::Rtc
        | HvfSnapshotV2PlatformRestoreStage::PvTime { .. }
        | HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight
        | HvfSnapshotV2PlatformRestoreStage::VmGenId
        | HvfSnapshotV2PlatformRestoreStage::VmClock => RestoreOwnership::Topology,
        HvfSnapshotV2PlatformRestoreStage::Publication => RestoreOwnership::Session,
    }
}

fn memory_matches_binding(memory: &GuestMemory, binding: &SnapshotV2MemoryBinding) -> bool {
    memory.regions().len() == binding.extents().len()
        && memory
            .regions()
            .iter()
            .zip(binding.extents())
            .all(|(region, extent)| region.range() == extent.range())
}

fn memory_covers_binding(memory: &GuestMemory, binding: &SnapshotV2MemoryBinding) -> bool {
    let mut region_index = 0_usize;
    let mut extent_index = 0_usize;
    let mut region_cursor = memory
        .regions()
        .first()
        .map(|region| region.range().start().raw_value());
    let mut extent_cursor = binding
        .extents()
        .first()
        .map(|extent| extent.range().start().raw_value());

    loop {
        let cursor = match (region_cursor, extent_cursor) {
            (None, None) => return true,
            (Some(region), Some(extent)) if region == extent => region,
            _ => return false,
        };
        let Some(region) = memory.regions().get(region_index) else {
            return false;
        };
        let Some(extent) = binding.extents().get(extent_index) else {
            return false;
        };
        let region_end = region.range().end_exclusive().raw_value();
        let extent_end = extent.range().end_exclusive().raw_value();
        let boundary = region_end.min(extent_end);
        if boundary <= cursor {
            return false;
        }

        if boundary == region_end {
            region_index += 1;
            region_cursor = memory
                .regions()
                .get(region_index)
                .map(|region| region.range().start().raw_value());
        } else {
            region_cursor = Some(boundary);
        }
        if boundary == extent_end {
            extent_index += 1;
            extent_cursor = binding
                .extents()
                .get(extent_index)
                .map(|extent| extent.range().start().raw_value());
        } else {
            extent_cursor = Some(boundary);
        }
    }
}

fn prepare_root_resource_plan(
    state: &HvfSnapshotV2PlatformState,
    root: &SnapshotV2RootRestorePlan,
    process: HvfSnapshotV2RootProcessConfig,
) -> Result<HvfSnapshotV2RootResourcePlan, PrepareHvfSnapshotV2RootPlanError> {
    let expected_kind = if process.pci_enabled {
        SnapshotV2DeviceTransportKind::Pci
    } else {
        SnapshotV2DeviceTransportKind::Mmio
    };
    if root.transport_kind() != expected_kind
        || state.time().rtc_layout()
            != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        return Err(PrepareHvfSnapshotV2RootPlanError::TransportPolicy);
    }
    if root_queue_ranges_conflict_with_platform(state, root) {
        return Err(PrepareHvfSnapshotV2RootPlanError::ResourcePlan);
    }

    let gic = state.global().compatibility().gic_metadata();
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
        .map_err(PrepareHvfSnapshotV2RootPlanError::Interrupt)?;
    let transport = match root.transport() {
        SnapshotV2DeviceTransport::Mmio(mmio) => {
            if gic.msi.is_some() {
                return Err(PrepareHvfSnapshotV2RootPlanError::ResourcePlan);
            }
            let expected_region = MmioRegion::new(
                process.block_mmio_layout.base_region_id(),
                process.block_mmio_layout.base_address(),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .map_err(|_| PrepareHvfSnapshotV2RootPlanError::ResourcePlan)?;
            let expected_interrupt = allocator
                .allocate()
                .map_err(PrepareHvfSnapshotV2RootPlanError::Interrupt)?;
            if mmio.region() != expected_region || mmio.interrupt_line() != expected_interrupt {
                return Err(PrepareHvfSnapshotV2RootPlanError::ResourcePlan);
            }
            HvfSnapshotV2RootTransportPlan::Mmio {
                region: expected_region,
                interrupt_line: expected_interrupt,
            }
        }
        SnapshotV2DeviceTransport::Pci(pci) => {
            let Some(msi) = gic.msi else {
                return Err(PrepareHvfSnapshotV2RootPlanError::ResourcePlan);
            };
            let expected_msi = pci_root_restore_gic_msi_configuration()
                .map_err(|_| PrepareHvfSnapshotV2RootPlanError::ResourcePlan)?;
            if msi.interrupt_range.count != expected_msi.interrupt_count().get() {
                return Err(PrepareHvfSnapshotV2RootPlanError::ResourcePlan);
            }
            let expected_sbdf = PciSbdf::new(
                PCI_SEGMENT_ZERO,
                PCI_BUS_ZERO,
                PCI_FIRST_ENDPOINT_DEVICE,
                PCI_FUNCTION_ZERO,
            )
            .map_err(|_| PrepareHvfSnapshotV2RootPlanError::ResourcePlan)?;
            let address_plan = Arm64PciAddressPlan::firecracker_v1_16()
                .map_err(|_| PrepareHvfSnapshotV2RootPlanError::ResourcePlan)?;
            let expected_bar =
                GuestMemoryRange::new(address_plan.bar64().start(), VIRTIO_PCI_CAPABILITY_BAR_SIZE)
                    .map_err(|_| PrepareHvfSnapshotV2RootPlanError::ResourcePlan)?;
            let bar_region_id = pci_root_restore_bar_region_id()
                .map_err(|_| PrepareHvfSnapshotV2RootPlanError::ResourcePlan)?;
            if pci.sbdf() != expected_sbdf
                || pci.bar_index() != VIRTIO_PCI_CAPABILITY_BAR_INDEX
                || pci.bar_address_space() != PciBarAddressSpace::Memory64
                || pci.bar_prefetchable() != PciBarPrefetchable::No
                || pci.bar_range() != expected_bar
                || !pci_msix_routes_match_gic(pci.msix(), msi)
            {
                return Err(PrepareHvfSnapshotV2RootPlanError::ResourcePlan);
            }
            HvfSnapshotV2RootTransportPlan::Pci {
                sbdf: expected_sbdf,
                bar_region_id,
                bar_range: expected_bar,
                msi,
            }
        }
    };

    let serial_interrupt = allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2RootPlanError::Interrupt)?;
    let vmgenid_interrupt = allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2RootPlanError::Interrupt)?;
    let vmclock_interrupt = allocator
        .allocate()
        .map_err(PrepareHvfSnapshotV2RootPlanError::Interrupt)?;
    if state.time().vmgenid().interrupt_line() != vmgenid_interrupt
        || state.time().vmclock().interrupt_line() != vmclock_interrupt
    {
        return Err(PrepareHvfSnapshotV2RootPlanError::ResourcePlan);
    }

    Ok(HvfSnapshotV2RootResourcePlan {
        transport,
        serial_interrupt,
        vmgenid_interrupt,
        vmclock_interrupt,
    })
}

fn root_queue_ranges_conflict_with_platform(
    state: &HvfSnapshotV2PlatformState,
    root: &SnapshotV2RootRestorePlan,
) -> bool {
    snapshot_v2_queue_ranges_conflict_with_platform(state, root.queue_ranges())
}

pub(crate) fn snapshot_v2_queue_ranges_conflict_with_platform(
    state: &HvfSnapshotV2PlatformState,
    queue_ranges: Option<[GuestMemoryRange; 3]>,
) -> bool {
    let Some(queue_ranges) = queue_ranges else {
        return false;
    };
    let fdt = state.machine().fdt();
    let Ok(fdt_range) = GuestMemoryRange::new(fdt.address(), u64::from(fdt.size())) else {
        return true;
    };
    if queue_ranges.iter().any(|queue| {
        [
            fdt_range,
            state.time().vmgenid().range(),
            state.time().vmclock().range(),
        ]
        .into_iter()
        .any(|reserved| queue.overlaps(reserved))
    }) {
        return true;
    }
    let Ok(pvtime_size) = u64::try_from(ARM64_PVTIME_STRUCTURE_SIZE) else {
        return true;
    };
    state.time().pvtime_vcpus().iter().any(|record| {
        GuestMemoryRange::new(record.record_ipa(), pvtime_size).map_or(true, |reserved| {
            queue_ranges.iter().any(|queue| queue.overlaps(reserved))
        })
    })
}

pub(crate) fn pci_msix_routes_match_gic(
    state: &bangbang_runtime::snapshot_device_v2::SnapshotV2PciMsixState,
    msi: crate::gic::HvfGicMsiMetadata,
) -> bool {
    let Some(expected_address) = msi
        .region
        .base
        .checked_add(ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
    else {
        return false;
    };
    let Some(interrupt_end) = msi
        .interrupt_range
        .base
        .checked_add(msi.interrupt_range.count)
    else {
        return false;
    };
    state.entries().iter().enumerate().all(|(index, entry)| {
        let Ok(vector) = u16::try_from(index) else {
            return false;
        };
        let referenced = state.config_vector() == vector || state.queue_vectors().contains(&vector);
        let pending = state
            .pending_words()
            .get(index / u64::BITS as usize)
            .is_some_and(|word| word & (1_u64 << (index % u64::BITS as usize)) != 0);
        if entry.vector_control() & 1 != 0 || (!referenced && !pending) {
            return true;
        }
        let address = (u64::from(entry.message_address_high()) << 32)
            | u64::from(entry.message_address_low());
        address == expected_address
            && entry.message_data() >= msi.interrupt_range.base
            && entry.message_data() < interrupt_end
    })
}

fn prepare_process_shell(
    shell: Option<HvfSnapshotV2ProcessShellRestore<'_>>,
    state: &HvfSnapshotV2PlatformState,
    fdt_bytes: &[u8],
) -> Result<(MmioDispatcher, Option<Arm64BootSerialDevice>), HvfSnapshotV2PlatformRestoreFailure> {
    let mut dispatcher = MmioDispatcher::new();
    let Some(shell_restore) = shell else {
        return Ok((dispatcher, None));
    };
    dispatcher.attach_guest_logger(shell_restore.guest_logger());

    let gic = state.global().compatibility().gic_metadata();
    if state.time().rtc_layout()
        != RtcMmioLayout::new(PROCESS_RTC_MMIO_BASE, PROCESS_RTC_MMIO_REGION_ID)
    {
        return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
            mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
        });
    }
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?;
    // A booted Linux guest may reclaim its original FDT bytes. Product
    // profiles therefore authenticate the exact captured byte range and
    // product marker, then reconstruct devices from their typed restore plan;
    // only the legacy device-free profile reparses the live FDT in detail.
    let (shell, block_plan, product_process_profile, validate_detailed, planned_interrupts) =
        match shell_restore {
            HvfSnapshotV2ProcessShellRestore::DeviceFree(shell) => {
                if gic.msi.is_some() {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                (
                    shell,
                    HvfSnapshotV2ProcessBlockFdtPlan::None,
                    false,
                    true,
                    None,
                )
            }
            HvfSnapshotV2ProcessShellRestore::SerialOnly { shell, process } => {
                let policy_matches = match (process.pci_enabled(), gic.msi) {
                    (false, None) => true,
                    (true, Some(msi)) => process.exact_pci_msi_interrupt_count.map_or_else(
                        || {
                            pci_root_restore_gic_msi_configuration().is_ok_and(|expected| {
                                msi.interrupt_range.count == expected.interrupt_count().get()
                            })
                        },
                        |expected| msi.interrupt_range.count == expected,
                    ),
                    (false, Some(_)) | (true, None) => false,
                };
                if !policy_matches {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                (
                    shell,
                    HvfSnapshotV2ProcessBlockFdtPlan::None,
                    true,
                    false,
                    None,
                )
            }
            HvfSnapshotV2ProcessShellRestore::SerialEntropyMmio {
                shell,
                entropy_interrupt,
            } => {
                if gic.msi.is_some()
                    || allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != entropy_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                (
                    shell,
                    HvfSnapshotV2ProcessBlockFdtPlan::None,
                    true,
                    false,
                    None,
                )
            }
            HvfSnapshotV2ProcessShellRestore::Root {
                shell,
                resources,
                partuuid,
            } => {
                match resources.transport() {
                    HvfSnapshotV2RootTransportPlan::Mmio { interrupt_line, .. } => {
                        if gic.msi.is_some()
                            || allocator.allocate().map_err(
                                HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt,
                            )? != interrupt_line
                        {
                            return Err(
                                HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                            );
                        }
                    }
                    HvfSnapshotV2RootTransportPlan::Pci { msi, .. } => {
                        if gic.msi != Some(msi) {
                            return Err(
                                HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                            );
                        }
                    }
                }
                (
                    shell,
                    HvfSnapshotV2ProcessBlockFdtPlan::Root {
                        partuuid,
                        transport: resources.transport(),
                    },
                    true,
                    false,
                    None,
                )
            }
            HvfSnapshotV2ProcessShellRestore::MultiBlockMmio { shell, plan } => {
                if gic.msi.is_some() || plan.records.is_empty() {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                for record in plan.records {
                    if allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != record.interrupt_line()
                    {
                        return Err(
                            HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                        );
                    }
                }
                (
                    shell,
                    HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockMmio {
                        command_line: plan.command_line,
                        records: plan.records,
                    },
                    true,
                    false,
                    Some((
                        plan.serial_interrupt,
                        plan.vmgenid_interrupt,
                        plan.vmclock_interrupt,
                    )),
                )
            }
            HvfSnapshotV2ProcessShellRestore::MultiBlockPci { shell, plan } => {
                if plan.pci.records().is_empty()
                    || plan.pci.route_demand() == 0
                    || gic.msi != Some(plan.pci.msi())
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                (
                    shell,
                    HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockPci {
                        command_line: plan.command_line,
                        pci: plan.pci,
                    },
                    true,
                    false,
                    Some((
                        plan.serial_interrupt,
                        plan.vmgenid_interrupt,
                        plan.vmclock_interrupt,
                    )),
                )
            }
            HvfSnapshotV2ProcessShellRestore::StorageMmio { shell, plan } => {
                if gic.msi.is_some() || plan.block_records.len() + plan.pmem_records.len() == 0 {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                for record in plan.block_records.iter().chain(plan.pmem_records) {
                    if allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != record.interrupt_line()
                    {
                        return Err(
                            HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                        );
                    }
                }
                (
                    shell,
                    HvfSnapshotV2ProcessBlockFdtPlan::StorageMmio {
                        command_line: plan.command_line,
                        block_records: plan.block_records,
                        pmem_records: plan.pmem_records,
                    },
                    true,
                    false,
                    Some((
                        plan.serial_interrupt,
                        plan.vmgenid_interrupt,
                        plan.vmclock_interrupt,
                    )),
                )
            }
            HvfSnapshotV2ProcessShellRestore::StorageEntropyMmio {
                shell,
                plan,
                entropy_interrupt,
            } => {
                if gic.msi.is_some() || plan.block_records.len() + plan.pmem_records.len() == 0 {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                for record in plan.block_records.iter().chain(plan.pmem_records) {
                    if allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != record.interrupt_line()
                    {
                        return Err(
                            HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                        );
                    }
                }
                if allocator
                    .allocate()
                    .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                    != entropy_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                (
                    shell,
                    HvfSnapshotV2ProcessBlockFdtPlan::StorageMmio {
                        command_line: plan.command_line,
                        block_records: plan.block_records,
                        pmem_records: plan.pmem_records,
                    },
                    true,
                    false,
                    Some((
                        plan.serial_interrupt,
                        plan.vmgenid_interrupt,
                        plan.vmclock_interrupt,
                    )),
                )
            }
            HvfSnapshotV2ProcessShellRestore::BalloonMmio { shell, plan } => {
                let storage_count = plan.block_records.len() + plan.pmem_records.len();
                if gic.msi.is_some() || plan.command_line.is_some() != (storage_count != 0) {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                if allocator
                    .allocate()
                    .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                    != plan.balloon_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                for record in plan.block_records.iter().chain(plan.pmem_records) {
                    if allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != record.interrupt_line()
                    {
                        return Err(
                            HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                        );
                    }
                }
                if let Some(entropy_interrupt) = plan.entropy_interrupt
                    && allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != entropy_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                let block_plan = match plan.command_line {
                    Some(command_line) => HvfSnapshotV2ProcessBlockFdtPlan::StorageMmio {
                        command_line,
                        block_records: plan.block_records,
                        pmem_records: plan.pmem_records,
                    },
                    None => HvfSnapshotV2ProcessBlockFdtPlan::None,
                };
                (
                    shell,
                    block_plan,
                    true,
                    false,
                    Some((
                        plan.serial_interrupt,
                        plan.vmgenid_interrupt,
                        plan.vmclock_interrupt,
                    )),
                )
            }
            HvfSnapshotV2ProcessShellRestore::MemoryHotplugMmio { shell, plan } => {
                let storage_count = plan.block_records.len() + plan.pmem_records.len();
                if gic.msi.is_some() || plan.command_line.is_some() != (storage_count != 0) {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                if let Some(balloon_interrupt) = plan.balloon_interrupt
                    && allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != balloon_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                for record in plan.block_records.iter().chain(plan.pmem_records) {
                    if allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != record.interrupt_line()
                    {
                        return Err(
                            HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                        );
                    }
                }
                if let Some(entropy_interrupt) = plan.entropy_interrupt
                    && allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != entropy_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                if allocator
                    .allocate()
                    .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                    != plan.memory_hotplug_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                let block_plan = match plan.command_line {
                    Some(command_line) => HvfSnapshotV2ProcessBlockFdtPlan::StorageMmio {
                        command_line,
                        block_records: plan.block_records,
                        pmem_records: plan.pmem_records,
                    },
                    None => HvfSnapshotV2ProcessBlockFdtPlan::None,
                };
                (
                    shell,
                    block_plan,
                    true,
                    false,
                    Some((
                        plan.serial_interrupt,
                        plan.vmgenid_interrupt,
                        plan.vmclock_interrupt,
                    )),
                )
            }
            HvfSnapshotV2ProcessShellRestore::NetworkMmio { shell, plan } => {
                let storage_count = plan.block_records.len() + plan.pmem_records.len();
                if gic.msi.is_some() || plan.command_line.is_some() != (storage_count != 0) {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                if let Some(balloon_interrupt) = plan.balloon_interrupt
                    && allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != balloon_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                for record in plan.block_records {
                    if allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != record.interrupt_line()
                    {
                        return Err(
                            HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                        );
                    }
                }
                for expected in plan.network_interrupts {
                    if allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != *expected
                    {
                        return Err(
                            HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                        );
                    }
                }
                for record in plan.pmem_records {
                    if allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != record.interrupt_line()
                    {
                        return Err(
                            HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity,
                        );
                    }
                }
                if let Some(following_interrupt) = plan.following_interrupt
                    && allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != following_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                if let Some(entropy_interrupt) = plan.entropy_interrupt
                    && allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != entropy_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                if let Some(memory_hotplug_interrupt) = plan.memory_hotplug_interrupt
                    && allocator
                        .allocate()
                        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?
                        != memory_hotplug_interrupt
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
                }
                let block_plan = match plan.command_line {
                    Some(command_line) => HvfSnapshotV2ProcessBlockFdtPlan::StorageMmio {
                        command_line,
                        block_records: plan.block_records,
                        pmem_records: plan.pmem_records,
                    },
                    None => HvfSnapshotV2ProcessBlockFdtPlan::None,
                };
                (
                    shell,
                    block_plan,
                    true,
                    false,
                    Some((
                        plan.serial_interrupt,
                        plan.vmgenid_interrupt,
                        plan.vmclock_interrupt,
                    )),
                )
            }
            HvfSnapshotV2ProcessShellRestore::NetworkPci { shell, plan } => {
                let canonical_host = Arm64PciAddressPlan::firecracker_v1_16()
                    .map(Arm64FdtPciHost::from_address_plan)
                    .ok();
                if plan.endpoint_count > PCI_ENDPOINT_SLOT_COUNT
                    || (plan.endpoint_count == 0) != (plan.route_demand == 0)
                    || canonical_host != Some(plan.host)
                    || gic.msi != Some(plan.msi)
                    || !usize::try_from(plan.msi.interrupt_range.count)
                        .is_ok_and(|count| plan.route_demand <= count)
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                let block_plan = match plan.storage {
                    Some(storage)
                        if storage.pci.record_count() != 0
                            && storage.pci.route_demand() != 0
                            && storage.pci.host() == plan.host
                            && storage.pci.msi() == plan.msi
                            && storage.serial_interrupt == plan.serial_interrupt
                            && storage.vmgenid_interrupt == plan.vmgenid_interrupt
                            && storage.vmclock_interrupt == plan.vmclock_interrupt =>
                    {
                        HvfSnapshotV2ProcessBlockFdtPlan::StoragePci {
                            command_line: storage.command_line,
                            pci: storage.pci,
                        }
                    }
                    None => HvfSnapshotV2ProcessBlockFdtPlan::None,
                    Some(_) => {
                        return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                            mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                        });
                    }
                };
                (
                    shell,
                    block_plan,
                    true,
                    false,
                    Some((
                        plan.serial_interrupt,
                        plan.vmgenid_interrupt,
                        plan.vmclock_interrupt,
                    )),
                )
            }
            HvfSnapshotV2ProcessShellRestore::StoragePci { shell, plan } => {
                if plan.pci.record_count() == 0
                    || plan.pci.route_demand() == 0
                    || gic.msi != Some(plan.pci.msi())
                {
                    return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                        mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
                    });
                }
                (
                    shell,
                    HvfSnapshotV2ProcessBlockFdtPlan::StoragePci {
                        command_line: plan.command_line,
                        pci: plan.pci,
                    },
                    true,
                    false,
                    Some((
                        plan.serial_interrupt,
                        plan.vmgenid_interrupt,
                        plan.vmclock_interrupt,
                    )),
                )
            }
        };
    let serial_interrupt = allocator
        .allocate()
        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?;
    let vmgenid_interrupt = allocator
        .allocate()
        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?;
    let vmclock_interrupt = allocator
        .allocate()
        .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterrupt)?;
    if planned_interrupts.is_some_and(|expected| {
        expected != (serial_interrupt, vmgenid_interrupt, vmclock_interrupt)
    }) || state.time().vmgenid().interrupt_line() != vmgenid_interrupt
        || state.time().vmclock().interrupt_line() != vmclock_interrupt
    {
        return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity);
    }
    if product_process_profile && !state.machine().fdt().is_product_process_profile() {
        return Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
            mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
        });
    }
    if validate_detailed {
        validate_process_fdt(fdt_bytes, state, serial_interrupt, &block_plan).map_err(
            |mismatch| HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt { mismatch },
        )?;
    }

    let serial = match shell {
        HvfSnapshotV2ProcessSerialShell::Default(shell) => register_arm64_boot_serial_mmio(
            &mut dispatcher,
            Arm64BootSerialDeviceConfig::new(
                PROCESS_SERIAL_MMIO_REGION_ID,
                PROCESS_SERIAL_MMIO_BASE,
                serial_interrupt,
                shell.serial_output,
            ),
        ),
        HvfSnapshotV2ProcessSerialShell::Restored(shell) => {
            register_arm64_boot_restored_serial_mmio(
                &mut dispatcher,
                PROCESS_SERIAL_MMIO_REGION_ID,
                PROCESS_SERIAL_MMIO_BASE,
                serial_interrupt,
                shell.serial,
            )
        }
    }
    .map_err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellSerial)?;
    Ok((dispatcher, Some(serial)))
}

#[cfg(test)]
fn validate_default_process_fdt(
    bytes: &[u8],
    state: &HvfSnapshotV2PlatformState,
    serial_interrupt: GuestInterruptLine,
) -> Result<(), HvfSnapshotV2ProcessFdtMismatch> {
    validate_process_fdt(
        bytes,
        state,
        serial_interrupt,
        &HvfSnapshotV2ProcessBlockFdtPlan::None,
    )
}

#[cfg(test)]
fn validate_root_process_fdt(
    bytes: &[u8],
    state: &HvfSnapshotV2PlatformState,
    root: &SnapshotV2RootRestorePlan,
    resources: HvfSnapshotV2RootResourcePlan,
) -> Result<(), HvfSnapshotV2ProcessFdtMismatch> {
    if root.transport().kind() != resources.transport().kind() {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Root);
    }
    validate_process_fdt(
        bytes,
        state,
        resources.serial_interrupt,
        &HvfSnapshotV2ProcessBlockFdtPlan::Root {
            partuuid: root.partuuid().map(str::to_owned),
            transport: resources.transport,
        },
    )
}

fn validate_root_process_profile(
    state: &HvfSnapshotV2PlatformState,
    root: &SnapshotV2RootRestorePlan,
    resources: HvfSnapshotV2RootResourcePlan,
) -> Result<(), HvfSnapshotV2ProcessFdtMismatch> {
    if !state.machine().fdt().is_product_process_profile() {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Profile);
    }
    if root.transport().kind() != resources.transport().kind() {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Root);
    }
    canonical_process_root_block_command_line(
        state.machine().boot().boot_arguments(),
        matches!(
            resources.transport(),
            HvfSnapshotV2RootTransportPlan::Pci { .. }
        ),
        root.partuuid(),
        true,
    )
    .map_err(|_| HvfSnapshotV2ProcessFdtMismatch::Boot)?;
    Ok(())
}

fn validate_process_fdt(
    bytes: &[u8],
    state: &HvfSnapshotV2PlatformState,
    serial_interrupt: GuestInterruptLine,
    block: &HvfSnapshotV2ProcessBlockFdtPlan<'_>,
) -> Result<(), HvfSnapshotV2ProcessFdtMismatch> {
    let tree = DeviceTree::load(bytes).map_err(|_| HvfSnapshotV2ProcessFdtMismatch::Parse)?;
    let time = state.time();
    let gic = state.global().compatibility().gic_metadata();
    let serial_name =
        |name: &str| node_name_has_number(name, "uart@", 16, PROCESS_SERIAL_MMIO_BASE.raw_value());
    let rtc_name =
        |name: &str| node_name_has_number(name, "rtc@", 16, PROCESS_RTC_MMIO_BASE.raw_value());
    let vmclock_name =
        |name: &str| node_name_has_number(name, "ptp@", 10, time.vmclock().fdt_region().base);
    let block_child_count = match block {
        HvfSnapshotV2ProcessBlockFdtPlan::None => 0,
        HvfSnapshotV2ProcessBlockFdtPlan::Root { .. } => 1,
        HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockMmio { records, .. } => records.len(),
        HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockPci { .. } => 1,
        HvfSnapshotV2ProcessBlockFdtPlan::StoragePci { .. } => 1,
        HvfSnapshotV2ProcessBlockFdtPlan::StorageMmio {
            block_records,
            pmem_records,
            ..
        } => block_records.len() + pmem_records.len(),
    };
    if tree.root.children.len() != 11 + block_child_count
        || [
            "cpus",
            "memory@ram",
            "chosen",
            "intc",
            "timer",
            "apb-pclk",
            "psci",
            "vmgenid",
        ]
        .iter()
        .any(|name| child_named(&tree.root, name).is_none())
        || child_matching(&tree.root, |node| serial_name(&node.name)).is_none()
        || child_matching(&tree.root, |node| rtc_name(&node.name)).is_none()
        || child_matching(&tree.root, |node| vmclock_name(&node.name)).is_none()
        || !tree
            .root
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "linux,dummy-virt")
        || !tree
            .root
            .prop_u32("#address-cells")
            .is_ok_and(|cells| cells == 2)
        || !tree
            .root
            .prop_u32("#size-cells")
            .is_ok_and(|cells| cells == 2)
        || !tree
            .root
            .prop_u32("interrupt-parent")
            .is_ok_and(|phandle| phandle == 1)
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory);
    }

    let Some(clock) = child_named(&tree.root, "apb-pclk") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory);
    };
    let Some(psci) = child_named(&tree.root, "psci") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory);
    };
    if !clock.children.is_empty()
        || !clock
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "fixed-clock")
        || !clock.prop_u32("#clock-cells").is_ok_and(|cells| cells == 0)
        || !clock
            .prop_u32("clock-frequency")
            .is_ok_and(|frequency| frequency == 24_000_000)
        || !clock
            .prop_str("clock-output-names")
            .is_ok_and(|name| name == "clk24mhz")
        || !clock.prop_u32("phandle").is_ok_and(|phandle| phandle == 2)
        || !psci.children.is_empty()
        || !psci
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "arm,psci-0.2")
        || !psci.prop_str("method").is_ok_and(|method| method == "hvc")
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory);
    }

    let Some(cpus) = child_named(&tree.root, "cpus") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::CpuInventory);
    };
    if cpus.children.len() != state.topology().members().len()
        || cpus
            .children
            .iter()
            .zip(state.topology().members())
            .any(|(node, member)| {
                !node_name_has_number(&node.name, "cpu@", 16, member.mpidr())
                    || !property_u64_cells_equal(node, "reg", &[member.mpidr()])
                    || !node
                        .prop_str("enable-method")
                        .is_ok_and(|method| method == "psci")
            })
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::CpuInventory);
    }

    let Some(memory) = child_named(&tree.root, "memory@ram") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Memory);
    };
    if !memory_property_matches_binding(memory, state.memory()) {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Memory);
    }

    let Some(chosen) = child_named(&tree.root, "chosen") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Boot);
    };
    let boot_arguments_match = match block {
        HvfSnapshotV2ProcessBlockFdtPlan::Root {
            partuuid,
            transport,
        } => {
            let expected = canonical_process_root_block_command_line(
                state.machine().boot().boot_arguments(),
                matches!(transport, HvfSnapshotV2RootTransportPlan::Pci { .. }),
                partuuid.as_deref(),
                true,
            )
            .map_err(|_| HvfSnapshotV2ProcessFdtMismatch::Boot)?;
            chosen
                .prop_str("bootargs")
                .is_ok_and(|arguments| arguments == expected.as_str())
        }
        HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockMmio { command_line, .. } => chosen
            .prop_str("bootargs")
            .is_ok_and(|arguments| arguments == *command_line),
        HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockPci { command_line, .. } => chosen
            .prop_str("bootargs")
            .is_ok_and(|arguments| arguments == *command_line),
        HvfSnapshotV2ProcessBlockFdtPlan::StorageMmio { command_line, .. } => chosen
            .prop_str("bootargs")
            .is_ok_and(|arguments| arguments == *command_line),
        HvfSnapshotV2ProcessBlockFdtPlan::StoragePci { command_line, .. } => chosen
            .prop_str("bootargs")
            .is_ok_and(|arguments| arguments == *command_line),
        HvfSnapshotV2ProcessBlockFdtPlan::None => {
            let expected = expected_process_boot_arguments(state.machine().boot().boot_arguments())
                .ok_or(HvfSnapshotV2ProcessFdtMismatch::Boot)?;
            chosen
                .prop_str("bootargs")
                .is_ok_and(|arguments| arguments == expected.as_str())
        }
    };
    if !boot_arguments_match
        || chosen.has_prop("linux,initrd-start") != state.machine().boot().initrd_path().is_some()
        || chosen.has_prop("linux,initrd-end") != state.machine().boot().initrd_path().is_some()
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Boot);
    }

    let Some(intc) = child_named(&tree.root, "intc") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Gic);
    };
    let expected_gic_children = usize::from(matches!(
        block,
        HvfSnapshotV2ProcessBlockFdtPlan::Root {
            transport: HvfSnapshotV2RootTransportPlan::Pci { .. },
            ..
        } | HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockPci { .. }
            | HvfSnapshotV2ProcessBlockFdtPlan::StoragePci { .. }
    ));
    if intc.children.len() != expected_gic_children
        || !intc
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "arm,gic-v3")
        || !property_u64_cells_equal(
            intc,
            "reg",
            &[
                gic.distributor.base,
                gic.distributor.size,
                gic.redistributor.region.base,
                gic.redistributor.region.size,
            ],
        )
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Gic);
    }
    let Ok(timer_metadata) = gic.arm64_fdt_timer_interrupts() else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Timer);
    };
    let Some(timer) = child_named(&tree.root, "timer") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Timer);
    };
    if !property_u32_cells_equal(
        timer,
        "interrupts",
        &[
            1,
            timer_metadata.secure_physical,
            4,
            1,
            timer_metadata.non_secure_physical,
            4,
            1,
            timer_metadata.virtual_timer,
            4,
            1,
            timer_metadata.hypervisor,
            4,
        ],
    ) {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Timer);
    }

    let Some(rtc) = child_matching(&tree.root, |node| rtc_name(&node.name)) else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Rtc);
    };
    if !rtc.children.is_empty()
        || rtc.prop_raw("compatible").map(Vec::as_slice) != Some(b"arm,pl031\0arm,primecell\0")
        || !property_u64_cells_equal(
            rtc,
            "reg",
            &[
                PROCESS_RTC_MMIO_BASE.raw_value(),
                RTC_MMIO_DEVICE_WINDOW_SIZE,
            ],
        )
        || !rtc.prop_u32("clocks").is_ok_and(|phandle| phandle == 2)
        || !rtc
            .prop_str("clock-names")
            .is_ok_and(|name| name == "apb_pclk")
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Rtc);
    }

    let Some(serial) = child_matching(&tree.root, |node| serial_name(&node.name)) else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Serial);
    };
    let Some(serial_interrupt_cell) = serial_interrupt.raw_value().checked_sub(32) else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Serial);
    };
    if !serial.children.is_empty()
        || !serial
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "ns16550a")
        || !property_u64_cells_equal(
            serial,
            "reg",
            &[
                PROCESS_SERIAL_MMIO_BASE.raw_value(),
                SERIAL_MMIO_DEVICE_WINDOW_SIZE,
            ],
        )
        || !property_u32_cells_equal(serial, "interrupts", &[0, serial_interrupt_cell, 1])
        || !serial.prop_u32("clocks").is_ok_and(|phandle| phandle == 2)
        || !serial
            .prop_str("clock-names")
            .is_ok_and(|name| name == "apb_pclk")
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Serial);
    }

    let Some(vmgenid) = child_named(&tree.root, "vmgenid") else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmGenId);
    };
    let vmgenid_metadata = time.vmgenid();
    let Some(vmgenid_interrupt_cell) = vmgenid_metadata
        .interrupt_line()
        .raw_value()
        .checked_sub(32)
    else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmGenId);
    };
    if !vmgenid.children.is_empty()
        || !vmgenid
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "microsoft,vmgenid")
        || !property_u64_cells_equal(
            vmgenid,
            "reg",
            &[
                vmgenid_metadata.fdt_region().base,
                vmgenid_metadata.fdt_region().size,
            ],
        )
        || !property_u32_cells_equal(vmgenid, "interrupts", &[0, vmgenid_interrupt_cell, 1])
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmGenId);
    }

    let Some(vmclock) = child_matching(&tree.root, |node| vmclock_name(&node.name)) else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmClock);
    };
    let vmclock_metadata = time.vmclock();
    let Some(vmclock_interrupt_cell) = vmclock_metadata
        .interrupt_line()
        .raw_value()
        .checked_sub(32)
    else {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmClock);
    };
    if !vmclock.children.is_empty()
        || !vmclock
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "amazon,vmclock")
        || !property_u64_cells_equal(
            vmclock,
            "reg",
            &[
                vmclock_metadata.fdt_region().base,
                vmclock_metadata.fdt_region().size,
            ],
        )
        || !property_u32_cells_equal(vmclock, "interrupts", &[0, vmclock_interrupt_cell, 1])
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::VmClock);
    }

    validate_process_block_nodes(&tree.root, intc, block)
}

fn validate_process_block_nodes(
    root_node: &Node,
    intc: &Node,
    block: &HvfSnapshotV2ProcessBlockFdtPlan<'_>,
) -> Result<(), HvfSnapshotV2ProcessFdtMismatch> {
    let resources = match block {
        HvfSnapshotV2ProcessBlockFdtPlan::None => return Ok(()),
        HvfSnapshotV2ProcessBlockFdtPlan::Root { transport, .. } => *transport,
        HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockMmio { records, .. } => {
            for record in *records {
                validate_process_mmio_block_node(
                    root_node,
                    record.region(),
                    record.interrupt_line(),
                )?;
            }
            return Ok(());
        }
        HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockPci { pci, .. } => {
            let canonical_host = Arm64PciAddressPlan::firecracker_v1_16()
                .map(bangbang_runtime::fdt::Arm64FdtPciHost::from_address_plan)
                .ok();
            if canonical_host != Some(pci.host())
                || !validate_process_pci_host(root_node)
                || !validate_process_gic_msi(intc, pci.msi())
            {
                return Err(HvfSnapshotV2ProcessFdtMismatch::Pci);
            }
            return Ok(());
        }
        HvfSnapshotV2ProcessBlockFdtPlan::StoragePci { pci, .. } => {
            let canonical_host = Arm64PciAddressPlan::firecracker_v1_16()
                .map(bangbang_runtime::fdt::Arm64FdtPciHost::from_address_plan)
                .ok();
            if canonical_host != Some(pci.host())
                || !validate_process_pci_host(root_node)
                || !validate_process_gic_msi(intc, pci.msi())
            {
                return Err(HvfSnapshotV2ProcessFdtMismatch::Pci);
            }
            return Ok(());
        }
        HvfSnapshotV2ProcessBlockFdtPlan::StorageMmio {
            block_records,
            pmem_records,
            ..
        } => {
            for record in block_records.iter().chain(*pmem_records) {
                validate_process_mmio_block_node(
                    root_node,
                    record.region(),
                    record.interrupt_line(),
                )?;
            }
            return Ok(());
        }
    };
    match resources {
        HvfSnapshotV2RootTransportPlan::Mmio {
            region,
            interrupt_line,
        } => validate_process_mmio_block_node(root_node, region, interrupt_line),
        HvfSnapshotV2RootTransportPlan::Pci {
            sbdf: _,
            bar_region_id,
            bar_range: _,
            msi,
        } => {
            if pci_root_restore_bar_region_id().ok() != Some(bar_region_id)
                || !validate_process_pci_host(root_node)
                || !validate_process_gic_msi(intc, msi)
            {
                return Err(HvfSnapshotV2ProcessFdtMismatch::Pci);
            }
            Ok(())
        }
    }
}

fn validate_process_mmio_block_node(
    root_node: &Node,
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
) -> Result<(), HvfSnapshotV2ProcessFdtMismatch> {
    let node = child_matching(root_node, |node| {
        node_name_has_number(
            &node.name,
            "virtio_mmio@",
            16,
            region.range().start().raw_value(),
        )
    })
    .ok_or(HvfSnapshotV2ProcessFdtMismatch::Root)?;
    let interrupt_cell = interrupt_line
        .raw_value()
        .checked_sub(32)
        .ok_or(HvfSnapshotV2ProcessFdtMismatch::Root)?;
    if !node.children.is_empty()
        || !node
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "virtio,mmio")
        || !property_u64_cells_equal(
            node,
            "reg",
            &[region.range().start().raw_value(), region.range().size()],
        )
        || !property_u32_cells_equal(node, "interrupts", &[0, interrupt_cell, 1])
        || !node
            .prop_u32("interrupt-parent")
            .is_ok_and(|phandle| phandle == 1)
        || !property_is_null(node, "dma-coherent")
    {
        return Err(HvfSnapshotV2ProcessFdtMismatch::Root);
    }
    Ok(())
}

fn validate_process_gic_msi(intc: &Node, msi: crate::gic::HvfGicMsiMetadata) -> bool {
    let Some(frame) = child_matching(intc, |node| {
        node_name_has_number(&node.name, "v2m@", 16, msi.region.base)
    }) else {
        return false;
    };
    frame.children.is_empty()
        && frame
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "arm,gic-v2m-frame")
        && property_is_null(frame, "msi-controller")
        && frame.prop_u32("phandle").is_ok_and(|phandle| phandle == 3)
        && property_u64_cells_equal(frame, "reg", &[msi.region.base, msi.region.size])
}

fn validate_process_pci_host(root: &Node) -> bool {
    let Ok(plan) = Arm64PciAddressPlan::firecracker_v1_16() else {
        return false;
    };
    let Some(node) = child_matching(root, |node| {
        node_name_has_number(&node.name, "pci@", 16, plan.ecam().start().raw_value())
    }) else {
        return false;
    };
    let ranges = [
        0x0200_0000,
        high_u32(plan.bar32().start().raw_value()),
        low_u32(plan.bar32().start().raw_value()),
        high_u32(plan.bar32().start().raw_value()),
        low_u32(plan.bar32().start().raw_value()),
        high_u32(plan.bar32().size()),
        low_u32(plan.bar32().size()),
        0x0300_0000,
        high_u32(plan.bar64().start().raw_value()),
        low_u32(plan.bar64().start().raw_value()),
        high_u32(plan.bar64().start().raw_value()),
        low_u32(plan.bar64().start().raw_value()),
        high_u32(plan.bar64().size()),
        low_u32(plan.bar64().size()),
    ];
    node.children.is_empty()
        && node
            .prop_str("compatible")
            .is_ok_and(|compatible| compatible == "pci-host-ecam-generic")
        && node
            .prop_str("device_type")
            .is_ok_and(|device_type| device_type == "pci")
        && property_u32_cells_equal(node, "ranges", &ranges)
        && property_u32_cells_equal(node, "bus-range", &[0, 0])
        && node
            .prop_u32("linux,pci-domain")
            .is_ok_and(|domain| domain == 0)
        && node
            .prop_u32("#address-cells")
            .is_ok_and(|cells| cells == 3)
        && node.prop_u32("#size-cells").is_ok_and(|cells| cells == 2)
        && property_u64_cells_equal(
            node,
            "reg",
            &[plan.ecam().start().raw_value(), plan.ecam().size()],
        )
        && node
            .prop_u32("#interrupt-cells")
            .is_ok_and(|cells| cells == 1)
        && property_is_null(node, "interrupt-map")
        && property_is_null(node, "interrupt-map-mask")
        && property_is_null(node, "dma-coherent")
        && node
            .prop_u32("msi-parent")
            .is_ok_and(|phandle| phandle == 3)
}

fn property_is_null(node: &Node, name: &str) -> bool {
    node.prop_raw(name).is_some_and(Vec::is_empty)
}

const fn high_u32(value: u64) -> u32 {
    (value >> 32) as u32
}

const fn low_u32(value: u64) -> u32 {
    value as u32
}

fn child_named<'a>(node: &'a Node, name: &str) -> Option<&'a Node> {
    let mut matches = node.children.iter().filter(|child| child.name == name);
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn child_matching(node: &Node, mut predicate: impl FnMut(&Node) -> bool) -> Option<&Node> {
    let mut matches = node.children.iter().filter(|child| predicate(child));
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn node_name_has_number(name: &str, prefix: &str, radix: u32, expected: u64) -> bool {
    name.strip_prefix(prefix)
        .and_then(|value| u64::from_str_radix(value, radix).ok())
        == Some(expected)
}

fn property_u32_cells_equal(node: &Node, name: &str, expected: &[u32]) -> bool {
    let Some(raw) = node.prop_raw(name) else {
        return false;
    };
    raw.len() == expected.len().saturating_mul(4)
        && raw.chunks_exact(4).zip(expected).all(|(chunk, expected)| {
            <[u8; 4]>::try_from(chunk).map(u32::from_be_bytes).ok() == Some(*expected)
        })
}

fn property_u64_cells_equal(node: &Node, name: &str, expected: &[u64]) -> bool {
    let Some(raw) = node.prop_raw(name) else {
        return false;
    };
    raw.len() == expected.len().saturating_mul(8)
        && raw.chunks_exact(8).zip(expected).all(|(chunk, expected)| {
            <[u8; 8]>::try_from(chunk).map(u64::from_be_bytes).ok() == Some(*expected)
        })
}

fn memory_property_matches_binding(node: &Node, binding: &SnapshotV2MemoryBinding) -> bool {
    let Some(raw) = node.prop_raw("reg") else {
        return false;
    };
    if raw.len() != binding.extents().len().saturating_mul(16) {
        return false;
    }
    raw.chunks_exact(16)
        .zip(binding.extents())
        .enumerate()
        .all(|(index, (chunk, extent))| {
            let (start, size) = chunk.split_at(8);
            let range = extent.range();
            let (expected_start, expected_size) = if index == 0 {
                let Some(start) = range
                    .start()
                    .checked_add(bangbang_runtime::memory::aarch64::SYSTEM_MEM_SIZE)
                else {
                    return false;
                };
                let Some(size) = range
                    .size()
                    .checked_sub(bangbang_runtime::memory::aarch64::SYSTEM_MEM_SIZE)
                else {
                    return false;
                };
                (start.raw_value(), size)
            } else {
                (range.start().raw_value(), range.size())
            };
            <[u8; 8]>::try_from(start).map(u64::from_be_bytes).ok() == Some(expected_start)
                && <[u8; 8]>::try_from(size).map(u64::from_be_bytes).ok() == Some(expected_size)
        })
}

fn expected_process_boot_arguments(source: Option<&str>) -> Option<String> {
    let source = source.unwrap_or(bangbang_runtime::boot::DEFAULT_KERNEL_COMMAND_LINE);
    let separator = " -- ";
    let split = source.match_indices(separator).find(|(index, _)| {
        source
            .get(..*index)
            .is_some_and(|prefix| prefix.matches('"').count().is_multiple_of(2))
    });
    let (kernel, init) = match split {
        Some((index, _)) => (
            source.get(..index)?.trim(),
            source.get(index.checked_add(separator.len())?..)?.trim(),
        ),
        None => (source.trim(), ""),
    };
    if kernel.is_empty() && !init.is_empty() {
        return None;
    }
    let capacity = kernel
        .len()
        .checked_add(" pci=off".len())?
        .checked_add(if init.is_empty() {
            0
        } else {
            separator.len().checked_add(init.len())?
        })?;
    let mut expected = String::new();
    expected.try_reserve_exact(capacity).ok()?;
    expected.push_str(kernel);
    if !kernel.is_empty() {
        expected.push(' ');
    }
    expected.push_str("pci=off");
    if !init.is_empty() {
        expected.push_str(separator);
        expected.push_str(init);
    }
    Some(expected)
}

fn verified_fdt_bytes(
    memory: &GuestMemory,
    machine: &HvfSnapshotV2MachineState,
) -> Result<Vec<u8>, HvfSnapshotV2PlatformRestoreFailure> {
    let fdt = machine.fdt();
    let size = usize::try_from(fdt.size())
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::FdtAllocation)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| HvfSnapshotV2PlatformRestoreFailure::FdtAllocation)?;
    bytes.resize(size, 0);
    memory
        .read_slice(&mut bytes, fdt.address())
        .map_err(HvfSnapshotV2PlatformRestoreFailure::FdtRead)?;
    if crc64(0, &bytes) != fdt.checksum() {
        return Err(HvfSnapshotV2PlatformRestoreFailure::FdtIdentity);
    }
    Ok(bytes)
}

fn verify_capture_fdt_identity(
    memory: &GuestMemory,
    machine: &HvfSnapshotV2MachineState,
) -> Result<(), HvfArm64BootSnapshotV2CaptureError> {
    let fdt = machine.fdt();
    let size = usize::try_from(fdt.size())
        .map_err(|_| HvfArm64BootSnapshotV2CaptureError::FdtAllocation)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| HvfArm64BootSnapshotV2CaptureError::FdtAllocation)?;
    bytes.resize(size, 0);
    memory
        .read_slice(&mut bytes, fdt.address())
        .map_err(|source| HvfArm64BootSnapshotV2CaptureError::FdtRead { source })?;
    if crc64(0, &bytes) != fdt.checksum() {
        return Err(HvfArm64BootSnapshotV2CaptureError::FdtIdentityMismatch);
    }
    Ok(())
}

fn identification_with_mpidr(
    common: HvfArm64VcpuIdentificationRegisterState,
    mpidr: u64,
) -> HvfArm64VcpuIdentificationRegisterState {
    HvfArm64VcpuIdentificationRegisterState::new([
        common.midr_el1(),
        mpidr,
        common.id_aa64pfr0_el1(),
        common.id_aa64pfr1_el1(),
        common.id_aa64dfr0_el1(),
        common.id_aa64dfr1_el1(),
        common.id_aa64isar0_el1(),
        common.id_aa64isar1_el1(),
        common.id_aa64mmfr0_el1(),
        common.id_aa64mmfr1_el1(),
        common.id_aa64mmfr2_el1(),
    ])
}

#[cfg(test)]
trait RestoreProtocol {
    type Error;

    fn execute(&mut self, stage: HvfSnapshotV2PlatformRestoreStage) -> Result<(), Self::Error>;

    fn cleanup(&mut self, stage: HvfSnapshotV2PlatformCleanupStage) -> Result<(), Self::Error>;
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
struct RestoreProtocolError<E> {
    stage: HvfSnapshotV2PlatformRestoreStage,
    primary: E,
    cleanup: Vec<(HvfSnapshotV2PlatformCleanupStage, E)>,
}

#[cfg(test)]
fn run_restore_protocol<P: RestoreProtocol>(
    protocol: &mut P,
    vcpu_count: usize,
) -> Result<(), RestoreProtocolError<P::Error>> {
    let stages = restore_protocol_stages(vcpu_count);
    let mut ownership = RestoreOwnership::Empty;
    for stage in stages {
        if stage == HvfSnapshotV2PlatformRestoreStage::Vm {
            // A failed create may still require the idempotent backend guard.
            ownership = RestoreOwnership::Backend;
        }
        if stage == HvfSnapshotV2PlatformRestoreStage::Lifecycle {
            // Lifecycle import consumes the raw topology and performs its own
            // owner rollback on failure.
            ownership = RestoreOwnership::Backend;
        }
        if let Err(primary) = protocol.execute(stage) {
            debug_assert_eq!(ownership, restore_ownership_before_failure(stage));
            let mut cleanup = Vec::new();
            for cleanup_stage in cleanup_sequence(ownership) {
                if let Err(source) = protocol.cleanup(*cleanup_stage) {
                    cleanup.push((*cleanup_stage, source));
                }
            }
            return Err(RestoreProtocolError {
                stage,
                primary,
                cleanup,
            });
        }
        match stage {
            HvfSnapshotV2PlatformRestoreStage::Topology => {
                ownership = RestoreOwnership::Topology;
            }
            HvfSnapshotV2PlatformRestoreStage::TopologyIdentity => {}
            HvfSnapshotV2PlatformRestoreStage::Lifecycle => {
                ownership = RestoreOwnership::Session;
            }
            HvfSnapshotV2PlatformRestoreStage::Publication => {
                ownership = RestoreOwnership::Empty;
            }
            HvfSnapshotV2PlatformRestoreStage::Preflight
            | HvfSnapshotV2PlatformRestoreStage::ProcessShell
            | HvfSnapshotV2PlatformRestoreStage::Vm
            | HvfSnapshotV2PlatformRestoreStage::Memory
            | HvfSnapshotV2PlatformRestoreStage::Gic
            | HvfSnapshotV2PlatformRestoreStage::CpuTemplate
            | HvfSnapshotV2PlatformRestoreStage::Compatibility { .. }
            | HvfSnapshotV2PlatformRestoreStage::GlobalGic
            | HvfSnapshotV2PlatformRestoreStage::Vcpu { .. }
            | HvfSnapshotV2PlatformRestoreStage::Rtc
            | HvfSnapshotV2PlatformRestoreStage::PvTime { .. }
            | HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight
            | HvfSnapshotV2PlatformRestoreStage::VmGenId
            | HvfSnapshotV2PlatformRestoreStage::VmClock => {}
        }
    }
    Ok(())
}

#[cfg(test)]
fn restore_protocol_stages(vcpu_count: usize) -> Vec<HvfSnapshotV2PlatformRestoreStage> {
    let mut stages = vec![
        HvfSnapshotV2PlatformRestoreStage::Preflight,
        HvfSnapshotV2PlatformRestoreStage::ProcessShell,
        HvfSnapshotV2PlatformRestoreStage::Vm,
        HvfSnapshotV2PlatformRestoreStage::Memory,
        HvfSnapshotV2PlatformRestoreStage::Gic,
        HvfSnapshotV2PlatformRestoreStage::Topology,
        HvfSnapshotV2PlatformRestoreStage::TopologyIdentity,
        HvfSnapshotV2PlatformRestoreStage::CpuTemplate,
    ];
    stages.extend(
        (0..vcpu_count).map(|index| HvfSnapshotV2PlatformRestoreStage::Compatibility { index }),
    );
    stages.push(HvfSnapshotV2PlatformRestoreStage::GlobalGic);
    stages.extend((0..vcpu_count).map(|index| HvfSnapshotV2PlatformRestoreStage::Vcpu { index }));
    stages.push(HvfSnapshotV2PlatformRestoreStage::Rtc);
    stages.extend((0..vcpu_count).map(|index| HvfSnapshotV2PlatformRestoreStage::PvTime { index }));
    stages.extend([
        HvfSnapshotV2PlatformRestoreStage::TimeIdentityPreflight,
        HvfSnapshotV2PlatformRestoreStage::VmGenId,
        HvfSnapshotV2PlatformRestoreStage::VmClock,
        HvfSnapshotV2PlatformRestoreStage::Lifecycle,
        HvfSnapshotV2PlatformRestoreStage::Publication,
    ]);
    stages
}

#[cfg(test)]
pub(crate) mod tests {
    use bangbang_runtime::snapshot_memory_v2::write_snapshot_v2_memory_image;

    use super::*;

    #[test]
    fn memory_coverage_accepts_only_equivalent_extent_segmentation() {
        let page_size = 64 * 1024;
        let start = bangbang_runtime::memory::aarch64::DRAM_MEM_START;
        let first = GuestMemoryRange::new(GuestAddress::new(start), page_size)
            .expect("first range should validate");
        let second = GuestMemoryRange::new(GuestAddress::new(start + page_size), page_size)
            .expect("second range should validate");
        let split_layout = bangbang_runtime::memory::GuestMemoryLayout::new(vec![first, second])
            .expect("split layout should validate");
        let split = GuestMemory::allocate(&split_layout).expect("split memory should allocate");
        let mut image = std::io::Cursor::new(Vec::new());
        let binding = write_snapshot_v2_memory_image(&split, &mut image)
            .expect("split binding should encode");
        assert_eq!(binding.extents().len(), 2);

        let combined = GuestMemoryRange::new(GuestAddress::new(start), page_size * 2)
            .expect("combined range should validate");
        let combined_layout = bangbang_runtime::memory::GuestMemoryLayout::new(vec![combined])
            .expect("combined layout should validate");
        let combined_memory =
            GuestMemory::allocate(&combined_layout).expect("combined memory should allocate");
        assert!(!memory_matches_binding(&combined_memory, &binding));
        assert!(memory_covers_binding(&combined_memory, &binding));

        let incomplete_layout = bangbang_runtime::memory::GuestMemoryLayout::new(vec![first])
            .expect("incomplete layout should validate");
        let incomplete =
            GuestMemory::allocate(&incomplete_layout).expect("incomplete memory should allocate");
        assert!(!memory_covers_binding(&incomplete, &binding));

        let shifted = GuestMemoryRange::new(GuestAddress::new(start + page_size), page_size * 2)
            .expect("shifted range should validate");
        let shifted_layout = bangbang_runtime::memory::GuestMemoryLayout::new(vec![shifted])
            .expect("shifted layout should validate");
        let shifted_memory =
            GuestMemory::allocate(&shifted_layout).expect("shifted memory should allocate");
        assert!(!memory_covers_binding(&shifted_memory, &binding));
    }

    fn root_restore_memory() -> GuestMemory {
        let layout = bangbang_runtime::memory::GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), 0x4_0000)
                .expect("root restore memory range should validate"),
        ])
        .expect("root restore memory layout should validate");
        let mut memory =
            GuestMemory::allocate(&layout).expect("root restore memory should allocate");
        memory
            .write_slice(&8_u16.to_le_bytes(), GuestAddress::new(0x2_0002))
            .expect("root available cursor should write");
        memory
            .write_slice(&6_u16.to_le_bytes(), GuestAddress::new(0x3_0002))
            .expect("root used cursor should write");
        memory
    }

    fn readdress_root_graph(
        graph: bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceGraph,
        descriptor_table: GuestAddress,
        driver_ring: GuestAddress,
        device_ring: GuestAddress,
    ) -> bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceGraph {
        let queue = graph.record().virtio().queues()[0];
        let mut bytes = graph
            .encode(
                bangbang_runtime::snapshot_device_v2::NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            )
            .expect("root graph should encode");
        let mut old = Vec::with_capacity(24);
        for address in [
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.device_ring(),
        ] {
            old.extend_from_slice(&address.raw_value().to_le_bytes());
        }
        let offsets = bytes
            .windows(old.len())
            .enumerate()
            .filter_map(|(offset, window)| (window == old).then_some(offset))
            .collect::<Vec<_>>();
        assert_eq!(
            offsets.len(),
            1,
            "ordered queue addresses should occur exactly once"
        );
        let offset = offsets[0];
        let mut replacement = Vec::with_capacity(24);
        for address in [descriptor_table, driver_ring, device_ring] {
            replacement.extend_from_slice(&address.raw_value().to_le_bytes());
        }
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceGraph::decode(
            bangbang_runtime::snapshot_device_v2::NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .expect("readdressed root graph should decode")
    }

    fn root_restore_memory_for_platform(
        platform: &HvfSnapshotV2PlatformState,
        driver_ring: GuestAddress,
        device_ring: GuestAddress,
    ) -> GuestMemory {
        let layout = bangbang_runtime::memory::GuestMemoryLayout::new(
            platform
                .memory()
                .extents()
                .iter()
                .map(|extent| extent.range())
                .collect(),
        )
        .expect("platform restore memory layout should validate");
        let mut memory =
            GuestMemory::allocate(&layout).expect("platform restore memory should allocate");
        memory
            .write_slice(
                &8_u16.to_le_bytes(),
                driver_ring
                    .checked_add(2)
                    .expect("available cursor address should fit"),
            )
            .expect("available cursor should write");
        memory
            .write_slice(
                &6_u16.to_le_bytes(),
                device_ring
                    .checked_add(2)
                    .expect("used cursor address should fit"),
            )
            .expect("used cursor should write");
        memory
    }

    fn coherent_mmio_root_plan_fixture() -> (
        HvfSnapshotV2State,
        GuestMemory,
        HvfSnapshotV2RootProcessConfig,
    ) {
        let state = crate::snapshot_v2::tests::complete_state_fixture(
            crate::snapshot_v2::tests::MMIO_GRAPH_FIXTURE_HEX,
        );
        let (platform, graph) = state.into_parts();
        let platform = without_initrd(platform);
        let descriptor_table = GuestAddress::new(aarch64::DRAM_MEM_START + 0x30_0000);
        let driver_ring = GuestAddress::new(aarch64::DRAM_MEM_START + 0x34_0000);
        let device_ring = GuestAddress::new(aarch64::DRAM_MEM_START + 0x38_0000);
        let graph = readdress_root_graph(graph, descriptor_table, driver_ring, device_ring);
        let mut memory = root_restore_memory_for_platform(&platform, driver_ring, device_ring);
        let root = SnapshotV2RootRestorePlan::prepare(graph.clone(), &memory, Instant::now())
            .expect("coherent MMIO root graph should prepare");
        let SnapshotV2DeviceTransport::Mmio(mmio) = root.transport() else {
            panic!("fixture root should use MMIO");
        };
        let process = HvfSnapshotV2RootProcessConfig::new(
            BlockMmioLayout::new(mmio.region().range().start(), mmio.region().id()),
            false,
        );
        let resources = prepare_root_resource_plan(&platform, &root, process)
            .expect("coherent MMIO resources should prepare");

        memory
            .write_slice(
                &platform.time().vmclock_abi().to_bytes(),
                platform.time().vmclock().range().start(),
            )
            .expect("VMClock ABI should write");
        for captured in platform.time().pvtime_vcpus() {
            let mut bytes = Arm64PvTimeStAbi::initial().to_bytes();
            bytes[ARM64_PVTIME_STOLEN_TIME_OFFSET..ARM64_PVTIME_STOLEN_TIME_OFFSET + 8]
                .copy_from_slice(&captured.stolen_time_ns().to_le_bytes());
            memory
                .write_slice(&bytes, captured.record_ipa())
                .expect("PVTime ABI should write");
        }

        let command_line = canonical_process_root_block_command_line(
            platform.machine().boot().boot_arguments(),
            false,
            root.partuuid(),
            true,
        )
        .expect("coherent MMIO root command line should normalize");
        let HvfSnapshotV2RootTransportPlan::Mmio {
            region,
            interrupt_line,
        } = resources.transport()
        else {
            panic!("coherent fixture resource plan should use MMIO");
        };
        let root_device = bangbang_runtime::fdt::Arm64FdtVirtioMmioDevice {
            region: bangbang_runtime::fdt::Arm64FdtRegion {
                base: region.range().start().raw_value(),
                size: region.range().size(),
            },
            interrupt_line,
        };
        let fdt = build_process_fdt_fixture_with_profile(
            &platform,
            root_shell_devices(&platform, resources),
            &[root_device],
            &command_line,
            None,
        );
        let fdt_address = platform.machine().fdt().address();
        memory
            .write_slice(&fdt, fdt_address)
            .expect("coherent process FDT should write");

        let mut image = std::io::Cursor::new(Vec::new());
        let binding =
            bangbang_runtime::snapshot_memory_v2::write_snapshot_v2_memory_image_with_compatibility_version(
                &memory,
                &mut image,
                bangbang_runtime::snapshot_device_v2::NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            )
            .expect("coherent restore memory should encode");
        let (_old_binding, machine, global, topology, vcpus, time) = platform.into_parts();
        let fdt = crate::snapshot_v2::HvfSnapshotV2FdtState::try_new_product_process_profile(
            fdt_address,
            fdt.len(),
            crc64(0, &fdt),
        )
        .expect("coherent FDT identity should validate");
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine(),
            machine.boot().clone(),
            fdt,
            machine.cpu_template().cloned(),
        )
        .expect("coherent machine metadata should validate");
        let platform =
            HvfSnapshotV2PlatformState::try_new(binding, machine, global, topology, vcpus, time)
                .expect("coherent platform should cross-validate");
        let state = HvfSnapshotV2State::try_new(platform, graph)
            .expect("coherent exact-2.4 state should cross-validate");
        (state, memory, process)
    }

    fn without_initrd(platform: HvfSnapshotV2PlatformState) -> HvfSnapshotV2PlatformState {
        let (memory, machine, global, topology, vcpus, time) = platform.into_parts();
        let boot = crate::snapshot_v2::HvfSnapshotV2BootState::try_new(
            machine.boot().kernel_path().clone(),
            None,
            machine.boot().boot_arguments(),
        )
        .expect("root fixture boot metadata should validate");
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine(),
            boot,
            machine.fdt(),
            machine.cpu_template().cloned(),
        )
        .expect("root fixture machine metadata should validate");
        HvfSnapshotV2PlatformState::try_new(memory, machine, global, topology, vcpus, time)
            .expect("root fixture platform should cross-validate")
    }

    pub(crate) fn mmio_root_plan_fixture() -> (
        HvfSnapshotV2PlatformState,
        SnapshotV2RootRestorePlan,
        HvfSnapshotV2RootProcessConfig,
    ) {
        let state = crate::snapshot_v2::tests::complete_state_fixture(
            crate::snapshot_v2::tests::MMIO_GRAPH_FIXTURE_HEX,
        );
        let (platform, graph) = state.into_parts();
        let platform = without_initrd(platform);
        let memory = root_restore_memory();
        let root = SnapshotV2RootRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("MMIO root graph should prepare");
        let SnapshotV2DeviceTransport::Mmio(mmio) = root.transport() else {
            panic!("fixture root should use MMIO");
        };
        let process = HvfSnapshotV2RootProcessConfig::new(
            BlockMmioLayout::new(mmio.region().range().start(), mmio.region().id()),
            false,
        );
        (platform, root, process)
    }

    pub(crate) fn pci_root_plan_fixture() -> (
        HvfSnapshotV2PlatformState,
        SnapshotV2RootRestorePlan,
        HvfSnapshotV2RootProcessConfig,
    ) {
        let state = crate::snapshot_v2::tests::complete_state_fixture(
            crate::snapshot_v2::tests::PCI_GRAPH_FIXTURE_HEX,
        );
        let (platform, graph) = state.into_parts();
        let SnapshotV2DeviceTransport::Pci(pci) = graph.record().transport() else {
            panic!("fixture root should use PCI");
        };
        let message = pci.msix().entries()[0];
        let message_address = (u64::from(message.message_address_high()) << 32)
            | u64::from(message.message_address_low());
        let msi_region_base = message_address
            .checked_sub(ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
            .expect("fixture MSI region base should fit");

        let (memory, machine, global, topology, vcpus, time) = platform.into_parts();
        let (compatibility, gic_device) = global.into_parts();
        let expected_msi = pci_root_restore_gic_msi_configuration()
            .expect("PCI root MSI demand should validate")
            .interrupt_count()
            .get();
        let mut gic = compatibility.gic_metadata();
        let legacy_count = 3;
        let msi_base = gic
            .spi_interrupt_range
            .base
            .checked_add(legacy_count)
            .expect("fixture MSI interrupt base should fit");
        gic.spi_interrupt_range.count = legacy_count;
        gic.msi = Some(crate::gic::HvfGicMsiMetadata {
            region: crate::gic::HvfGicRegion {
                base: msi_region_base,
                size: 0x1_0000,
            },
            interrupt_range: crate::gic::HvfGicInterruptRange {
                base: msi_base,
                count: expected_msi,
            },
        });
        let compatibility = HvfSnapshotV1CompatibilityState::new(
            compatibility.identification(),
            compatibility.optional_sve_sme_identification(),
            compatibility.cache_manifest(),
            compatibility.primary_mpidr(),
            gic,
            compatibility.rtc_mmio_layout(),
        );
        let global = HvfSnapshotV2GlobalState::try_new(compatibility, gic_device)
            .expect("PCI root global state should validate");

        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
            .expect("PCI root legacy interrupt allocator should validate");
        let _serial = allocator
            .allocate()
            .expect("PCI root serial interrupt should allocate");
        let vmgenid_interrupt = allocator
            .allocate()
            .expect("PCI root VMGenID interrupt should allocate");
        let vmclock_interrupt = allocator
            .allocate()
            .expect("PCI root VMClock interrupt should allocate");
        let (rtc, vmgenid, vmclock, vmclock_abi, pvtime) = time.into_parts();
        let vmgenid = bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata::new(
            vmgenid.range(),
            vmgenid.fdt_region(),
            vmgenid_interrupt,
        );
        let vmclock = bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata::new(
            vmclock.range(),
            vmclock.fdt_region(),
            vmclock_interrupt,
        );
        let time = HvfSnapshotV2TimeState::try_new(rtc, vmgenid, vmclock, vmclock_abi, pvtime)
            .expect("PCI root time metadata should validate");
        let platform =
            HvfSnapshotV2PlatformState::try_new(memory, machine, global, topology, vcpus, time)
                .expect("PCI root platform should cross-validate");
        let platform = without_initrd(platform);
        let root =
            SnapshotV2RootRestorePlan::prepare(graph, &root_restore_memory(), Instant::now())
                .expect("PCI root graph should prepare");
        let process = HvfSnapshotV2RootProcessConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0xd000_0000), MmioRegionId::new(9)),
            true,
        );
        (platform, root, process)
    }

    pub(crate) fn process_platform_fixture() -> HvfSnapshotV2PlatformState {
        let state = crate::snapshot_v2::tests::platform_fixture(false);
        let gic = state.global().compatibility().gic_metadata();
        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
            .expect("fixture GIC should provide an interrupt allocator");
        let _serial_interrupt = allocator
            .allocate()
            .expect("fixture serial interrupt should allocate");
        let vmgenid_interrupt = allocator
            .allocate()
            .expect("fixture VMGenID interrupt should allocate");
        let vmclock_interrupt = allocator
            .allocate()
            .expect("fixture VMClock interrupt should allocate");
        let (memory, machine, global, topology, vcpus, time) = state.into_parts();
        let boot = crate::snapshot_v2::HvfSnapshotV2BootState::try_new(
            machine.boot().kernel_path().clone(),
            None,
            machine.boot().boot_arguments(),
        )
        .expect("process fixture boot metadata should validate");
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine(),
            boot,
            machine.fdt(),
            machine.cpu_template().cloned(),
        )
        .expect("process fixture machine metadata should validate");
        let (rtc, vmgenid, vmclock, vmclock_abi, pvtime) = time.into_parts();
        let vmgenid = bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata::new(
            vmgenid.range(),
            vmgenid.fdt_region(),
            vmgenid_interrupt,
        );
        let vmclock = bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata::new(
            vmclock.range(),
            vmclock.fdt_region(),
            vmclock_interrupt,
        );
        let time = HvfSnapshotV2TimeState::try_new(rtc, vmgenid, vmclock, vmclock_abi, pvtime)
            .expect("process fixture time metadata should validate");
        HvfSnapshotV2PlatformState::try_new(memory, machine, global, topology, vcpus, time)
            .expect("process fixture platform should cross-validate")
    }

    fn product_process_platform_fixture() -> HvfSnapshotV2PlatformState {
        let state = process_platform_fixture();
        let layout = bangbang_runtime::memory::GuestMemoryLayout::new(
            state
                .memory()
                .extents()
                .iter()
                .map(|extent| extent.range())
                .collect(),
        )
        .expect("product process memory layout should validate");
        let memory =
            GuestMemory::allocate(&layout).expect("product process memory should allocate");
        let mut image = std::io::Cursor::new(Vec::new());
        let binding =
            bangbang_runtime::snapshot_memory_v2::write_snapshot_v2_memory_image_with_compatibility_version(
                &memory,
                &mut image,
                bangbang_runtime::snapshot_serial_v2_7::NATIVE_V2_SERIAL_STATE_COMPATIBILITY_VERSION,
            )
            .expect("exact-2.7 memory binding should encode");
        let (_old_binding, machine, global, topology, vcpus, time) = state.into_parts();
        let source_fdt = machine.fdt();
        let fdt = crate::snapshot_v2::HvfSnapshotV2FdtState::try_new_product_process_profile(
            source_fdt.address(),
            usize::try_from(source_fdt.size()).expect("FDT size should fit usize"),
            source_fdt.checksum(),
        )
        .expect("product process FDT marker should validate");
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine(),
            machine.boot().clone(),
            fdt,
            machine.cpu_template().cloned(),
        )
        .expect("product process machine should validate");
        HvfSnapshotV2PlatformState::try_new(binding, machine, global, topology, vcpus, time)
            .expect("product process platform should cross-validate")
    }

    pub(crate) fn product_entropy_interrupt_platform_fixture(
        state: HvfSnapshotV2PlatformState,
        prefix: &[GuestInterruptLine],
    ) -> (
        HvfSnapshotV2PlatformState,
        GuestInterruptLine,
        GuestInterruptLine,
        GuestInterruptLine,
        GuestInterruptLine,
    ) {
        assert!(state.machine().fdt().is_product_process_profile());
        let gic = state.global().compatibility().gic_metadata();
        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
            .expect("entropy fixture GIC should provide an interrupt allocator");
        for expected in prefix {
            assert_eq!(
                allocator
                    .allocate()
                    .expect("prefix interrupt should allocate"),
                *expected
            );
        }
        let entropy_interrupt = allocator
            .allocate()
            .expect("entropy interrupt should allocate");
        let serial_interrupt = allocator
            .allocate()
            .expect("serial interrupt should allocate");
        let vmgenid_interrupt = allocator
            .allocate()
            .expect("VMGenID interrupt should allocate");
        let vmclock_interrupt = allocator
            .allocate()
            .expect("VMClock interrupt should allocate");

        let layout = bangbang_runtime::memory::GuestMemoryLayout::new(
            state
                .memory()
                .extents()
                .iter()
                .map(|extent| extent.range())
                .collect(),
        )
        .expect("entropy process memory layout should validate");
        let memory =
            GuestMemory::allocate(&layout).expect("entropy process memory should allocate");
        let mut image = std::io::Cursor::new(Vec::new());
        let binding =
            bangbang_runtime::snapshot_memory_v2::write_snapshot_v2_memory_image_with_compatibility_version(
                &memory,
                &mut image,
                bangbang_runtime::snapshot_entropy_v2_8::NATIVE_V2_ENTROPY_STATE_COMPATIBILITY_VERSION,
            )
            .expect("exact-2.8 memory binding should encode");
        let (_old_binding, machine, global, topology, vcpus, time) = state.into_parts();
        let (rtc, vmgenid, vmclock, vmclock_abi, pvtime) = time.into_parts();
        let vmgenid = bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata::new(
            vmgenid.range(),
            vmgenid.fdt_region(),
            vmgenid_interrupt,
        );
        let vmclock = bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata::new(
            vmclock.range(),
            vmclock.fdt_region(),
            vmclock_interrupt,
        );
        let time = HvfSnapshotV2TimeState::try_new(rtc, vmgenid, vmclock, vmclock_abi, pvtime)
            .expect("entropy process time metadata should validate");
        let state =
            HvfSnapshotV2PlatformState::try_new(binding, machine, global, topology, vcpus, time)
                .expect("entropy process platform should cross-validate");
        (
            state,
            entropy_interrupt,
            serial_interrupt,
            vmgenid_interrupt,
            vmclock_interrupt,
        )
    }

    fn restored_serial_shell() -> HvfSnapshotV2RestoredSerialShell {
        let serial = SerialMmioDevice::with_shared_output(SharedSerialOutput::new(
            bangbang_runtime::serial::SharedSerialOutputBuffer::default(),
        ));
        HvfSnapshotV2RestoredSerialShell::new(serial)
    }

    fn fixture_shell_devices(
        state: &HvfSnapshotV2PlatformState,
    ) -> (
        bangbang_runtime::fdt::Arm64FdtSerialDevice,
        bangbang_runtime::fdt::Arm64FdtRtcDevice,
        bangbang_runtime::fdt::Arm64FdtVmGenIdDevice,
        bangbang_runtime::fdt::Arm64FdtVmClockDevice,
    ) {
        let gic = state.global().compatibility().gic_metadata();
        let mut allocator = HvfGicInterruptLineAllocator::from_metadata(&gic)
            .expect("fixture GIC should provide an interrupt allocator");
        let serial_interrupt = allocator
            .allocate()
            .expect("fixture serial interrupt should allocate");
        let expected_vmgenid = allocator
            .allocate()
            .expect("fixture VMGenID interrupt should allocate");
        let expected_vmclock = allocator
            .allocate()
            .expect("fixture VMClock interrupt should allocate");
        assert_eq!(state.time().vmgenid().interrupt_line(), expected_vmgenid);
        assert_eq!(state.time().vmclock().interrupt_line(), expected_vmclock);
        let rtc_layout = state.time().rtc_layout();
        (
            bangbang_runtime::fdt::Arm64FdtSerialDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_SERIAL_MMIO_BASE.raw_value(),
                    size: SERIAL_MMIO_DEVICE_WINDOW_SIZE,
                },
                interrupt_line: serial_interrupt,
            },
            bangbang_runtime::fdt::Arm64FdtRtcDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: rtc_layout.base().raw_value(),
                    size: RTC_MMIO_DEVICE_WINDOW_SIZE,
                },
            },
            bangbang_runtime::fdt::Arm64FdtVmGenIdDevice {
                region: state.time().vmgenid().fdt_region(),
                interrupt_line: state.time().vmgenid().interrupt_line(),
            },
            bangbang_runtime::fdt::Arm64FdtVmClockDevice {
                region: state.time().vmclock().fdt_region(),
                interrupt_line: state.time().vmclock().interrupt_line(),
            },
        )
    }

    fn root_shell_devices(
        state: &HvfSnapshotV2PlatformState,
        resources: HvfSnapshotV2RootResourcePlan,
    ) -> (
        bangbang_runtime::fdt::Arm64FdtSerialDevice,
        bangbang_runtime::fdt::Arm64FdtRtcDevice,
        bangbang_runtime::fdt::Arm64FdtVmGenIdDevice,
        bangbang_runtime::fdt::Arm64FdtVmClockDevice,
    ) {
        (
            bangbang_runtime::fdt::Arm64FdtSerialDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_SERIAL_MMIO_BASE.raw_value(),
                    size: SERIAL_MMIO_DEVICE_WINDOW_SIZE,
                },
                interrupt_line: resources.serial_interrupt(),
            },
            bangbang_runtime::fdt::Arm64FdtRtcDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_RTC_MMIO_BASE.raw_value(),
                    size: RTC_MMIO_DEVICE_WINDOW_SIZE,
                },
            },
            bangbang_runtime::fdt::Arm64FdtVmGenIdDevice {
                region: state.time().vmgenid().fdt_region(),
                interrupt_line: resources.vmgenid_interrupt(),
            },
            bangbang_runtime::fdt::Arm64FdtVmClockDevice {
                region: state.time().vmclock().fdt_region(),
                interrupt_line: resources.vmclock_interrupt(),
            },
        )
    }

    fn build_process_fdt_fixture(
        state: &HvfSnapshotV2PlatformState,
        serial: bangbang_runtime::fdt::Arm64FdtSerialDevice,
        rtc: bangbang_runtime::fdt::Arm64FdtRtcDevice,
        vmgenid: bangbang_runtime::fdt::Arm64FdtVmGenIdDevice,
        vmclock: bangbang_runtime::fdt::Arm64FdtVmClockDevice,
        optional_devices: &[bangbang_runtime::fdt::Arm64FdtVirtioMmioDevice],
    ) -> Vec<u8> {
        let command_line = expected_process_boot_arguments(state.machine().boot().boot_arguments())
            .expect("fixture command line should normalize");
        build_process_fdt_fixture_with_profile(
            state,
            (serial, rtc, vmgenid, vmclock),
            optional_devices,
            &command_line,
            None,
        )
    }

    fn build_process_fdt_fixture_with_profile(
        state: &HvfSnapshotV2PlatformState,
        shell: (
            bangbang_runtime::fdt::Arm64FdtSerialDevice,
            bangbang_runtime::fdt::Arm64FdtRtcDevice,
            bangbang_runtime::fdt::Arm64FdtVmGenIdDevice,
            bangbang_runtime::fdt::Arm64FdtVmClockDevice,
        ),
        optional_devices: &[bangbang_runtime::fdt::Arm64FdtVirtioMmioDevice],
        command_line: &str,
        pci: Option<bangbang_runtime::fdt::Arm64FdtPciHost>,
    ) -> Vec<u8> {
        let (serial, rtc, vmgenid, vmclock) = shell;
        let ranges = state
            .memory()
            .extents()
            .iter()
            .map(|extent| extent.range())
            .collect();
        let layout = bangbang_runtime::memory::GuestMemoryLayout::new(ranges)
            .expect("fixture memory layout should validate");
        let cache = bangbang_runtime::fdt::Arm64FdtCache::new(
            1,
            bangbang_runtime::fdt::Arm64FdtCacheType::Unified,
            32_768,
            64,
            64,
            8,
            1,
        )
        .expect("fixture cache geometry should validate");
        let cache_hierarchy = bangbang_runtime::fdt::Arm64FdtCacheHierarchy::new(vec![cache])
            .expect("fixture cache hierarchy should validate");
        let mpidrs = state
            .topology()
            .members()
            .iter()
            .map(|member| member.mpidr())
            .collect::<Vec<_>>();
        let initrd = state.machine().boot().initrd_path().map(|_| {
            let address = state.memory().extents()[0]
                .range()
                .start()
                .checked_add(aarch64::SYSTEM_MEM_SIZE + 0x1_0000)
                .expect("fixture initrd address should fit");
            bangbang_runtime::boot::LoadedInitrd {
                address,
                size: 4096,
            }
        });
        let gic = state.global().compatibility().gic_metadata();
        let config = bangbang_runtime::fdt::Arm64FdtConfig {
            layout: &layout,
            boot: bangbang_runtime::fdt::Arm64FdtBootInfo {
                command_line,
                initrd,
            },
            vcpu_mpidrs: &mpidrs,
            cache_hierarchy: &cache_hierarchy,
            gic: gic.arm64_fdt_gic(),
            timer: gic
                .arm64_fdt_timer_interrupts()
                .expect("fixture timer metadata should validate"),
            rtc_device: Some(rtc),
            serial_device: Some(serial),
            vmgenid_device: Some(vmgenid),
            vmclock_device: Some(vmclock),
            virtio_mmio_devices: optional_devices,
        };
        match pci {
            Some(pci) => bangbang_runtime::fdt::build_arm64_fdt_with_pci(&config, pci),
            None => bangbang_runtime::fdt::build_arm64_fdt(&config),
        }
        .expect("fixture FDT should build")
    }

    #[test]
    fn mmio_root_resource_plan_matches_fresh_product_order() {
        let (platform, root, process) = mmio_root_plan_fixture();
        let resources = prepare_root_resource_plan(&platform, &root, process)
            .expect("MMIO root resources should match product order");
        let SnapshotV2DeviceTransport::Mmio(graph) = root.transport() else {
            panic!("fixture root should use MMIO");
        };
        assert_eq!(
            resources.transport(),
            HvfSnapshotV2RootTransportPlan::Mmio {
                region: graph.region(),
                interrupt_line: graph.interrupt_line(),
            }
        );
        assert_eq!(
            resources.vmgenid_interrupt(),
            platform.time().vmgenid().interrupt_line()
        );
        assert_eq!(
            resources.vmclock_interrupt(),
            platform.time().vmclock().interrupt_line()
        );

        let wrong_policy = HvfSnapshotV2RootProcessConfig::new(process.block_mmio_layout(), true);
        assert!(matches!(
            prepare_root_resource_plan(&platform, &root, wrong_policy),
            Err(PrepareHvfSnapshotV2RootPlanError::TransportPolicy)
        ));
        let wrong_layout = HvfSnapshotV2RootProcessConfig::new(
            BlockMmioLayout::new(
                process
                    .block_mmio_layout()
                    .base_address()
                    .checked_add(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
                    .expect("wrong layout base should fit"),
                process.block_mmio_layout().base_region_id(),
            ),
            false,
        );
        assert!(matches!(
            prepare_root_resource_plan(&platform, &root, wrong_layout),
            Err(PrepareHvfSnapshotV2RootPlanError::ResourcePlan)
        ));
    }

    #[test]
    fn root_resource_plan_rejects_queue_platform_collision() {
        let state = crate::snapshot_v2::tests::complete_state_fixture(
            crate::snapshot_v2::tests::MMIO_GRAPH_FIXTURE_HEX,
        );
        let (platform, graph) = state.into_parts();
        let descriptor_table = platform.time().vmgenid().range().start();
        let driver_ring = GuestAddress::new(aarch64::DRAM_MEM_START + 0x28_0000);
        let device_ring = GuestAddress::new(aarch64::DRAM_MEM_START + 0x30_0000);
        let graph = readdress_root_graph(graph, descriptor_table, driver_ring, device_ring);
        let memory = root_restore_memory_for_platform(&platform, driver_ring, device_ring);
        let root = SnapshotV2RootRestorePlan::prepare(graph, &memory, Instant::now())
            .expect("colliding graph should pass device-local memory checks");
        let SnapshotV2DeviceTransport::Mmio(mmio) = root.transport() else {
            panic!("fixture root should use MMIO");
        };
        let process = HvfSnapshotV2RootProcessConfig::new(
            BlockMmioLayout::new(mmio.region().range().start(), mmio.region().id()),
            false,
        );

        assert!(matches!(
            prepare_root_resource_plan(&platform, &root, process),
            Err(PrepareHvfSnapshotV2RootPlanError::ResourcePlan)
        ));
    }

    #[test]
    fn exact_root_preparation_accepts_one_coherent_pathless_mmio_plan() {
        let (state, memory, process) = coherent_mmio_root_plan_fixture();
        let prepared = prepare_hvf_snapshot_v2_root_plan(state, memory, process, Instant::now())
            .expect("coherent exact-2.4 root plan should prepare");

        assert_eq!(
            prepared.resources().transport().kind(),
            SnapshotV2DeviceTransportKind::Mmio
        );
        assert_eq!(prepared.root().drive_id(), "rootfs");
        assert_eq!(prepared.selector(), "root-selector");
        assert!(!format!("{prepared:?}").contains(prepared.selector()));
    }

    #[test]
    fn exact_root_preparation_integrity_binds_guest_consumed_fdt_bytes() {
        let (state, mut memory, process) = coherent_mmio_root_plan_fixture();
        let (platform, graph) = state.into_parts();
        let fdt = platform.machine().fdt();
        let consumed = vec![0xa5; usize::try_from(fdt.size()).expect("FDT size should fit")];
        memory
            .write_slice(&consumed, fdt.address())
            .expect("guest-consumed FDT bytes should write");
        let (binding, machine, global, topology, vcpus, time) = platform.into_parts();
        let fdt = crate::snapshot_v2::HvfSnapshotV2FdtState::try_new_product_process_profile(
            fdt.address(),
            consumed.len(),
            crc64(0, &consumed),
        )
        .expect("consumed FDT identity should validate");
        let machine = HvfSnapshotV2MachineState::try_new(
            machine.machine(),
            machine.boot().clone(),
            fdt,
            machine.cpu_template().cloned(),
        )
        .expect("product machine metadata should validate");
        let platform =
            HvfSnapshotV2PlatformState::try_new(binding, machine, global, topology, vcpus, time)
                .expect("product platform should cross-validate");
        let state = HvfSnapshotV2State::try_new(platform, graph)
            .expect("product state should cross-validate");

        prepare_hvf_snapshot_v2_root_plan(state, memory, process, Instant::now())
            .expect("typed product profile should not parse guest-consumed FDT bytes");
    }

    #[test]
    fn exact_root_preparation_rejects_changed_live_fdt_identity() {
        let (state, mut memory, process) = coherent_mmio_root_plan_fixture();
        let fdt_address = state.platform().machine().fdt().address();
        memory
            .write_slice(&[0xff], fdt_address)
            .expect("changed live FDT byte should write");

        let error = prepare_hvf_snapshot_v2_root_plan(state, memory, process, Instant::now())
            .expect_err("changed live FDT bytes must fail integrity binding");
        assert!(matches!(
            error,
            PrepareHvfSnapshotV2RootPlanError::Fdt(source)
                if matches!(
                    source.as_ref(),
                    HvfSnapshotV2PlatformRestoreFailure::FdtIdentity
                )
        ));
    }

    #[test]
    fn mmio_root_fdt_requires_exact_node_and_command_line() {
        let (platform, root, process) = mmio_root_plan_fixture();
        let resources = prepare_root_resource_plan(&platform, &root, process)
            .expect("MMIO root resources should prepare");
        let shell = root_shell_devices(&platform, resources);
        let HvfSnapshotV2RootTransportPlan::Mmio {
            region,
            interrupt_line,
        } = resources.transport()
        else {
            panic!("fixture resource plan should use MMIO");
        };
        let root_device = bangbang_runtime::fdt::Arm64FdtVirtioMmioDevice {
            region: bangbang_runtime::fdt::Arm64FdtRegion {
                base: region.range().start().raw_value(),
                size: region.range().size(),
            },
            interrupt_line,
        };
        let command_line = canonical_process_root_block_command_line(
            platform.machine().boot().boot_arguments(),
            false,
            root.partuuid(),
            true,
        )
        .expect("MMIO root command line should normalize");
        let valid = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &[root_device],
            &command_line,
            None,
        );
        assert_eq!(
            validate_root_process_fdt(&valid, &platform, &root, resources),
            Ok(())
        );

        let missing_root_command_line =
            expected_process_boot_arguments(platform.machine().boot().boot_arguments())
                .expect("device-free command line should normalize");
        let missing_root = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &[root_device],
            &missing_root_command_line,
            None,
        );
        assert_eq!(
            validate_root_process_fdt(&missing_root, &platform, &root, resources),
            Err(HvfSnapshotV2ProcessFdtMismatch::Boot)
        );

        let optional = bangbang_runtime::fdt::Arm64FdtVirtioMmioDevice {
            region: bangbang_runtime::fdt::Arm64FdtRegion {
                base: region.range().end_exclusive().raw_value(),
                size: region.range().size(),
            },
            interrupt_line: GuestInterruptLine::new(interrupt_line.raw_value() + 1)
                .expect("optional interrupt should validate"),
        };
        let with_optional = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &[root_device, optional],
            &command_line,
            None,
        );
        assert_eq!(
            validate_root_process_fdt(&with_optional, &platform, &root, resources),
            Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory)
        );
    }

    #[test]
    fn multi_block_mmio_fdt_requires_exact_unique_node_set_and_command_line() {
        let (platform, plan) =
            crate::snapshot_v2_multi_block_platform::tests::mmio_fdt_plan_fixture();
        let records = plan
            .transport()
            .mmio_records()
            .expect("fixture plan should use MMIO");
        let devices = records
            .iter()
            .map(|record| record.fdt_device())
            .collect::<Vec<_>>();
        assert_eq!(devices.len(), 2);
        let shell = (
            bangbang_runtime::fdt::Arm64FdtSerialDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_SERIAL_MMIO_BASE.raw_value(),
                    size: SERIAL_MMIO_DEVICE_WINDOW_SIZE,
                },
                interrupt_line: plan.serial_interrupt(),
            },
            bangbang_runtime::fdt::Arm64FdtRtcDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_RTC_MMIO_BASE.raw_value(),
                    size: RTC_MMIO_DEVICE_WINDOW_SIZE,
                },
            },
            bangbang_runtime::fdt::Arm64FdtVmGenIdDevice {
                region: platform.time().vmgenid().fdt_region(),
                interrupt_line: plan.vmgenid_interrupt(),
            },
            bangbang_runtime::fdt::Arm64FdtVmClockDevice {
                region: platform.time().vmclock().fdt_region(),
                interrupt_line: plan.vmclock_interrupt(),
            },
        );
        let block = HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockMmio {
            command_line: plan.command_line(),
            records,
        };
        let valid = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &devices,
            plan.command_line(),
            None,
        );
        assert_eq!(
            validate_process_fdt(&valid, &platform, plan.serial_interrupt(), &block),
            Ok(())
        );

        let mut reversed = devices.clone();
        reversed.reverse();
        let reordered = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &reversed,
            plan.command_line(),
            None,
        );
        assert_eq!(
            validate_process_fdt(&reordered, &platform, plan.serial_interrupt(), &block),
            Ok(())
        );

        let mut missing = devices.clone();
        missing.pop();
        let missing = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &missing,
            plan.command_line(),
            None,
        );
        assert_eq!(
            validate_process_fdt(&missing, &platform, plan.serial_interrupt(), &block),
            Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory)
        );

        let mut extra = devices.clone();
        let mut extra_device = extra
            .last()
            .copied()
            .expect("fixture should have a last device");
        extra_device.region.base = extra_device
            .region
            .base
            .checked_add(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
            .expect("extra device address should fit");
        extra_device.interrupt_line =
            GuestInterruptLine::new(extra_device.interrupt_line.raw_value().saturating_add(1))
                .expect("extra device interrupt should validate");
        extra.push(extra_device);
        let extra = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &extra,
            plan.command_line(),
            None,
        );
        assert_eq!(
            validate_process_fdt(&extra, &platform, plan.serial_interrupt(), &block),
            Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory)
        );

        let mut unexpected = devices.clone();
        let last = unexpected
            .last_mut()
            .expect("fixture should have a last device");
        last.region.base = last
            .region
            .base
            .checked_add(VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
            .expect("unexpected device address should fit");
        let unexpected = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &unexpected,
            plan.command_line(),
            None,
        );
        assert_eq!(
            validate_process_fdt(&unexpected, &platform, plan.serial_interrupt(), &block),
            Err(HvfSnapshotV2ProcessFdtMismatch::Root)
        );

        let mut wrong_interrupt = devices.clone();
        let second = wrong_interrupt
            .last_mut()
            .expect("fixture should have a second device");
        second.interrupt_line =
            GuestInterruptLine::new(second.interrupt_line.raw_value().saturating_add(1))
                .expect("mutated interrupt should validate");
        let wrong_interrupt = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &wrong_interrupt,
            plan.command_line(),
            None,
        );
        assert_eq!(
            validate_process_fdt(&wrong_interrupt, &platform, plan.serial_interrupt(), &block),
            Err(HvfSnapshotV2ProcessFdtMismatch::Root)
        );

        let wrong_command_line = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &devices,
            "console=ttyS0 pci=off",
            None,
        );
        assert_eq!(
            validate_process_fdt(
                &wrong_command_line,
                &platform,
                plan.serial_interrupt(),
                &block
            ),
            Err(HvfSnapshotV2ProcessFdtMismatch::Boot)
        );
    }

    #[test]
    fn multi_block_pci_fdt_requires_one_exact_host_msi_frame_and_command_line() {
        let (platform, plan) =
            crate::snapshot_v2_multi_block_platform::tests::pci_fdt_plan_fixture();
        let pci = plan.transport().pci().expect("fixture plan should use PCI");
        assert_eq!(pci.records().len(), 2);
        let shell = (
            bangbang_runtime::fdt::Arm64FdtSerialDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_SERIAL_MMIO_BASE.raw_value(),
                    size: SERIAL_MMIO_DEVICE_WINDOW_SIZE,
                },
                interrupt_line: plan.serial_interrupt(),
            },
            bangbang_runtime::fdt::Arm64FdtRtcDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_RTC_MMIO_BASE.raw_value(),
                    size: RTC_MMIO_DEVICE_WINDOW_SIZE,
                },
            },
            bangbang_runtime::fdt::Arm64FdtVmGenIdDevice {
                region: platform.time().vmgenid().fdt_region(),
                interrupt_line: plan.vmgenid_interrupt(),
            },
            bangbang_runtime::fdt::Arm64FdtVmClockDevice {
                region: platform.time().vmclock().fdt_region(),
                interrupt_line: plan.vmclock_interrupt(),
            },
        );
        let block = HvfSnapshotV2ProcessBlockFdtPlan::MultiBlockPci {
            command_line: plan.command_line(),
            pci,
        };
        let valid = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &[],
            plan.command_line(),
            Some(pci.host()),
        );
        assert_eq!(
            validate_process_fdt(&valid, &platform, plan.serial_interrupt(), &block),
            Ok(())
        );

        let missing_host = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &[],
            plan.command_line(),
            None,
        );
        assert_eq!(
            validate_process_fdt(&missing_host, &platform, plan.serial_interrupt(), &block),
            Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory)
        );

        let wrong_command_line = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &[],
            "console=ttyS0 pci=off",
            Some(pci.host()),
        );
        assert_eq!(
            validate_process_fdt(
                &wrong_command_line,
                &platform,
                plan.serial_interrupt(),
                &block
            ),
            Err(HvfSnapshotV2ProcessFdtMismatch::Boot)
        );
    }

    #[test]
    fn storage_pci_fdt_requires_one_host_for_the_heterogeneous_vector() {
        let (platform, plan) = crate::snapshot_v2_storage_platform::tests::pci_fdt_plan_fixture();
        let pci = plan.pci();
        assert_eq!(pci.block_records().len(), 1);
        assert_eq!(pci.pmem_records().len(), 1);
        let shell = (
            bangbang_runtime::fdt::Arm64FdtSerialDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_SERIAL_MMIO_BASE.raw_value(),
                    size: SERIAL_MMIO_DEVICE_WINDOW_SIZE,
                },
                interrupt_line: plan.serial_interrupt(),
            },
            bangbang_runtime::fdt::Arm64FdtRtcDevice {
                region: bangbang_runtime::fdt::Arm64FdtRegion {
                    base: PROCESS_RTC_MMIO_BASE.raw_value(),
                    size: RTC_MMIO_DEVICE_WINDOW_SIZE,
                },
            },
            bangbang_runtime::fdt::Arm64FdtVmGenIdDevice {
                region: platform.time().vmgenid().fdt_region(),
                interrupt_line: plan.vmgenid_interrupt(),
            },
            bangbang_runtime::fdt::Arm64FdtVmClockDevice {
                region: platform.time().vmclock().fdt_region(),
                interrupt_line: plan.vmclock_interrupt(),
            },
        );
        let block = HvfSnapshotV2ProcessBlockFdtPlan::StoragePci {
            command_line: plan.command_line(),
            pci,
        };
        let valid = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &[],
            plan.command_line(),
            Some(pci.host()),
        );
        assert_eq!(
            validate_process_fdt(&valid, &platform, plan.serial_interrupt(), &block),
            Ok(())
        );
        let missing_host = build_process_fdt_fixture_with_profile(
            &platform,
            shell,
            &[],
            plan.command_line(),
            None,
        );
        assert_eq!(
            validate_process_fdt(&missing_host, &platform, plan.serial_interrupt(), &block),
            Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory)
        );
    }

    #[test]
    fn pci_root_resource_and_fdt_plans_match_first_product_slot() {
        let (platform, root, process) = pci_root_plan_fixture();
        let resources = prepare_root_resource_plan(&platform, &root, process)
            .expect("PCI root resources should match product order");
        let HvfSnapshotV2RootTransportPlan::Pci {
            sbdf,
            bar_region_id,
            bar_range,
            msi,
        } = resources.transport()
        else {
            panic!("fixture resource plan should use PCI");
        };
        assert_eq!(
            sbdf,
            PciSbdf::new(
                PCI_SEGMENT_ZERO,
                PCI_BUS_ZERO,
                PCI_FIRST_ENDPOINT_DEVICE,
                PCI_FUNCTION_ZERO,
            )
            .expect("first endpoint identity should validate")
        );
        assert_eq!(
            bar_region_id,
            pci_root_restore_bar_region_id().expect("root BAR region id should validate")
        );
        assert_eq!(
            bar_range,
            GuestMemoryRange::new(
                Arm64PciAddressPlan::firecracker_v1_16()
                    .expect("PCI address plan should validate")
                    .bar64()
                    .start(),
                VIRTIO_PCI_CAPABILITY_BAR_SIZE,
            )
            .expect("first PCI BAR should validate")
        );
        assert_eq!(
            msi.interrupt_range.count,
            pci_root_restore_gic_msi_configuration()
                .expect("PCI MSI demand should validate")
                .interrupt_count()
                .get()
        );

        let command_line = canonical_process_root_block_command_line(
            platform.machine().boot().boot_arguments(),
            true,
            root.partuuid(),
            true,
        )
        .expect("PCI root command line should normalize");
        let pci_host = bangbang_runtime::fdt::Arm64FdtPciHost::from_address_plan(
            Arm64PciAddressPlan::firecracker_v1_16()
                .expect("PCI host address plan should validate"),
        );
        let valid = build_process_fdt_fixture_with_profile(
            &platform,
            root_shell_devices(&platform, resources),
            &[],
            &command_line,
            Some(pci_host),
        );
        assert_eq!(
            validate_root_process_fdt(&valid, &platform, &root, resources),
            Ok(())
        );

        let missing_host = build_process_fdt_fixture_with_profile(
            &platform,
            root_shell_devices(&platform, resources),
            &[],
            &command_line,
            None,
        );
        assert_eq!(
            validate_root_process_fdt(&missing_host, &platform, &root, resources),
            Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory)
        );
    }

    #[test]
    fn exact_default_process_fdt_is_accepted_and_hostile_profiles_are_rejected() {
        let state = process_platform_fixture();
        let (serial, rtc, vmgenid, vmclock) = fixture_shell_devices(&state);
        let valid = build_process_fdt_fixture(&state, serial, rtc, vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&valid, &state, serial.interrupt_line),
            Ok(())
        );
        assert_eq!(
            validate_default_process_fdt(b"not an FDT", &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::Parse)
        );

        let optional = bangbang_runtime::fdt::Arm64FdtVirtioMmioDevice {
            region: bangbang_runtime::fdt::Arm64FdtRegion {
                base: 0x5000_0000,
                size: bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            },
            interrupt_line: bangbang_runtime::interrupt::GuestInterruptLine::new(
                vmclock.interrupt_line.raw_value() + 1,
            )
            .expect("fixture optional-device interrupt should validate"),
        };
        let with_optional =
            build_process_fdt_fixture(&state, serial, rtc, vmgenid, vmclock, &[optional]);
        assert_eq!(
            validate_default_process_fdt(&with_optional, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::RootInventory)
        );
    }

    #[test]
    fn serial_only_shell_registers_complete_uart_at_fixed_product_identity() {
        let state = product_process_platform_fixture();
        assert!(state.global().compatibility().gic_metadata().msi.is_none());
        let expected = bangbang_runtime::serial::SerialMmioCaptureState::try_from_parts(
            bangbang_runtime::serial::SerialMmioCaptureStateParts {
                legacy_state: bangbang_runtime::serial::SerialMmioState::new(
                    1, 3, 8, 0x5a, 12, 0,
                ),
                interrupt_identification:
                    bangbang_runtime::serial::SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE,
                line_status: bangbang_runtime::serial::SERIAL_LINE_STATUS_DEFAULT
                    | bangbang_runtime::serial::SERIAL_LINE_STATUS_DATA_READY
                    | bangbang_runtime::serial::SERIAL_LINE_STATUS_OVERRUN_ERROR,
                modem_status: 0,
                receive_bytes: b"restored".to_vec(),
                receive_interrupt_intent_pending: true,
                input_ready_intent_pending: false,
            },
        )
        .expect("complete restored UART state should validate");
        let output =
            SharedSerialOutput::new(bangbang_runtime::serial::SharedSerialOutputBuffer::default());
        let serial =
            SerialMmioDevice::from_capture_state_with_shared_output(output, expected.clone());

        let (mut dispatcher, device) = prepare_process_shell(
            Some(HvfSnapshotV2ProcessShellRestore::SerialOnly {
                shell: HvfSnapshotV2RestoredSerialShell::new(serial).into(),
                process: HvfSnapshotV2SerialOnlyProcessConfig::new(false),
            }),
            &state,
            b"authenticated product FDT bytes are not reparsed",
        )
        .expect("serial-only restored shell should prepare");
        let device = device.expect("serial metadata should be retained");
        let (serial_fdt, _, _, _) = fixture_shell_devices(&state);
        assert_eq!(device.region.id(), PROCESS_SERIAL_MMIO_REGION_ID);
        assert_eq!(device.region.range().start(), PROCESS_SERIAL_MMIO_BASE);
        assert_eq!(device.fdt_device, serial_fdt);
        assert_eq!(
            bangbang_runtime::startup::capture_serial_state_for_device(&device, &mut dispatcher,)
                .expect("registered UART should recapture"),
            expected
        );
    }

    #[test]
    fn entropy_mmio_shell_allocates_before_fixed_product_interrupts() {
        let (state, entropy_interrupt, serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
            product_entropy_interrupt_platform_fixture(product_process_platform_fixture(), &[]);

        let (_dispatcher, device) = prepare_process_shell(
            Some(HvfSnapshotV2ProcessShellRestore::SerialEntropyMmio {
                shell: restored_serial_shell().into(),
                entropy_interrupt,
            }),
            &state,
            b"authenticated product FDT bytes are not reparsed",
        )
        .expect("serial-plus-entropy shell should prepare");
        assert_eq!(
            device
                .expect("restored serial metadata should exist")
                .fdt_device
                .interrupt_line,
            serial_interrupt
        );
        assert_eq!(state.time().vmgenid().interrupt_line(), vmgenid_interrupt);
        assert_eq!(state.time().vmclock().interrupt_line(), vmclock_interrupt);

        let wrong_entropy =
            GuestInterruptLine::new(entropy_interrupt.raw_value().saturating_add(1))
                .expect("wrong entropy line should validate structurally");
        assert!(matches!(
            prepare_process_shell(
                Some(HvfSnapshotV2ProcessShellRestore::SerialEntropyMmio {
                    shell: restored_serial_shell().into(),
                    entropy_interrupt: wrong_entropy,
                }),
                &state,
                b"authenticated product FDT bytes are not reparsed",
            ),
            Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity)
        ));
    }

    #[test]
    fn balloon_mmio_shell_allocates_before_optional_and_fixed_product_interrupts() {
        let (state, balloon_interrupt, serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
            product_entropy_interrupt_platform_fixture(product_process_platform_fixture(), &[]);
        let shell_plan = HvfSnapshotV2BalloonMmioShellPlan {
            balloon_interrupt,
            command_line: None,
            block_records: &[],
            pmem_records: &[],
            entropy_interrupt: None,
            serial_interrupt,
            vmgenid_interrupt,
            vmclock_interrupt,
        };

        let (_dispatcher, device) = prepare_process_shell(
            Some(HvfSnapshotV2ProcessShellRestore::BalloonMmio {
                shell: restored_serial_shell().into(),
                plan: shell_plan,
            }),
            &state,
            b"authenticated product FDT bytes are not reparsed",
        )
        .expect("serial-plus-balloon shell should prepare");
        assert_eq!(
            device
                .expect("restored serial metadata should exist")
                .fdt_device
                .interrupt_line,
            serial_interrupt
        );

        let wrong_balloon =
            GuestInterruptLine::new(balloon_interrupt.raw_value().saturating_add(1))
                .expect("wrong balloon line should validate structurally");
        let shell_plan = HvfSnapshotV2BalloonMmioShellPlan {
            balloon_interrupt: wrong_balloon,
            command_line: None,
            block_records: &[],
            pmem_records: &[],
            entropy_interrupt: None,
            serial_interrupt,
            vmgenid_interrupt,
            vmclock_interrupt,
        };
        assert!(matches!(
            prepare_process_shell(
                Some(HvfSnapshotV2ProcessShellRestore::BalloonMmio {
                    shell: restored_serial_shell().into(),
                    plan: shell_plan,
                }),
                &state,
                b"authenticated product FDT bytes are not reparsed",
            ),
            Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity)
        ));
    }

    #[test]
    fn network_mmio_shell_accepts_an_empty_internal_network_product() {
        let state = product_process_platform_fixture();
        let (serial, _, vmgenid, vmclock) = fixture_shell_devices(&state);
        let shell_plan = HvfSnapshotV2NetworkMmioShellPlan {
            balloon_interrupt: None,
            command_line: None,
            block_records: &[],
            network_interrupts: &[],
            pmem_records: &[],
            following_interrupt: None,
            entropy_interrupt: None,
            memory_hotplug_interrupt: None,
            serial_interrupt: serial.interrupt_line,
            vmgenid_interrupt: vmgenid.interrupt_line,
            vmclock_interrupt: vmclock.interrupt_line,
        };

        let (_dispatcher, device) = prepare_process_shell(
            Some(HvfSnapshotV2ProcessShellRestore::NetworkMmio {
                shell: restored_serial_shell().into(),
                plan: shell_plan,
            }),
            &state,
            b"authenticated product FDT bytes are not reparsed",
        )
        .expect("zero-network exact-2.11 shell should prepare");
        assert_eq!(
            device
                .expect("restored serial metadata should exist")
                .fdt_device
                .interrupt_line,
            serial.interrupt_line,
        );
    }

    #[test]
    fn network_mmio_shell_replays_a_following_endpoint_before_fixed_interrupts() {
        let (state, following_interrupt, serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
            product_entropy_interrupt_platform_fixture(product_process_platform_fixture(), &[]);
        let shell_plan = HvfSnapshotV2NetworkMmioShellPlan {
            balloon_interrupt: None,
            command_line: None,
            block_records: &[],
            network_interrupts: &[],
            pmem_records: &[],
            following_interrupt: Some(following_interrupt),
            entropy_interrupt: None,
            memory_hotplug_interrupt: None,
            serial_interrupt,
            vmgenid_interrupt,
            vmclock_interrupt,
        };

        prepare_process_shell(
            Some(HvfSnapshotV2ProcessShellRestore::NetworkMmio {
                shell: restored_serial_shell().into(),
                plan: shell_plan,
            }),
            &state,
            b"authenticated product FDT bytes are not reparsed",
        )
        .expect("a following exact-2.12 endpoint should preserve product interrupt order");

        let wrong_interrupt =
            GuestInterruptLine::new(following_interrupt.raw_value().saturating_add(1))
                .expect("wrong following interrupt should validate structurally");
        let shell_plan = HvfSnapshotV2NetworkMmioShellPlan {
            balloon_interrupt: None,
            command_line: None,
            block_records: &[],
            network_interrupts: &[],
            pmem_records: &[],
            following_interrupt: Some(wrong_interrupt),
            entropy_interrupt: None,
            memory_hotplug_interrupt: None,
            serial_interrupt,
            vmgenid_interrupt,
            vmclock_interrupt,
        };
        assert!(matches!(
            prepare_process_shell(
                Some(HvfSnapshotV2ProcessShellRestore::NetworkMmio {
                    shell: restored_serial_shell().into(),
                    plan: shell_plan,
                }),
                &state,
                b"authenticated product FDT bytes are not reparsed",
            ),
            Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity)
        ));
    }

    #[test]
    fn storage_entropy_mmio_shell_allocates_after_storage_and_before_serial() {
        let (platform, plan) = crate::snapshot_v2_storage_platform::tests::mmio_fdt_plan_fixture();
        let storage_interrupts = plan
            .block_records()
            .iter()
            .chain(plan.pmem_records())
            .map(|record| record.interrupt_line())
            .collect::<Vec<_>>();
        let (state, entropy_interrupt, serial_interrupt, vmgenid_interrupt, vmclock_interrupt) =
            product_entropy_interrupt_platform_fixture(platform, &storage_interrupts);
        let shell_plan = HvfSnapshotV2StorageMmioShellPlan {
            command_line: plan.command_line(),
            block_records: plan.block_records(),
            pmem_records: plan.pmem_records(),
            serial_interrupt,
            vmgenid_interrupt,
            vmclock_interrupt,
        };

        let (_dispatcher, device) = prepare_process_shell(
            Some(HvfSnapshotV2ProcessShellRestore::StorageEntropyMmio {
                shell: restored_serial_shell().into(),
                plan: shell_plan,
                entropy_interrupt,
            }),
            &state,
            b"authenticated product FDT bytes are not reparsed",
        )
        .expect("storage-plus-entropy shell should prepare");
        assert_eq!(
            device
                .expect("restored serial metadata should exist")
                .fdt_device
                .interrupt_line,
            serial_interrupt
        );

        let shell_plan = HvfSnapshotV2StorageMmioShellPlan {
            command_line: plan.command_line(),
            block_records: plan.block_records(),
            pmem_records: plan.pmem_records(),
            serial_interrupt,
            vmgenid_interrupt,
            vmclock_interrupt,
        };
        assert!(matches!(
            prepare_process_shell(
                Some(HvfSnapshotV2ProcessShellRestore::StorageEntropyMmio {
                    shell: restored_serial_shell().into(),
                    plan: shell_plan,
                    entropy_interrupt: storage_interrupts[0],
                }),
                &state,
                b"authenticated product FDT bytes are not reparsed",
            ),
            Err(HvfSnapshotV2PlatformRestoreFailure::ProcessShellInterruptIdentity)
        ));
    }

    #[test]
    fn serial_only_shell_rejects_pci_policy_before_uart_registration() {
        let state = product_process_platform_fixture();
        assert!(state.global().compatibility().gic_metadata().msi.is_none());
        let serial = SerialMmioDevice::with_shared_output(SharedSerialOutput::new(
            bangbang_runtime::serial::SharedSerialOutputBuffer::default(),
        ));
        let error = prepare_process_shell(
            Some(HvfSnapshotV2ProcessShellRestore::SerialOnly {
                shell: HvfSnapshotV2RestoredSerialShell::new(serial).into(),
                process: HvfSnapshotV2SerialOnlyProcessConfig::new(true),
            }),
            &state,
            b"authenticated product FDT bytes are not reparsed",
        )
        .expect_err("MMIO GIC must reject a PCI serial-only policy");
        assert!(matches!(
            error,
            HvfSnapshotV2PlatformRestoreFailure::ProcessShellFdt {
                mismatch: HvfSnapshotV2ProcessFdtMismatch::Profile,
            }
        ));
    }

    #[test]
    fn default_process_fdt_rejects_serial_rtc_and_identity_drift() {
        let state = process_platform_fixture();
        let (serial, rtc, vmgenid, vmclock) = fixture_shell_devices(&state);

        let wrong_serial_size = bangbang_runtime::fdt::Arm64FdtSerialDevice {
            region: bangbang_runtime::fdt::Arm64FdtRegion {
                size: SERIAL_MMIO_DEVICE_WINDOW_SIZE * 2,
                ..serial.region
            },
            ..serial
        };
        let bytes =
            build_process_fdt_fixture(&state, wrong_serial_size, rtc, vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::Serial)
        );

        let wrong_serial_interrupt = bangbang_runtime::fdt::Arm64FdtSerialDevice {
            interrupt_line: bangbang_runtime::interrupt::GuestInterruptLine::new(
                vmclock.interrupt_line.raw_value() + 1,
            )
            .expect("fixture serial interrupt drift should validate"),
            ..serial
        };
        let bytes =
            build_process_fdt_fixture(&state, wrong_serial_interrupt, rtc, vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::Serial)
        );

        let wrong_rtc_size = bangbang_runtime::fdt::Arm64FdtRtcDevice {
            region: bangbang_runtime::fdt::Arm64FdtRegion {
                size: RTC_MMIO_DEVICE_WINDOW_SIZE / 2,
                ..rtc.region
            },
        };
        let bytes =
            build_process_fdt_fixture(&state, serial, wrong_rtc_size, vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::Rtc)
        );

        let wrong_vmgenid = bangbang_runtime::fdt::Arm64FdtVmGenIdDevice {
            interrupt_line: bangbang_runtime::interrupt::GuestInterruptLine::new(
                vmclock.interrupt_line.raw_value() + 1,
            )
            .expect("fixture VMGenID interrupt drift should validate"),
            ..vmgenid
        };
        let bytes = build_process_fdt_fixture(&state, serial, rtc, wrong_vmgenid, vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::VmGenId)
        );

        let wrong_vmclock = bangbang_runtime::fdt::Arm64FdtVmClockDevice {
            interrupt_line: bangbang_runtime::interrupt::GuestInterruptLine::new(
                vmclock.interrupt_line.raw_value() + 2,
            )
            .expect("fixture VMClock interrupt drift should validate"),
            ..vmclock
        };
        let bytes = build_process_fdt_fixture(&state, serial, rtc, vmgenid, wrong_vmclock, &[]);
        assert_eq!(
            validate_default_process_fdt(&bytes, &state, serial.interrupt_line),
            Err(HvfSnapshotV2ProcessFdtMismatch::VmClock)
        );
    }

    #[test]
    fn process_fdt_memory_omits_the_reserved_arm64_system_area() {
        let range = GuestMemoryRange::new(
            GuestAddress::new(aarch64::DRAM_MEM_START),
            aarch64::SYSTEM_MEM_SIZE * 2,
        )
        .expect("test memory range should validate");
        let layout = bangbang_runtime::memory::GuestMemoryLayout::new(vec![range])
            .expect("test memory layout should validate");
        let memory = GuestMemory::allocate(&layout).expect("test guest memory should allocate");
        let mut image = std::io::Cursor::new(Vec::new());
        let binding = write_snapshot_v2_memory_image(&memory, &mut image)
            .expect("test native-v2 binding should encode");
        let advertised_start = aarch64::DRAM_MEM_START + aarch64::SYSTEM_MEM_SIZE;
        let advertised_size = range.size() - aarch64::SYSTEM_MEM_SIZE;
        let mut advertised = Vec::with_capacity(16);
        advertised.extend_from_slice(&advertised_start.to_be_bytes());
        advertised.extend_from_slice(&advertised_size.to_be_bytes());
        let node = Node {
            name: "memory@ram".to_owned(),
            props: vec![("reg".to_owned(), advertised)],
            children: Vec::new(),
        };

        assert!(memory_property_matches_binding(&node, &binding));

        let mut unadjusted = Vec::with_capacity(16);
        unadjusted.extend_from_slice(&range.start().raw_value().to_be_bytes());
        unadjusted.extend_from_slice(&range.size().to_be_bytes());
        let unadjusted_node = Node {
            name: "memory@ram".to_owned(),
            props: vec![("reg".to_owned(), unadjusted)],
            children: Vec::new(),
        };
        assert!(!memory_property_matches_binding(&unadjusted_node, &binding));
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ProtocolEvent {
        Execute(HvfSnapshotV2PlatformRestoreStage),
        Cleanup(HvfSnapshotV2PlatformCleanupStage),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct Injected;

    impl fmt::Display for Injected {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("injected")
        }
    }

    impl std::error::Error for Injected {}

    struct FakeProtocol {
        fail_stage: Option<HvfSnapshotV2PlatformRestoreStage>,
        fail_cleanup: Vec<HvfSnapshotV2PlatformCleanupStage>,
        events: Vec<ProtocolEvent>,
    }

    impl RestoreProtocol for FakeProtocol {
        type Error = Injected;

        fn execute(&mut self, stage: HvfSnapshotV2PlatformRestoreStage) -> Result<(), Self::Error> {
            self.events.push(ProtocolEvent::Execute(stage));
            if self.fail_stage == Some(stage) {
                Err(Injected)
            } else {
                Ok(())
            }
        }

        fn cleanup(&mut self, stage: HvfSnapshotV2PlatformCleanupStage) -> Result<(), Self::Error> {
            self.events.push(ProtocolEvent::Cleanup(stage));
            if self.fail_cleanup.contains(&stage) {
                Err(Injected)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn every_restore_stage_cleans_the_exact_owned_prefix_once() {
        let stages = restore_protocol_stages(3);
        for (failed_position, failed_stage) in stages.iter().copied().enumerate() {
            let mut protocol = FakeProtocol {
                fail_stage: Some(failed_stage),
                fail_cleanup: Vec::new(),
                events: Vec::new(),
            };
            let error = run_restore_protocol(&mut protocol, 3)
                .expect_err("injected stage should fail the protocol");
            assert_eq!(error.stage, failed_stage);
            assert!(error.cleanup.is_empty());

            let mut expected = stages
                .iter()
                .take(failed_position + 1)
                .copied()
                .map(ProtocolEvent::Execute)
                .collect::<Vec<_>>();
            expected.extend(
                cleanup_sequence(restore_ownership_before_failure(failed_stage))
                    .iter()
                    .copied()
                    .map(ProtocolEvent::Cleanup),
            );
            assert_eq!(protocol.events, expected);
        }
    }

    #[test]
    fn cleanup_failures_are_retained_without_skipping_later_cleanup() {
        let failed_stage = HvfSnapshotV2PlatformRestoreStage::Vcpu { index: 1 };
        let mut protocol = FakeProtocol {
            fail_stage: Some(failed_stage),
            fail_cleanup: vec![
                HvfSnapshotV2PlatformCleanupStage::Topology,
                HvfSnapshotV2PlatformCleanupStage::Backend,
            ],
            events: Vec::new(),
        };
        let error = run_restore_protocol(&mut protocol, 3)
            .expect_err("injected vCPU stage should fail the protocol");
        assert_eq!(
            error.cleanup,
            vec![
                (HvfSnapshotV2PlatformCleanupStage::Topology, Injected),
                (HvfSnapshotV2PlatformCleanupStage::Backend, Injected),
            ]
        );
        assert_eq!(
            protocol
                .events
                .iter()
                .rev()
                .take(2)
                .copied()
                .collect::<Vec<_>>(),
            vec![
                ProtocolEvent::Cleanup(HvfSnapshotV2PlatformCleanupStage::Backend),
                ProtocolEvent::Cleanup(HvfSnapshotV2PlatformCleanupStage::Topology),
            ]
        );
    }

    #[test]
    fn publication_failure_shuts_session_before_backend() {
        let mut protocol = FakeProtocol {
            fail_stage: Some(HvfSnapshotV2PlatformRestoreStage::Publication),
            fail_cleanup: Vec::new(),
            events: Vec::new(),
        };
        run_restore_protocol(&mut protocol, 2).expect_err("injected publication stage should fail");
        assert_eq!(
            protocol
                .events
                .iter()
                .rev()
                .take(2)
                .copied()
                .collect::<Vec<_>>(),
            vec![
                ProtocolEvent::Cleanup(HvfSnapshotV2PlatformCleanupStage::Backend),
                ProtocolEvent::Cleanup(HvfSnapshotV2PlatformCleanupStage::Session),
            ]
        );
    }

    #[test]
    fn successful_protocol_publishes_without_cleanup() {
        let mut protocol = FakeProtocol {
            fail_stage: None,
            fail_cleanup: Vec::new(),
            events: Vec::new(),
        };
        run_restore_protocol(&mut protocol, 3).expect("complete protocol should publish");
        assert_eq!(
            protocol.events,
            restore_protocol_stages(3)
                .into_iter()
                .map(ProtocolEvent::Execute)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn restore_error_marks_the_first_guest_visible_identity_commit_boundary() {
        let before_identity = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Preflight,
            HvfSnapshotV2PlatformRestoreFailure::MemoryTopology,
            Vec::new(),
        );
        assert!(!before_identity.is_committed());

        let replacement_failure = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::VmGenId,
            HvfSnapshotV2PlatformRestoreFailure::VmGenId(
                HvfArm64BootVmGenIdRestoreError::Replacement {
                    source: bangbang_runtime::startup::Arm64BootVmGenIdReplacementError::Random,
                },
            ),
            Vec::new(),
        );
        assert!(!replacement_failure.is_committed());

        let vmgenid_signal_failure = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::VmGenId,
            HvfSnapshotV2PlatformRestoreFailure::VmGenId(HvfArm64BootVmGenIdRestoreError::Signal {
                source: HvfGicSpiSignalError::InvalidState("injected"),
            }),
            Vec::new(),
        );
        assert!(vmgenid_signal_failure.is_committed());

        let vmclock_failure = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::VmClock,
            HvfSnapshotV2PlatformRestoreFailure::VmClock(HvfArm64BootVmClockRestoreError::Update {
                source: bangbang_runtime::vmclock::VmClockRestoreUpdateError::InvalidRange,
            }),
            Vec::new(),
        );
        assert!(vmclock_failure.is_committed());
        assert!(
            vmclock_failure
                .to_string()
                .contains("after destination identity committed")
        );
    }

    #[test]
    fn root_restore_is_terminal_when_nested_platform_cleanup_is_incomplete() {
        let clean = crate::startup::HvfSnapshotV2RootRestoreError::platform(
            HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Memory,
                HvfSnapshotV2PlatformRestoreFailure::MemoryTopology,
                Vec::new(),
            ),
        );
        assert!(!clean.is_committed());
        assert!(!clean.has_incomplete_cleanup());
        assert!(!clean.is_terminal());

        let incomplete = crate::startup::HvfSnapshotV2RootRestoreError::platform(
            HvfSnapshotV2PlatformRestoreError::new(
                HvfSnapshotV2PlatformRestoreStage::Memory,
                HvfSnapshotV2PlatformRestoreFailure::MemoryTopology,
                vec![HvfSnapshotV2PlatformCleanupFailure {
                    stage: HvfSnapshotV2PlatformCleanupStage::Backend,
                    source: Box::new(Injected),
                }],
            ),
        );
        assert!(!incomplete.is_committed());
        assert!(
            incomplete.cleanup_failures().is_empty(),
            "nested platform cleanup stays attached to its platform failure"
        );
        assert!(incomplete.has_incomplete_cleanup());
        assert!(incomplete.is_terminal());
    }

    #[test]
    fn restore_diagnostics_redact_sensitive_values() {
        let failure = HvfSnapshotV2PlatformRestoreFailure::CompatibilityMismatch;
        let error = HvfSnapshotV2PlatformRestoreError::new(
            HvfSnapshotV2PlatformRestoreStage::Compatibility { index: 2 },
            failure,
            Vec::new(),
        );
        let debug = format!("{error:?}");
        let display = error.to_string();
        assert!(debug.contains(REDACTED));
        for secret in ["deadbeef", "12345678", "guest/path", "gic-bytes"] {
            assert!(!debug.contains(secret));
            assert!(!display.contains(secret));
        }
    }
}
