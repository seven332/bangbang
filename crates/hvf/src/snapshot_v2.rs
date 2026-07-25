//! Permanent native-v2 multi-vCPU Hypervisor.framework platform state.

use std::collections::TryReserveError;
use std::ffi::OsStr;
use std::fmt;
use std::mem::size_of;
use std::os::unix::ffi::OsStrExt;

use bangbang_runtime::fdt::{ARM64_FDT_VMCLOCK_SIZE, ARM64_FDT_VMGENID_SIZE, Arm64FdtRegion};
use bangbang_runtime::interrupt::GuestInterruptLine;
use bangbang_runtime::machine::{
    MAX_SUPPORTED_VCPUS, MachineConfig, MachineConfigCpuTemplate, MachineConfigHugePages,
    MachineConfigInput,
};
use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange, aarch64};
use bangbang_runtime::pvtime::{
    ARM64_PVTIME_STRUCTURE_ALIGNMENT, ARM64_PVTIME_STRUCTURE_SIZE, Arm64PvTimeLayout,
};
use bangbang_runtime::rtc::{RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
use bangbang_runtime::snapshot_device::SnapshotV1PlatformDeviceMetadata;
use bangbang_runtime::snapshot_format_v2::{
    NATIVE_V2_GLOBAL_COMPONENT_KEY, NATIVE_V2_MACHINE_COMPONENT_KEY,
    NATIVE_V2_MEMORY_COMPONENT_KEY, NATIVE_V2_TIME_COMPONENT_KEY, NATIVE_V2_TOPOLOGY_COMPONENT_KEY,
    SnapshotV2Component, SnapshotV2ComponentDisposition, SnapshotV2EncodeError, SnapshotV2State,
    encode_snapshot_v2_state, native_v2_vcpu_component_key,
};
use bangbang_runtime::snapshot_memory_v2::{
    SnapshotV2MemoryBinding, SnapshotV2MemoryBindingError, SnapshotV2MemoryStateError,
    decode_snapshot_v2_memory_binding,
};
use bangbang_runtime::vmclock::{VMCLOCK_ABI_SIZE, VmClockAbi};

use crate::cpu_template::{
    HVF_ARM64_CPU_TEMPLATE_APPLICATION_MAX_ENTRIES, HvfArm64CpuTemplateApplicationEntry,
    HvfArm64CpuTemplateApplicationState, HvfArm64CpuTemplateValueWidth,
};
use crate::gic::{
    HvfArm64GicIccRegisterState, HvfGicDeviceState, HvfGicInterruptRange, HvfGicMetadata,
    HvfGicMsiMetadata, HvfGicRedistributor, HvfGicRegion, HvfGicTimerInterrupts,
    validate_gic_ppi_pending_intid,
};
use crate::optional_state::{
    HvfArm64DebugRegisterRestoreState, HvfArm64OptionalStateValue,
    HvfArm64ReviewedOptionalStateRestore, HvfArm64SmeRestoreState, HvfArm64SmeRestoreStateInput,
};
use crate::paused_topology::{
    HvfArm64CpuSuspendConvention, HvfArm64StableCpuSuspendState,
    HvfArm64StablePausedTopologyMember, HvfArm64StablePausedTopologyState,
    HvfArm64StableVcpuDisposition,
};
use crate::snapshot::HvfArm64SnapshotTimerState;
use crate::snapshot_bundle::{
    HvfSnapshotV1CompatibilityState, HvfSnapshotV1DecodeError, HvfSnapshotV1EncodeError,
    HvfSnapshotV1VcpuState, decode_vcpu, encode_vcpu,
};
use crate::vcpu::{
    HvfArm64VcpuIdentificationRegisterState, HvfArm64VcpuPendingInterruptState,
    HvfArm64VcpuSimdFpState, HvfArm64VcpuSmePstate, HvfArm64VcpuSveSmeIdentificationRegisterState,
};
use crate::vcpu_config::{
    HvfArm64VcpuCacheConfiguration, HvfArm64VcpuCacheGeometry, HvfArm64VcpuCacheManifest,
};

const REDACTED: &str = "<redacted>";
const MIB: u64 = 1024 * 1024;
const COMPONENT_PROFILE: u16 = 1;
const COMPONENT_FLAGS: u32 = 0;
const MACHINE_MAGIC: [u8; 8] = *b"BANGMC2\0";
const GLOBAL_MAGIC: [u8; 8] = *b"BANGGL2\0";
const TOPOLOGY_MAGIC: [u8; 8] = *b"BANGTP2\0";
const VCPU_MAGIC: [u8; 8] = *b"BANGVC2\0";
const TIME_MAGIC: [u8; 8] = *b"BANGTM2\0";
const OPTIONAL_MAGIC: [u8; 8] = *b"BANGOP2\0";
const MACHINE_HEADER_BYTES: usize = 80;
const MACHINE_CPU_ENTRY_BYTES: usize = 72;
const GLOBAL_HEADER_BYTES: usize = 24;
const GLOBAL_COMPATIBILITY_BYTES: usize = 376;
const TOPOLOGY_HEADER_BYTES: usize = 32;
const TOPOLOGY_MEMBER_BYTES: usize = 48;
const VCPU_HEADER_BYTES: usize = 48;
const VCPU_INTERRUPT_BYTES: usize = 144;
const TIME_HEADER_BYTES: usize = 240;
const TIME_PVTIME_ENTRY_BYTES: usize = 24;
const TIME_RTC_POLICY_DESTINATION_SYSTEM_TIME: u8 = 1;
const TIME_PVTIME_POLICY_PRESERVE_EXCLUDE_DOWNTIME: u8 = 1;
const TIME_VMGENID_POLICY_REGENERATE_NOTIFY: u8 = 1;
const TIME_VMCLOCK_POLICY_INCREMENT_NOTIFY: u8 = 1;
const OPTIONAL_HEADER_BYTES: usize = 64;
const OPTIONAL_RECORD_HEADER_BYTES: usize = 12;
const OPTIONAL_DEBUG_CAPACITY: usize = 16;
const OPTIONAL_SME_Z_COUNT: usize = 32;
const OPTIONAL_SME_P_COUNT: usize = 16;
const OPTIONAL_SME_VERSION_SME2: u8 = 1;
const OPTIONAL_MAX_RECORDS: usize =
    OPTIONAL_DEBUG_CAPACITY * 4 + 4 + OPTIONAL_SME_Z_COUNT + OPTIONAL_SME_P_COUNT + 2;
const OPTIONAL_MAX_REGISTRY_BYTES: usize = 96 * 1024;
const OPTIONAL_TAG_BREAKPOINT_VALUE: u16 = 1;
const OPTIONAL_TAG_BREAKPOINT_CONTROL: u16 = 17;
const OPTIONAL_TAG_WATCHPOINT_VALUE: u16 = 33;
const OPTIONAL_TAG_WATCHPOINT_CONTROL: u16 = 49;
const OPTIONAL_TAG_SME_PSTATE: u16 = 65;
const OPTIONAL_TAG_SME_SMCR: u16 = 66;
const OPTIONAL_TAG_SME_Z: u16 = 100;
const OPTIONAL_TAG_SME_P: u16 = 132;
const OPTIONAL_TAG_SME_ZA: u16 = 148;
const OPTIONAL_TAG_SME_ZT0: u16 = 149;
const OPTIONAL_DISPOSITION_EXPLICIT: u8 = 0;
const OPTIONAL_DISPOSITION_DESTINATION_DEFAULT: u8 = 1;

/// Maximum inert native path bytes admitted by the native-v2 platform profile.
pub const HVF_SNAPSHOT_V2_MAX_PATH_BYTES: usize = 4096;

/// Maximum boot-argument bytes admitted before their implicit NUL terminator.
pub const HVF_SNAPSHOT_V2_MAX_BOOT_ARGUMENT_BYTES: usize = aarch64::CMDLINE_MAX_SIZE - 1;

/// Architectural maximum SME streaming vector length admitted by the profile.
pub const HVF_SNAPSHOT_V2_MAX_SME_SVL_BYTES: usize = 256;

/// Maximum opaque VM-global GIC state admitted by the profile.
pub const HVF_SNAPSHOT_V2_GIC_DEVICE_STATE_MAX_BYTES: usize = 12 * 1024 * 1024;

/// One bounded inert native path retained only as snapshot metadata.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV2NativePath {
    bytes: Box<[u8]>,
}

impl HvfSnapshotV2NativePath {
    /// Copy and validate one native path without resolving or opening it.
    pub fn try_new(path: &OsStr) -> Result<Self, HvfSnapshotV2BuildError> {
        Self::try_from_bytes(path.as_bytes())
    }

    /// Copy and validate native path bytes without assigning path authority.
    pub fn try_from_bytes(bytes: &[u8]) -> Result<Self, HvfSnapshotV2BuildError> {
        if bytes.is_empty() || bytes.len() > HVF_SNAPSHOT_V2_MAX_PATH_BYTES || bytes.contains(&0) {
            return Err(HvfSnapshotV2BuildError::BootMetadata);
        }
        Ok(Self {
            bytes: copy_boxed(bytes).map_err(|_| HvfSnapshotV2BuildError::Allocation)?,
        })
    }

    /// Return inert native path bytes to trusted persistence code.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for HvfSnapshotV2NativePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HvfSnapshotV2NativePath(<redacted>)")
    }
}

/// Inert logical boot metadata retained by the native-v2 machine component.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV2BootState {
    kernel_path: HvfSnapshotV2NativePath,
    initrd_path: Option<HvfSnapshotV2NativePath>,
    boot_arguments: Option<Box<str>>,
}

impl HvfSnapshotV2BootState {
    /// Construct bounded inert boot metadata.
    pub fn try_new(
        kernel_path: HvfSnapshotV2NativePath,
        initrd_path: Option<HvfSnapshotV2NativePath>,
        boot_arguments: Option<&str>,
    ) -> Result<Self, HvfSnapshotV2BuildError> {
        let boot_arguments = boot_arguments
            .map(|arguments| {
                if arguments.len() > HVF_SNAPSHOT_V2_MAX_BOOT_ARGUMENT_BYTES
                    || arguments.as_bytes().contains(&0)
                {
                    return Err(HvfSnapshotV2BuildError::BootMetadata);
                }
                copy_string(arguments).map_err(|_| HvfSnapshotV2BuildError::Allocation)
            })
            .transpose()?;
        Ok(Self {
            kernel_path,
            initrd_path,
            boot_arguments,
        })
    }

    /// Return the inert kernel path.
    pub const fn kernel_path(&self) -> &HvfSnapshotV2NativePath {
        &self.kernel_path
    }

    /// Return the inert optional initrd path.
    pub const fn initrd_path(&self) -> Option<&HvfSnapshotV2NativePath> {
        self.initrd_path.as_ref()
    }

    /// Return optional boot arguments to trusted persistence code.
    pub fn boot_arguments(&self) -> Option<&str> {
        self.boot_arguments.as_deref()
    }
}

impl fmt::Debug for HvfSnapshotV2BootState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2BootState")
            .field("kernel_path", &REDACTED)
            .field("initrd_path", &self.initrd_path.as_ref().map(|_| REDACTED))
            .field(
                "boot_arguments",
                &self.boot_arguments.as_ref().map(|_| REDACTED),
            )
            .finish()
    }
}

/// Stable FDT placement and redacted content identity.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2FdtState {
    address: GuestAddress,
    size: u32,
    checksum: u64,
}

impl HvfSnapshotV2FdtState {
    /// Construct one locally bounded FDT fact.
    pub fn try_new(
        address: GuestAddress,
        size: usize,
        checksum: u64,
    ) -> Result<Self, HvfSnapshotV2BuildError> {
        let size = u32::try_from(size).map_err(|_| HvfSnapshotV2BuildError::Fdt)?;
        if size == 0 || u64::from(size) > aarch64::FDT_MAX_SIZE {
            return Err(HvfSnapshotV2BuildError::Fdt);
        }
        Ok(Self {
            address,
            size,
            checksum,
        })
    }

    /// Return the guest-physical FDT address.
    pub const fn address(self) -> GuestAddress {
        self.address
    }

    /// Return the exact FDT byte count.
    pub const fn size(self) -> u32 {
        self.size
    }

    /// Return the redacted FDT identity checksum to trusted code.
    pub const fn checksum(self) -> u64 {
        self.checksum
    }
}

impl fmt::Debug for HvfSnapshotV2FdtState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2FdtState")
            .field("placement", &REDACTED)
            .field("checksum", &REDACTED)
            .finish()
    }
}

/// Complete native-v2 machine, logical boot, FDT, and CPU-template facts.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV2MachineState {
    machine: MachineConfig,
    boot: HvfSnapshotV2BootState,
    fdt: HvfSnapshotV2FdtState,
    cpu_template: Option<HvfArm64CpuTemplateApplicationState>,
}

impl HvfSnapshotV2MachineState {
    /// Construct one locally checked machine component value.
    pub fn try_new(
        machine: MachineConfig,
        boot: HvfSnapshotV2BootState,
        fdt: HvfSnapshotV2FdtState,
        cpu_template: Option<HvfArm64CpuTemplateApplicationState>,
    ) -> Result<Self, HvfSnapshotV2BuildError> {
        validate_machine_config(machine)?;
        if cpu_template
            .as_ref()
            .is_some_and(|state| state.entries().is_empty())
        {
            return Err(HvfSnapshotV2BuildError::CpuTemplate);
        }
        Ok(Self {
            machine,
            boot,
            fdt,
            cpu_template,
        })
    }

    /// Return the checked machine configuration.
    pub const fn machine(&self) -> MachineConfig {
        self.machine
    }

    /// Return inert logical boot metadata.
    pub const fn boot(&self) -> &HvfSnapshotV2BootState {
        &self.boot
    }

    /// Return stable FDT placement and identity.
    pub const fn fdt(&self) -> HvfSnapshotV2FdtState {
        self.fdt
    }

    /// Return retained custom CPU-template application evidence.
    pub const fn cpu_template(&self) -> Option<&HvfArm64CpuTemplateApplicationState> {
        self.cpu_template.as_ref()
    }
}

impl fmt::Debug for HvfSnapshotV2MachineState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2MachineState")
            .field("machine", &self.machine)
            .field("boot", &self.boot)
            .field("fdt", &self.fdt)
            .field("cpu_template", &self.cpu_template)
            .finish()
    }
}

/// Common compatibility facts and the singular VM-global GIC state.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV2GlobalState {
    compatibility: HvfSnapshotV1CompatibilityState,
    gic_device: HvfGicDeviceState,
}

impl HvfSnapshotV2GlobalState {
    /// Construct one locally checked common/global component value.
    pub fn try_new(
        compatibility: HvfSnapshotV1CompatibilityState,
        gic_device: HvfGicDeviceState,
    ) -> Result<Self, HvfSnapshotV2BuildError> {
        validate_compatibility(&compatibility)?;
        if gic_device.is_empty() || gic_device.len() > HVF_SNAPSHOT_V2_GIC_DEVICE_STATE_MAX_BYTES {
            return Err(HvfSnapshotV2BuildError::GlobalGic);
        }
        Ok(Self {
            compatibility,
            gic_device,
        })
    }

    /// Return common host-compatibility facts.
    pub const fn compatibility(&self) -> &HvfSnapshotV1CompatibilityState {
        &self.compatibility
    }

    /// Return singular opaque VM-global GIC state.
    pub const fn gic_device(&self) -> &HvfGicDeviceState {
        &self.gic_device
    }

    /// Consume the component into compatibility facts and VM-global GIC state.
    pub fn into_parts(self) -> (HvfSnapshotV1CompatibilityState, HvfGicDeviceState) {
        (self.compatibility, self.gic_device)
    }
}

impl fmt::Debug for HvfSnapshotV2GlobalState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2GlobalState")
            .field("compatibility", &REDACTED)
            .field("gic_device_bytes", &self.gic_device.len())
            .finish()
    }
}

/// Complete typed state for one native-v2 vCPU component.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV2VcpuState {
    index: u32,
    mpidr: u64,
    mandatory: HvfSnapshotV1VcpuState,
    timer: HvfArm64SnapshotTimerState,
    pending_interrupts: HvfArm64VcpuPendingInterruptState,
    gic_icc: HvfArm64GicIccRegisterState,
    reviewed_optional: HvfArm64ReviewedOptionalStateRestore,
}

impl HvfSnapshotV2VcpuState {
    /// Construct one locally checked per-vCPU component value.
    pub fn try_new(
        index: u32,
        mpidr: u64,
        mandatory: HvfSnapshotV1VcpuState,
        timer: HvfArm64SnapshotTimerState,
        pending_interrupts: HvfArm64VcpuPendingInterruptState,
        gic_icc: HvfArm64GicIccRegisterState,
        reviewed_optional: HvfArm64ReviewedOptionalStateRestore,
    ) -> Result<Self, HvfSnapshotV2BuildError> {
        if index >= u32::from(MAX_SUPPORTED_VCPUS)
            || mpidr != u64::from(index)
            || reviewed_optional.simd_fp() != &mandatory.simd_fp
        {
            return Err(HvfSnapshotV2BuildError::Vcpu);
        }
        Ok(Self {
            index,
            mpidr,
            mandatory,
            timer,
            pending_interrupts,
            gic_icc,
            reviewed_optional,
        })
    }

    /// Return the canonical vCPU index.
    pub const fn index(&self) -> u32 {
        self.index
    }

    /// Return the canonical MPIDR.
    pub const fn mpidr(&self) -> u64 {
        self.mpidr
    }

    /// Return complete mandatory architectural state.
    pub const fn mandatory(&self) -> &HvfSnapshotV1VcpuState {
        &self.mandatory
    }

    /// Return normalized timer state.
    pub const fn timer(&self) -> &HvfArm64SnapshotTimerState {
        &self.timer
    }

    /// Return pending IRQ/FIQ state.
    pub const fn pending_interrupts(&self) -> HvfArm64VcpuPendingInterruptState {
        self.pending_interrupts
    }

    /// Return vCPU-affine GIC ICC state.
    pub const fn gic_icc(&self) -> HvfArm64GicIccRegisterState {
        self.gic_icc
    }

    /// Return the checked reviewed optional state.
    pub const fn reviewed_optional(&self) -> &HvfArm64ReviewedOptionalStateRestore {
        &self.reviewed_optional
    }

    /// Consume the component into its canonical index, affinity, and state families.
    pub fn into_parts(
        self,
    ) -> (
        u32,
        u64,
        HvfSnapshotV1VcpuState,
        HvfArm64SnapshotTimerState,
        HvfArm64VcpuPendingInterruptState,
        HvfArm64GicIccRegisterState,
        HvfArm64ReviewedOptionalStateRestore,
    ) {
        (
            self.index,
            self.mpidr,
            self.mandatory,
            self.timer,
            self.pending_interrupts,
            self.gic_icc,
            self.reviewed_optional,
        )
    }
}

impl fmt::Debug for HvfSnapshotV2VcpuState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2VcpuState")
            .field("index", &self.index)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Portable PL031 reconstruction policy for native-v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV2RtcRestorePolicy {
    /// Reset mutable PL031 state and anchor it to destination `SystemTime`.
    DestinationSystemTimeReset,
}

/// Portable arm64 PVTime downtime policy for native-v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV2PvTimeRestorePolicy {
    /// Preserve cumulative stolen time and exclude paused snapshot downtime.
    PreserveCumulativeExcludeDowntime,
}

/// Portable VMGenID reconstruction policy for native-v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV2VmGenIdRestorePolicy {
    /// Generate, publish, and notify a fresh destination identity.
    RegenerateAndNotify,
}

/// Portable VMClock reconstruction policy for native-v2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV2VmClockRestorePolicy {
    /// Atomically increment saved disruption/generation state and notify.
    IncrementAndNotify,
}

/// One topology-ordered arm64 PVTime capture retained by native-v2.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfSnapshotV2PvTimeVcpuState {
    index: u32,
    record_ipa: GuestAddress,
    stolen_time_ns: u64,
}

impl HvfSnapshotV2PvTimeVcpuState {
    /// Construct one locally checked per-vCPU PVTime value.
    pub fn try_new(
        index: u32,
        record_ipa: GuestAddress,
        stolen_time_ns: u64,
    ) -> Result<Self, HvfSnapshotV2BuildError> {
        if index >= u32::from(MAX_SUPPORTED_VCPUS)
            || !record_ipa
                .raw_value()
                .is_multiple_of(ARM64_PVTIME_STRUCTURE_ALIGNMENT)
        {
            return Err(HvfSnapshotV2BuildError::Time);
        }
        Ok(Self {
            index,
            record_ipa,
            stolen_time_ns,
        })
    }

    /// Return the canonical vCPU index.
    pub const fn index(self) -> u32 {
        self.index
    }

    /// Return the guest-physical standard stolen-time record address.
    pub const fn record_ipa(self) -> GuestAddress {
        self.record_ipa
    }

    /// Return the captured cumulative stolen time.
    pub const fn stolen_time_ns(self) -> u64 {
        self.stolen_time_ns
    }
}

impl fmt::Debug for HvfSnapshotV2PvTimeVcpuState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2PvTimeVcpuState")
            .field("index", &self.index)
            .field("state", &REDACTED)
            .finish()
    }
}

/// Complete portable native-v2 time and clone-identity state.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV2TimeState {
    rtc_layout: RtcMmioLayout,
    vmgenid: SnapshotV1PlatformDeviceMetadata,
    vmclock: SnapshotV1PlatformDeviceMetadata,
    vmclock_abi: VmClockAbi,
    pvtime_vcpus: Vec<HvfSnapshotV2PvTimeVcpuState>,
}

impl HvfSnapshotV2TimeState {
    /// Construct one locally checked time component.
    pub fn try_new(
        rtc_layout: RtcMmioLayout,
        vmgenid: SnapshotV1PlatformDeviceMetadata,
        vmclock: SnapshotV1PlatformDeviceMetadata,
        vmclock_abi: VmClockAbi,
        pvtime_vcpus: Vec<HvfSnapshotV2PvTimeVcpuState>,
    ) -> Result<Self, HvfSnapshotV2BuildError> {
        let state = Self {
            rtc_layout,
            vmgenid,
            vmclock,
            vmclock_abi,
            pvtime_vcpus,
        };
        validate_time(&state)?;
        Ok(state)
    }

    /// Return the destination-owned PL031 placement.
    pub const fn rtc_layout(&self) -> RtcMmioLayout {
        self.rtc_layout
    }

    /// Return the supported PL031 restore policy.
    pub const fn rtc_restore_policy(&self) -> HvfSnapshotV2RtcRestorePolicy {
        HvfSnapshotV2RtcRestorePolicy::DestinationSystemTimeReset
    }

    /// Return portable VMGenID placement and notification metadata.
    pub const fn vmgenid(&self) -> SnapshotV1PlatformDeviceMetadata {
        self.vmgenid
    }

    /// Return the supported VMGenID restore policy.
    pub const fn vmgenid_restore_policy(&self) -> HvfSnapshotV2VmGenIdRestorePolicy {
        HvfSnapshotV2VmGenIdRestorePolicy::RegenerateAndNotify
    }

    /// Return portable VMClock placement and notification metadata.
    pub const fn vmclock(&self) -> SnapshotV1PlatformDeviceMetadata {
        self.vmclock
    }

    /// Return the exact captured VMClock ABI.
    pub const fn vmclock_abi(&self) -> VmClockAbi {
        self.vmclock_abi
    }

    /// Return the supported VMClock restore policy.
    pub const fn vmclock_restore_policy(&self) -> HvfSnapshotV2VmClockRestorePolicy {
        HvfSnapshotV2VmClockRestorePolicy::IncrementAndNotify
    }

    /// Return ordered per-vCPU PVTime values.
    pub fn pvtime_vcpus(&self) -> &[HvfSnapshotV2PvTimeVcpuState] {
        &self.pvtime_vcpus
    }

    /// Return the supported PVTime downtime policy.
    pub const fn pvtime_restore_policy(&self) -> HvfSnapshotV2PvTimeRestorePolicy {
        HvfSnapshotV2PvTimeRestorePolicy::PreserveCumulativeExcludeDowntime
    }

    /// Consume the portable state into placement, ABI, and per-vCPU values.
    pub fn into_parts(
        self,
    ) -> (
        RtcMmioLayout,
        SnapshotV1PlatformDeviceMetadata,
        SnapshotV1PlatformDeviceMetadata,
        VmClockAbi,
        Vec<HvfSnapshotV2PvTimeVcpuState>,
    ) {
        (
            self.rtc_layout,
            self.vmgenid,
            self.vmclock,
            self.vmclock_abi,
            self.pvtime_vcpus,
        )
    }
}

impl fmt::Debug for HvfSnapshotV2TimeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2TimeState")
            .field("vcpu_count", &self.pvtime_vcpus.len())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Completely decoded and cross-validated native-v2 platform graph.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV2PlatformState {
    memory: SnapshotV2MemoryBinding,
    machine: HvfSnapshotV2MachineState,
    global: HvfSnapshotV2GlobalState,
    topology: HvfArm64StablePausedTopologyState,
    vcpus: Vec<HvfSnapshotV2VcpuState>,
    time: HvfSnapshotV2TimeState,
}

impl HvfSnapshotV2PlatformState {
    /// Construct and cross-validate one complete owned platform graph.
    pub fn try_new(
        memory: SnapshotV2MemoryBinding,
        machine: HvfSnapshotV2MachineState,
        global: HvfSnapshotV2GlobalState,
        topology: HvfArm64StablePausedTopologyState,
        vcpus: Vec<HvfSnapshotV2VcpuState>,
        time: HvfSnapshotV2TimeState,
    ) -> Result<Self, HvfSnapshotV2BuildError> {
        let state = Self {
            memory,
            machine,
            global,
            topology,
            vcpus,
            time,
        };
        validate_platform(&state)?;
        Ok(state)
    }

    /// Return the exact memory-image binding.
    pub const fn memory(&self) -> &SnapshotV2MemoryBinding {
        &self.memory
    }

    /// Return machine and inert logical metadata.
    pub const fn machine(&self) -> &HvfSnapshotV2MachineState {
        &self.machine
    }

    /// Return common compatibility and VM-global state.
    pub const fn global(&self) -> &HvfSnapshotV2GlobalState {
        &self.global
    }

    /// Return stable paused topology/lifecycle state.
    pub const fn topology(&self) -> &HvfArm64StablePausedTopologyState {
        &self.topology
    }

    /// Return complete vCPUs in canonical instance order.
    pub fn vcpus(&self) -> &[HvfSnapshotV2VcpuState] {
        &self.vcpus
    }

    /// Return portable time and clone-identity state.
    pub const fn time(&self) -> &HvfSnapshotV2TimeState {
        &self.time
    }

    /// Consume the graph into all canonical semantic components.
    pub fn into_parts(
        self,
    ) -> (
        SnapshotV2MemoryBinding,
        HvfSnapshotV2MachineState,
        HvfSnapshotV2GlobalState,
        HvfArm64StablePausedTopologyState,
        Vec<HvfSnapshotV2VcpuState>,
        HvfSnapshotV2TimeState,
    ) {
        (
            self.memory,
            self.machine,
            self.global,
            self.topology,
            self.vcpus,
            self.time,
        )
    }
}

impl fmt::Debug for HvfSnapshotV2PlatformState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV2PlatformState")
            .field("vcpu_count", &self.vcpus.len())
            .field("state", &REDACTED)
            .finish()
    }
}

/// Value-free rejection while constructing a native-v2 platform graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfSnapshotV2BuildError {
    /// A bounded allocation failed.
    Allocation,
    /// Machine configuration is outside the HVF profile.
    Machine,
    /// Logical boot metadata is invalid.
    BootMetadata,
    /// FDT placement or size is invalid.
    Fdt,
    /// CPU-template retained evidence is invalid.
    CpuTemplate,
    /// Common compatibility metadata is invalid.
    Compatibility,
    /// VM-global GIC state is empty or oversized.
    GlobalGic,
    /// Memory extents disagree with the machine.
    Memory,
    /// Stable topology state is inconsistent.
    Topology,
    /// A per-vCPU value is locally invalid.
    Vcpu,
    /// Reviewed optional state is invalid.
    Optional,
    /// Time or clone-identity state is locally invalid.
    Time,
    /// Two otherwise valid components disagree.
    CrossComponent,
}

impl fmt::Display for HvfSnapshotV2BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::Allocation => "allocation",
            Self::Machine => "machine",
            Self::BootMetadata => "boot metadata",
            Self::Fdt => "FDT",
            Self::CpuTemplate => "CPU template",
            Self::Compatibility => "compatibility",
            Self::GlobalGic => "global GIC",
            Self::Memory => "memory",
            Self::Topology => "topology",
            Self::Vcpu => "vCPU",
            Self::Optional => "optional state",
            Self::Time => "time and clone identity",
            Self::CrossComponent => "cross-component relationship",
        };
        write!(f, "invalid native-v2 HVF platform {category}")
    }
}

impl std::error::Error for HvfSnapshotV2BuildError {}

fn validate_machine_config(machine: MachineConfig) -> Result<(), HvfSnapshotV2BuildError> {
    if machine.smt() || machine.huge_pages() != MachineConfigHugePages::None {
        return Err(HvfSnapshotV2BuildError::Machine);
    }
    if machine
        .cpu_template()
        .is_some_and(|template| template != MachineConfigCpuTemplate::V1N1)
    {
        return Err(HvfSnapshotV2BuildError::Machine);
    }
    Ok(())
}

fn validate_time(state: &HvfSnapshotV2TimeState) -> Result<(), HvfSnapshotV2BuildError> {
    if state.pvtime_vcpus.is_empty()
        || state.pvtime_vcpus.len() > usize::from(MAX_SUPPORTED_VCPUS)
        || state.vmgenid.interrupt_line() == state.vmclock.interrupt_line()
        || !platform_metadata_is_locally_valid(state.vmgenid, ARM64_FDT_VMGENID_SIZE)
        || !platform_metadata_is_locally_valid(state.vmclock, ARM64_FDT_VMCLOCK_SIZE)
        || VmClockAbi::from_bytes(state.vmclock_abi.to_bytes()).is_err()
    {
        return Err(HvfSnapshotV2BuildError::Time);
    }
    for (position, vcpu) in state.pvtime_vcpus.iter().enumerate() {
        if usize::try_from(vcpu.index).ok() != Some(position)
            || !vcpu
                .record_ipa
                .raw_value()
                .is_multiple_of(ARM64_PVTIME_STRUCTURE_ALIGNMENT)
        {
            return Err(HvfSnapshotV2BuildError::Time);
        }
    }
    Ok(())
}

fn platform_metadata_is_locally_valid(
    metadata: SnapshotV1PlatformDeviceMetadata,
    expected_size: u64,
) -> bool {
    let range = metadata.range();
    let fdt = metadata.fdt_region();
    range.size() == expected_size
        && fdt.base == range.start().raw_value()
        && fdt.size == range.size()
        && range
            .start()
            .raw_value()
            .checked_add(range.size())
            .is_some()
}

fn validate_platform(state: &HvfSnapshotV2PlatformState) -> Result<(), HvfSnapshotV2BuildError> {
    validate_machine_config(state.machine.machine)?;
    validate_compatibility(&state.global.compatibility)?;
    validate_time(&state.time)?;
    if state.global.gic_device.is_empty()
        || state.global.gic_device.len() > HVF_SNAPSHOT_V2_GIC_DEVICE_STATE_MAX_BYTES
    {
        return Err(HvfSnapshotV2BuildError::GlobalGic);
    }

    let memory_bytes = state
        .machine
        .machine
        .mem_size_mib()
        .checked_mul(MIB)
        .ok_or(HvfSnapshotV2BuildError::Memory)?;
    let layout = aarch64::dram_layout(memory_bytes).map_err(|_| HvfSnapshotV2BuildError::Memory)?;
    if state.memory.extents().len() != layout.ranges().len()
        || state
            .memory
            .extents()
            .iter()
            .zip(layout.ranges())
            .any(|(extent, expected)| extent.range() != *expected)
    {
        return Err(HvfSnapshotV2BuildError::Memory);
    }

    let fdt = state.machine.fdt;
    let expected_fdt = aarch64::fdt_address(&layout).map_err(|_| HvfSnapshotV2BuildError::Fdt)?;
    let fdt_end = fdt
        .address
        .checked_add(u64::from(fdt.size))
        .ok_or(HvfSnapshotV2BuildError::Fdt)?;
    if fdt.address != expected_fdt
        || !layout
            .ranges()
            .iter()
            .any(|range| range.contains(fdt.address) && fdt_end <= range.end_exclusive())
    {
        return Err(HvfSnapshotV2BuildError::Fdt);
    }

    let expected_count = usize::from(state.machine.machine.vcpu_count());
    if state.topology.members().len() != expected_count || state.vcpus.len() != expected_count {
        return Err(HvfSnapshotV2BuildError::CrossComponent);
    }

    let compatibility = &state.global.compatibility;
    let gic = compatibility.gic_metadata();
    let primary = state
        .topology
        .members()
        .first()
        .ok_or(HvfSnapshotV2BuildError::Topology)?;
    if compatibility.primary_mpidr() != primary.mpidr()
        || compatibility.identification().mpidr_el1() != primary.mpidr()
        || gic.timer_interrupts.el1_virtual_timer_intid != state.topology.virtual_timer_intid()
    {
        return Err(HvfSnapshotV2BuildError::CrossComponent);
    }
    let redistributor_capacity = gic
        .redistributor
        .region
        .size
        .checked_div(gic.redistributor.single_redistributor_size)
        .ok_or(HvfSnapshotV2BuildError::Compatibility)?;
    if redistributor_capacity
        < u64::try_from(expected_count).map_err(|_| HvfSnapshotV2BuildError::CrossComponent)?
    {
        return Err(HvfSnapshotV2BuildError::CrossComponent);
    }

    let identification = compatibility.identification();
    let expected_sme_version = sme_version(identification.id_aa64pfr1_el1());
    for (position, (member, vcpu)) in state
        .topology
        .members()
        .iter()
        .zip(&state.vcpus)
        .enumerate()
    {
        if usize::try_from(vcpu.index).ok() != Some(position)
            || vcpu.mpidr != member.mpidr()
            || vcpu.reviewed_optional.expected_id_aa64dfr0_el1() != identification.id_aa64dfr0_el1()
            || vcpu.reviewed_optional.expected_sme_version() != expected_sme_version
            || vcpu.reviewed_optional.simd_fp() != &vcpu.mandatory.simd_fp
        {
            return Err(HvfSnapshotV2BuildError::CrossComponent);
        }
        match (
            vcpu.reviewed_optional.sme(),
            compatibility.optional_sve_sme_identification(),
        ) {
            (Some(sme), Some(common)) if sme.identification() == common => {}
            (None, _) if expected_sme_version.is_none() => {}
            _ => return Err(HvfSnapshotV2BuildError::CrossComponent),
        }
    }
    validate_platform_time(state, &layout)?;
    Ok(())
}

fn validate_platform_time(
    state: &HvfSnapshotV2PlatformState,
    memory_layout: &bangbang_runtime::memory::GuestMemoryLayout,
) -> Result<(), HvfSnapshotV2BuildError> {
    let time = &state.time;
    let expected_count = usize::from(state.machine.machine.vcpu_count());
    if time.pvtime_vcpus.len() != expected_count
        || time.rtc_layout != state.global.compatibility.rtc_mmio_layout()
    {
        return Err(HvfSnapshotV2BuildError::CrossComponent);
    }

    let expected_vmclock = aarch64::SYSTEM_MEM_START
        .checked_add(aarch64::SYSTEM_MEM_SIZE)
        .and_then(|end| end.checked_sub(ARM64_FDT_VMCLOCK_SIZE))
        .ok_or(HvfSnapshotV2BuildError::Time)?;
    let expected_vmgenid = expected_vmclock
        .checked_sub(ARM64_FDT_VMGENID_SIZE)
        .ok_or(HvfSnapshotV2BuildError::Time)?;
    if time.vmclock.range().start().raw_value() != expected_vmclock
        || time.vmgenid.range().start().raw_value() != expected_vmgenid
        || time.vmclock.range().overlaps(time.vmgenid.range())
        || !range_is_backed(memory_layout, time.vmclock.range())
        || !range_is_backed(memory_layout, time.vmgenid.range())
    {
        return Err(HvfSnapshotV2BuildError::CrossComponent);
    }

    let spi = state
        .global
        .compatibility
        .gic_metadata()
        .spi_interrupt_range;
    let spi_end = spi
        .base
        .checked_add(spi.count)
        .ok_or(HvfSnapshotV2BuildError::Compatibility)?;
    for line in [
        time.vmgenid.interrupt_line().raw_value(),
        time.vmclock.interrupt_line().raw_value(),
    ] {
        if line < spi.base || line >= spi_end {
            return Err(HvfSnapshotV2BuildError::CrossComponent);
        }
    }

    let arena_size = expected_vmgenid
        .checked_sub(aarch64::SYSTEM_MEM_START)
        .ok_or(HvfSnapshotV2BuildError::Time)?;
    let arena = GuestMemoryRange::new(GuestAddress::new(aarch64::SYSTEM_MEM_START), arena_size)
        .map_err(|_| HvfSnapshotV2BuildError::Time)?;
    let expected_layout = Arm64PvTimeLayout::plan(state.machine.machine.vcpu_count(), arena)
        .map_err(|_| HvfSnapshotV2BuildError::Time)?;
    for (captured, expected_range) in time
        .pvtime_vcpus
        .iter()
        .zip(expected_layout.records().iter())
    {
        let captured_range = GuestMemoryRange::new(
            captured.record_ipa,
            u64::try_from(ARM64_PVTIME_STRUCTURE_SIZE)
                .map_err(|_| HvfSnapshotV2BuildError::Time)?,
        )
        .map_err(|_| HvfSnapshotV2BuildError::Time)?;
        if captured_range != *expected_range || !range_is_backed(memory_layout, captured_range) {
            return Err(HvfSnapshotV2BuildError::CrossComponent);
        }
    }
    Ok(())
}

fn range_is_backed(
    layout: &bangbang_runtime::memory::GuestMemoryLayout,
    range: GuestMemoryRange,
) -> bool {
    layout.ranges().iter().any(|backing| {
        backing.contains(range.start()) && range.end_exclusive() <= backing.end_exclusive()
    })
}

fn validate_compatibility(
    state: &HvfSnapshotV1CompatibilityState,
) -> Result<(), HvfSnapshotV2BuildError> {
    if state.primary_mpidr() != state.identification().mpidr_el1() {
        return Err(HvfSnapshotV2BuildError::Compatibility);
    }
    let identification = state.identification();
    let sve_present = ((identification.id_aa64pfr0_el1() >> 32) & 0xf) != 0xf;
    let sme_present = sme_version(identification.id_aa64pfr1_el1()).is_some();
    if (sve_present || sme_present) != state.optional_sve_sme_identification().is_some() {
        return Err(HvfSnapshotV2BuildError::Compatibility);
    }

    let gic = state.gic_metadata();
    validate_mmio_region(gic.distributor)?;
    validate_mmio_region(gic.redistributor.region)?;
    if gic.redistributor.single_redistributor_size == 0
        || gic.redistributor.single_redistributor_size > gic.redistributor.region.size
        || !gic
            .redistributor
            .region
            .size
            .is_multiple_of(gic.redistributor.single_redistributor_size)
        || regions_overlap(gic.distributor, gic.redistributor.region)
    {
        return Err(HvfSnapshotV2BuildError::Compatibility);
    }
    validate_interrupt_range(gic.spi_interrupt_range)?;
    validate_gic_ppi_pending_intid(gic.timer_interrupts.el1_virtual_timer_intid)
        .map_err(|_| HvfSnapshotV2BuildError::Compatibility)?;
    validate_gic_ppi_pending_intid(gic.timer_interrupts.el1_physical_timer_intid)
        .map_err(|_| HvfSnapshotV2BuildError::Compatibility)?;
    if gic.timer_interrupts.el1_virtual_timer_intid == gic.timer_interrupts.el1_physical_timer_intid
    {
        return Err(HvfSnapshotV2BuildError::Compatibility);
    }
    if let Some(msi) = gic.msi {
        validate_mmio_region(msi.region)?;
        validate_interrupt_range(msi.interrupt_range)?;
        if regions_overlap(msi.region, gic.distributor)
            || regions_overlap(msi.region, gic.redistributor.region)
            || interrupt_ranges_overlap(msi.interrupt_range, gic.spi_interrupt_range)
        {
            return Err(HvfSnapshotV2BuildError::Compatibility);
        }
    }

    let rtc = HvfGicRegion {
        base: state.rtc_mmio_layout().base().raw_value(),
        size: RTC_MMIO_DEVICE_WINDOW_SIZE,
    };
    validate_mmio_region(rtc)?;
    if regions_overlap(rtc, gic.distributor)
        || regions_overlap(rtc, gic.redistributor.region)
        || gic.msi.is_some_and(|msi| regions_overlap(rtc, msi.region))
    {
        return Err(HvfSnapshotV2BuildError::Compatibility);
    }
    Ok(())
}

fn validate_mmio_region(region: HvfGicRegion) -> Result<(), HvfSnapshotV2BuildError> {
    if region.size == 0
        || region.base.checked_add(region.size).is_none()
        || region.end_exclusive() > aarch64::DRAM_MEM_START
    {
        Err(HvfSnapshotV2BuildError::Compatibility)
    } else {
        Ok(())
    }
}

fn validate_interrupt_range(range: HvfGicInterruptRange) -> Result<(), HvfSnapshotV2BuildError> {
    if range.base < 32 || range.count == 0 || range.base.checked_add(range.count).is_none() {
        Err(HvfSnapshotV2BuildError::Compatibility)
    } else {
        Ok(())
    }
}

fn regions_overlap(first: HvfGicRegion, second: HvfGicRegion) -> bool {
    first.base < second.end_exclusive() && second.base < first.end_exclusive()
}

fn interrupt_ranges_overlap(first: HvfGicInterruptRange, second: HvfGicInterruptRange) -> bool {
    let first_end = first.base.saturating_add(first.count);
    let second_end = second.base.saturating_add(second.count);
    first.base < second_end && second.base < first_end
}

const fn sme_version(id_aa64pfr1_el1: u64) -> Option<u8> {
    let version = ((id_aa64pfr1_el1 >> 24) & 0xf) as u8;
    if version == 0xf { None } else { Some(version) }
}

fn copy_boxed(bytes: &[u8]) -> Result<Box<[u8]>, TryReserveError> {
    let mut value = Vec::new();
    value.try_reserve_exact(bytes.len())?;
    value.extend_from_slice(bytes);
    Ok(value.into_boxed_slice())
}

fn copy_string(value: &str) -> Result<Box<str>, TryReserveError> {
    let mut copy = String::new();
    copy.try_reserve_exact(value.len())?;
    copy.push_str(value);
    Ok(copy.into_boxed_str())
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn with_capacity(capacity: usize) -> Result<Self, TryReserveError> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(capacity)?;
        Ok(Self { bytes })
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u128(&mut self, value: u128) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    fn zeroes(&mut self, count: usize) {
        self.bytes.resize(self.bytes.len() + count, 0);
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

#[derive(Clone, Copy)]
struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn slice(&mut self, length: usize) -> Result<&'a [u8], HvfSnapshotV2DecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(HvfSnapshotV2DecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], HvfSnapshotV2DecodeError> {
        self.slice(N)?
            .try_into()
            .map_err(|_| HvfSnapshotV2DecodeError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, HvfSnapshotV2DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn bool(&mut self) -> Result<bool, HvfSnapshotV2DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(HvfSnapshotV2DecodeError::InvalidBoolean),
        }
    }

    fn u16(&mut self) -> Result<u16, HvfSnapshotV2DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, HvfSnapshotV2DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, HvfSnapshotV2DecodeError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn u128(&mut self) -> Result<u128, HvfSnapshotV2DecodeError> {
        Ok(u128::from_le_bytes(self.array()?))
    }

    fn zeroes(&mut self, count: usize) -> Result<(), HvfSnapshotV2DecodeError> {
        if self.slice(count)?.iter().any(|byte| *byte != 0) {
            Err(HvfSnapshotV2DecodeError::NonzeroReserved)
        } else {
            Ok(())
        }
    }

    fn finish(self) -> Result<(), HvfSnapshotV2DecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(HvfSnapshotV2DecodeError::TrailingData)
        }
    }
}

/// Failure while encoding one complete native-v2 HVF platform graph.
pub enum HvfSnapshotV2EncodeError {
    /// Whole-graph validation rejected trusted input.
    Build(HvfSnapshotV2BuildError),
    /// Memory binding encoding failed.
    Memory(SnapshotV2MemoryBindingError),
    /// Nested mandatory-vCPU encoding failed.
    Mandatory(HvfSnapshotV1EncodeError),
    /// A bounded component allocation failed.
    Allocation(TryReserveError),
    /// A component length cannot be represented.
    LengthOverflow,
    /// The structural native-v2 container rejected the components.
    Container(SnapshotV2EncodeError),
}

impl fmt::Debug for HvfSnapshotV2EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for HvfSnapshotV2EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Build(_) => f.write_str("native-v2 HVF platform graph is invalid"),
            Self::Memory(_) => f.write_str("native-v2 memory binding encoding failed"),
            Self::Mandatory(_) => f.write_str("native-v2 mandatory vCPU state encoding failed"),
            Self::Allocation(_) => f.write_str("native-v2 HVF component allocation failed"),
            Self::LengthOverflow => {
                f.write_str("native-v2 HVF component length arithmetic overflowed")
            }
            Self::Container(_) => f.write_str("native-v2 structural container encoding failed"),
        }
    }
}

impl std::error::Error for HvfSnapshotV2EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Build(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::Mandatory(source) => Some(source),
            Self::Allocation(source) => Some(source),
            Self::Container(source) => Some(source),
            Self::LengthOverflow => None,
        }
    }
}

/// Failure while decoding or cross-validating native-v2 HVF platform state.
pub enum HvfSnapshotV2DecodeError {
    /// The compatible structural state predates the platform profile.
    UnsupportedProfile,
    /// The outer component graph is not the exact native-v2 platform profile.
    InvalidComponentProfile,
    /// A component ends before a required field.
    Truncated,
    /// A component carries bytes after its canonical end.
    TrailingData,
    /// A component magic or profile header is invalid.
    InvalidHeader,
    /// A boolean field is not zero or one.
    InvalidBoolean,
    /// A reserved field is nonzero.
    NonzeroReserved,
    /// A count or byte length is invalid or overflows.
    InvalidLength,
    /// Machine, boot, FDT, or CPU state is invalid.
    InvalidMachine,
    /// Common compatibility or global GIC state is invalid.
    InvalidGlobal,
    /// Stable topology payload is invalid.
    InvalidTopology,
    /// Per-vCPU mandatory/timer/interrupt state is invalid.
    InvalidVcpu,
    /// Time or clone-identity component is invalid.
    InvalidTime,
    /// The reviewed optional registry is invalid.
    InvalidOptional,
    /// A bounded typed allocation failed.
    Allocation(TryReserveError),
    /// Memory binding decoding failed.
    Memory(SnapshotV2MemoryStateError),
    /// Nested mandatory-vCPU decoding failed.
    Mandatory(HvfSnapshotV1DecodeError),
    /// A complete locally valid graph failed cross-validation.
    Build(HvfSnapshotV2BuildError),
}

impl fmt::Debug for HvfSnapshotV2DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for HvfSnapshotV2DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedProfile => "native-v2 state has no HVF platform profile",
            Self::InvalidComponentProfile => "native-v2 HVF component profile is invalid",
            Self::Truncated => "native-v2 HVF component is truncated",
            Self::TrailingData => "native-v2 HVF component has trailing data",
            Self::InvalidHeader => "native-v2 HVF component header is invalid",
            Self::InvalidBoolean => "native-v2 HVF boolean is invalid",
            Self::NonzeroReserved => "native-v2 HVF reserved field is nonzero",
            Self::InvalidLength => "native-v2 HVF count or length is invalid",
            Self::InvalidMachine => "native-v2 HVF machine component is invalid",
            Self::InvalidGlobal => "native-v2 HVF global component is invalid",
            Self::InvalidTopology => "native-v2 HVF topology component is invalid",
            Self::InvalidVcpu => "native-v2 HVF vCPU component is invalid",
            Self::InvalidTime => "native-v2 HVF time component is invalid",
            Self::InvalidOptional => "native-v2 HVF optional registry is invalid",
            Self::Allocation(_) => "native-v2 HVF typed allocation failed",
            Self::Memory(_) => "native-v2 HVF memory binding is invalid",
            Self::Mandatory(_) => "native-v2 HVF mandatory vCPU state is invalid",
            Self::Build(_) => "native-v2 HVF platform graph is inconsistent",
        };
        f.write_str(message)
    }
}

impl std::error::Error for HvfSnapshotV2DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(source) => Some(source),
            Self::Memory(source) => Some(source),
            Self::Mandatory(source) => Some(source),
            Self::Build(source) => Some(source),
            _ => None,
        }
    }
}

/// Encode one complete canonical native-v2 HVF platform graph.
pub fn encode_hvf_snapshot_v2_platform_state(
    state: &HvfSnapshotV2PlatformState,
) -> Result<Vec<u8>, HvfSnapshotV2EncodeError> {
    validate_platform(state).map_err(HvfSnapshotV2EncodeError::Build)?;
    let memory = state
        .memory
        .encode()
        .map_err(HvfSnapshotV2EncodeError::Memory)?;
    let machine = encode_machine(&state.machine)?;
    let global = encode_global(&state.global)?;
    let topology = encode_topology(&state.topology)?;
    let time = encode_time(&state.time)?;
    let mut vcpu_payloads = Vec::new();
    vcpu_payloads
        .try_reserve_exact(state.vcpus.len())
        .map_err(HvfSnapshotV2EncodeError::Allocation)?;
    for vcpu in &state.vcpus {
        vcpu_payloads.push(encode_platform_vcpu(vcpu)?);
    }

    let component_count = 5_usize
        .checked_add(vcpu_payloads.len())
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let mut components = Vec::new();
    components
        .try_reserve_exact(component_count)
        .map_err(HvfSnapshotV2EncodeError::Allocation)?;
    for (key, payload) in [
        (NATIVE_V2_MEMORY_COMPONENT_KEY, memory.as_slice()),
        (NATIVE_V2_MACHINE_COMPONENT_KEY, machine.as_slice()),
        (NATIVE_V2_GLOBAL_COMPONENT_KEY, global.as_slice()),
        (NATIVE_V2_TOPOLOGY_COMPONENT_KEY, topology.as_slice()),
    ] {
        components.push(SnapshotV2Component::new(
            key,
            SnapshotV2ComponentDisposition::Semantic,
            payload,
        ));
    }
    for (index, payload) in vcpu_payloads.iter().enumerate() {
        components.push(SnapshotV2Component::new(
            native_v2_vcpu_component_key(
                u32::try_from(index).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
            ),
            SnapshotV2ComponentDisposition::Semantic,
            payload,
        ));
    }
    components.push(SnapshotV2Component::new(
        NATIVE_V2_TIME_COMPONENT_KEY,
        SnapshotV2ComponentDisposition::Semantic,
        &time,
    ));
    encode_snapshot_v2_state(&[], &components).map_err(HvfSnapshotV2EncodeError::Container)
}

/// Decode, own, and cross-validate one native-v2 HVF platform graph.
///
/// The exact directory profile is checked without payload-dependent
/// allocation before any typed component decoder is invoked.
pub fn decode_hvf_snapshot_v2_platform_state(
    state: &SnapshotV2State<'_>,
) -> Result<HvfSnapshotV2PlatformState, HvfSnapshotV2DecodeError> {
    let vcpu_count = scan_component_profile(state)?;
    let memory =
        decode_snapshot_v2_memory_binding(state).map_err(HvfSnapshotV2DecodeError::Memory)?;
    let machine = decode_machine(component_payload(state, NATIVE_V2_MACHINE_COMPONENT_KEY)?)?;
    let global = decode_global(component_payload(state, NATIVE_V2_GLOBAL_COMPONENT_KEY)?)?;
    let topology = decode_topology(component_payload(state, NATIVE_V2_TOPOLOGY_COMPONENT_KEY)?)?;
    let time = decode_time(component_payload(state, NATIVE_V2_TIME_COMPONENT_KEY)?)?;

    let mut vcpus = Vec::new();
    vcpus
        .try_reserve_exact(vcpu_count)
        .map_err(HvfSnapshotV2DecodeError::Allocation)?;
    for index in 0..vcpu_count {
        let key = native_v2_vcpu_component_key(
            u32::try_from(index).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?,
        );
        vcpus.push(decode_platform_vcpu(component_payload(state, key)?)?);
    }
    HvfSnapshotV2PlatformState::try_new(memory, machine, global, topology, vcpus, time)
        .map_err(HvfSnapshotV2DecodeError::Build)
}

fn scan_component_profile(state: &SnapshotV2State<'_>) -> Result<usize, HvfSnapshotV2DecodeError> {
    if state.metadata().version().minor() < 3 {
        return Err(HvfSnapshotV2DecodeError::UnsupportedProfile);
    }
    let count = usize::try_from(state.metadata().component_count())
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidComponentProfile)?;
    let max_count = 5_usize + usize::from(MAX_SUPPORTED_VCPUS);
    if !(6..=max_count).contains(&count) {
        return Err(HvfSnapshotV2DecodeError::InvalidComponentProfile);
    }
    let vcpu_count = count - 5;
    for (position, component) in state.components().enumerate() {
        if component.disposition() != SnapshotV2ComponentDisposition::Semantic {
            return Err(HvfSnapshotV2DecodeError::InvalidComponentProfile);
        }
        let expected = match position {
            0 => NATIVE_V2_MEMORY_COMPONENT_KEY,
            1 => NATIVE_V2_MACHINE_COMPONENT_KEY,
            2 => NATIVE_V2_GLOBAL_COMPONENT_KEY,
            3 => NATIVE_V2_TOPOLOGY_COMPONENT_KEY,
            position if position < 4 + vcpu_count => native_v2_vcpu_component_key(
                u32::try_from(position - 4)
                    .map_err(|_| HvfSnapshotV2DecodeError::InvalidComponentProfile)?,
            ),
            _ => NATIVE_V2_TIME_COMPONENT_KEY,
        };
        if component.key() != expected {
            return Err(HvfSnapshotV2DecodeError::InvalidComponentProfile);
        }
    }
    Ok(vcpu_count)
}

fn component_payload<'a>(
    state: &'a SnapshotV2State<'a>,
    key: bangbang_runtime::snapshot_format_v2::SnapshotV2ComponentKey,
) -> Result<&'a [u8], HvfSnapshotV2DecodeError> {
    state
        .component(key)
        .map(SnapshotV2Component::payload)
        .ok_or(HvfSnapshotV2DecodeError::InvalidComponentProfile)
}

fn encode_machine(state: &HvfSnapshotV2MachineState) -> Result<Vec<u8>, HvfSnapshotV2EncodeError> {
    let kernel = state.boot.kernel_path.as_bytes();
    let initrd = state
        .boot
        .initrd_path
        .as_ref()
        .map(HvfSnapshotV2NativePath::as_bytes);
    let arguments = state.boot.boot_arguments.as_deref().map(str::as_bytes);
    let cpu_entries = state
        .cpu_template
        .as_ref()
        .map(HvfArm64CpuTemplateApplicationState::entries)
        .unwrap_or_default();
    let cpu_bytes = cpu_entries
        .len()
        .checked_mul(MACHINE_CPU_ENTRY_BYTES)
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let capacity = MACHINE_HEADER_BYTES
        .checked_add(kernel.len())
        .and_then(|value| value.checked_add(initrd.map_or(0, <[u8]>::len)))
        .and_then(|value| value.checked_add(arguments.map_or(0, <[u8]>::len)))
        .and_then(|value| value.checked_add(cpu_bytes))
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let mut encoder =
        Encoder::with_capacity(capacity).map_err(HvfSnapshotV2EncodeError::Allocation)?;
    encoder.bytes(&MACHINE_MAGIC);
    encoder.u16(COMPONENT_PROFILE);
    encoder.u16(
        u16::try_from(MACHINE_HEADER_BYTES)
            .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder.u32(COMPONENT_FLAGS);
    encoder.u8(state.machine.vcpu_count());
    encoder.bool(state.machine.smt());
    encoder.bool(state.machine.track_dirty_pages());
    encoder.u8(match state.machine.huge_pages() {
        MachineConfigHugePages::None => 0,
        MachineConfigHugePages::TwoM => {
            return Err(HvfSnapshotV2EncodeError::Build(
                HvfSnapshotV2BuildError::Machine,
            ));
        }
    });
    encoder.u8(match state.machine.cpu_template() {
        None => 0,
        Some(MachineConfigCpuTemplate::V1N1) => 1,
        Some(_) => {
            return Err(HvfSnapshotV2EncodeError::Build(
                HvfSnapshotV2BuildError::Machine,
            ));
        }
    });
    encoder.bool(initrd.is_some());
    encoder.bool(arguments.is_some());
    encoder.bool(state.cpu_template.is_some());
    encoder.u64(state.machine.mem_size_mib());
    for length in [
        kernel.len(),
        initrd.map_or(0, <[u8]>::len),
        arguments.map_or(0, <[u8]>::len),
        cpu_entries.len(),
    ] {
        encoder.u32(u32::try_from(length).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?);
    }
    encoder.u64(state.fdt.address.raw_value());
    encoder.u32(state.fdt.size);
    encoder.u32(0);
    encoder.u64(state.fdt.checksum);
    encoder.u64(0);
    debug_assert_eq!(encoder.len(), MACHINE_HEADER_BYTES);
    encoder.bytes(kernel);
    if let Some(initrd) = initrd {
        encoder.bytes(initrd);
    }
    if let Some(arguments) = arguments {
        encoder.bytes(arguments);
    }
    for entry in cpu_entries {
        encoder.u16(entry.tag().raw());
        encoder.u8(match entry.width() {
            HvfArm64CpuTemplateValueWidth::U32 => 1,
            HvfArm64CpuTemplateValueWidth::U64 => 2,
            HvfArm64CpuTemplateValueWidth::U128 => 3,
        });
        encoder.zeroes(1);
        encoder.u32(0);
        for value in [
            entry.filter(),
            entry.logical_value(),
            entry.common_baseline(),
            entry.effective_value(),
        ] {
            encoder.u128(value.zero_extended());
        }
    }
    debug_assert_eq!(encoder.len(), capacity);
    Ok(encoder.finish())
}

fn decode_machine(payload: &[u8]) -> Result<HvfSnapshotV2MachineState, HvfSnapshotV2DecodeError> {
    if payload.len() < MACHINE_HEADER_BYTES {
        return Err(HvfSnapshotV2DecodeError::Truncated);
    }
    let mut decoder = Decoder::new(payload);
    if decoder.array::<8>()? != MACHINE_MAGIC
        || decoder.u16()? != COMPONENT_PROFILE
        || usize::from(decoder.u16()?) != MACHINE_HEADER_BYTES
        || decoder.u32()? != COMPONENT_FLAGS
    {
        return Err(HvfSnapshotV2DecodeError::InvalidHeader);
    }
    let vcpu_count = decoder.u8()?;
    let smt = decoder.bool()?;
    let track_dirty_pages = decoder.bool()?;
    if decoder.u8()? != 0 {
        return Err(HvfSnapshotV2DecodeError::InvalidMachine);
    }
    let cpu_template = match decoder.u8()? {
        0 => None,
        1 => Some(MachineConfigCpuTemplate::V1N1),
        _ => return Err(HvfSnapshotV2DecodeError::InvalidMachine),
    };
    let initrd_present = decoder.bool()?;
    let arguments_present = decoder.bool()?;
    let application_present = decoder.bool()?;
    let memory_mib = decoder.u64()?;
    let kernel_length =
        decode_bounded_length(decoder.u32()?, HVF_SNAPSHOT_V2_MAX_PATH_BYTES, false, false)?;
    let initrd_length = decode_bounded_length(
        decoder.u32()?,
        HVF_SNAPSHOT_V2_MAX_PATH_BYTES,
        !initrd_present,
        false,
    )?;
    let arguments_length = decode_bounded_length(
        decoder.u32()?,
        HVF_SNAPSHOT_V2_MAX_BOOT_ARGUMENT_BYTES,
        !arguments_present,
        true,
    )?;
    let cpu_count =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?;
    if cpu_count > HVF_ARM64_CPU_TEMPLATE_APPLICATION_MAX_ENTRIES
        || application_present != (cpu_count != 0)
    {
        return Err(HvfSnapshotV2DecodeError::InvalidMachine);
    }
    let fdt_address = GuestAddress::new(decoder.u64()?);
    let fdt_size =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?;
    decoder.zeroes(4)?;
    let fdt_checksum = decoder.u64()?;
    decoder.zeroes(8)?;
    debug_assert_eq!(decoder.position, MACHINE_HEADER_BYTES);

    let cpu_bytes = cpu_count
        .checked_mul(MACHINE_CPU_ENTRY_BYTES)
        .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?;
    let expected_length = MACHINE_HEADER_BYTES
        .checked_add(kernel_length)
        .and_then(|value| value.checked_add(initrd_length))
        .and_then(|value| value.checked_add(arguments_length))
        .and_then(|value| value.checked_add(cpu_bytes))
        .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?;
    if payload.len() != expected_length {
        return Err(HvfSnapshotV2DecodeError::InvalidLength);
    }

    let kernel = HvfSnapshotV2NativePath::try_from_bytes(decoder.slice(kernel_length)?)
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidMachine)?;
    let initrd = if initrd_present {
        Some(
            HvfSnapshotV2NativePath::try_from_bytes(decoder.slice(initrd_length)?)
                .map_err(|_| HvfSnapshotV2DecodeError::InvalidMachine)?,
        )
    } else {
        None
    };
    let arguments = if arguments_present {
        Some(
            std::str::from_utf8(decoder.slice(arguments_length)?)
                .map_err(|_| HvfSnapshotV2DecodeError::InvalidMachine)?,
        )
    } else {
        None
    };
    let boot = HvfSnapshotV2BootState::try_new(kernel, initrd, arguments)
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidMachine)?;

    let mut entries = Vec::new();
    entries
        .try_reserve_exact(cpu_count)
        .map_err(HvfSnapshotV2DecodeError::Allocation)?;
    for _ in 0..cpu_count {
        let tag = decoder.u16()?;
        let width = match decoder.u8()? {
            1 => HvfArm64CpuTemplateValueWidth::U32,
            2 => HvfArm64CpuTemplateValueWidth::U64,
            3 => HvfArm64CpuTemplateValueWidth::U128,
            _ => return Err(HvfSnapshotV2DecodeError::InvalidMachine),
        };
        decoder.zeroes(1)?;
        decoder.zeroes(4)?;
        entries.push(
            HvfArm64CpuTemplateApplicationEntry::try_from_stable_values(
                tag,
                width,
                decoder.u128()?,
                decoder.u128()?,
                decoder.u128()?,
                decoder.u128()?,
            )
            .map_err(|_| HvfSnapshotV2DecodeError::InvalidMachine)?,
        );
    }
    decoder.finish()?;
    let application = if application_present {
        Some(
            HvfArm64CpuTemplateApplicationState::try_new(entries)
                .map_err(|_| HvfSnapshotV2DecodeError::InvalidMachine)?,
        )
    } else {
        None
    };
    let mut machine = MachineConfigInput::new(vcpu_count, memory_mib)
        .with_smt(smt)
        .with_track_dirty_pages(track_dirty_pages);
    if let Some(cpu_template) = cpu_template {
        machine = machine.with_cpu_template(cpu_template);
    }
    let machine = machine
        .validate()
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidMachine)?;
    let fdt = HvfSnapshotV2FdtState::try_new(fdt_address, fdt_size, fdt_checksum)
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidMachine)?;
    HvfSnapshotV2MachineState::try_new(machine, boot, fdt, application)
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidMachine)
}

fn decode_bounded_length(
    encoded: u32,
    maximum: usize,
    require_zero: bool,
    allow_zero: bool,
) -> Result<usize, HvfSnapshotV2DecodeError> {
    let length = usize::try_from(encoded).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?;
    if length > maximum
        || require_zero && length != 0
        || !require_zero && !allow_zero && length == 0
    {
        Err(HvfSnapshotV2DecodeError::InvalidLength)
    } else {
        Ok(length)
    }
}

fn encode_global(state: &HvfSnapshotV2GlobalState) -> Result<Vec<u8>, HvfSnapshotV2EncodeError> {
    let gic_bytes = state.gic_device.as_bytes();
    let capacity = GLOBAL_HEADER_BYTES
        .checked_add(GLOBAL_COMPATIBILITY_BYTES)
        .and_then(|value| value.checked_add(gic_bytes.len()))
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let mut encoder =
        Encoder::with_capacity(capacity).map_err(HvfSnapshotV2EncodeError::Allocation)?;
    encoder.bytes(&GLOBAL_MAGIC);
    encoder.u16(COMPONENT_PROFILE);
    encoder.u16(
        u16::try_from(GLOBAL_HEADER_BYTES).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder.u32(COMPONENT_FLAGS);
    encoder
        .u32(u32::try_from(gic_bytes.len()).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?);
    encoder.u32(0);
    encode_compatibility(&mut encoder, &state.compatibility);
    debug_assert_eq!(
        encoder.len(),
        GLOBAL_HEADER_BYTES + GLOBAL_COMPATIBILITY_BYTES
    );
    encoder.bytes(gic_bytes);
    debug_assert_eq!(encoder.len(), capacity);
    Ok(encoder.finish())
}

fn encode_compatibility(encoder: &mut Encoder, state: &HvfSnapshotV1CompatibilityState) {
    let identification = state.identification();
    for value in [
        identification.midr_el1(),
        identification.mpidr_el1(),
        identification.id_aa64pfr0_el1(),
        identification.id_aa64pfr1_el1(),
        identification.id_aa64dfr0_el1(),
        identification.id_aa64dfr1_el1(),
        identification.id_aa64isar0_el1(),
        identification.id_aa64isar1_el1(),
        identification.id_aa64mmfr0_el1(),
        identification.id_aa64mmfr1_el1(),
        identification.id_aa64mmfr2_el1(),
    ] {
        encoder.u64(value);
    }
    if let Some(optional) = state.optional_sve_sme_identification() {
        encoder.bool(true);
        encoder.zeroes(7);
        encoder.u64(optional.id_aa64zfr0_el1());
        encoder.u64(optional.id_aa64smfr0_el1());
    } else {
        encoder.bool(false);
        encoder.zeroes(7 + 16);
    }
    let cache = state.cache_manifest();
    for value in [
        cache.configuration().ctr_el0(),
        cache.configuration().clidr_el1(),
        cache.configuration().dczid_el0(),
    ] {
        encoder.u64(value);
    }
    for value in cache.geometry().data_or_unified_ccsidr_el1() {
        encoder.u64(*value);
    }
    for value in cache.geometry().instruction_ccsidr_el1() {
        encoder.u64(*value);
    }
    encoder.u64(state.primary_mpidr());

    let gic = state.gic_metadata();
    encode_gic_region(encoder, gic.distributor);
    encode_gic_region(encoder, gic.redistributor.region);
    encoder.u64(gic.redistributor.single_redistributor_size);
    encode_interrupt_range(encoder, gic.spi_interrupt_range);
    encoder.u32(gic.timer_interrupts.el1_virtual_timer_intid);
    encoder.u32(gic.timer_interrupts.el1_physical_timer_intid);
    if let Some(msi) = gic.msi {
        encoder.bool(true);
        encoder.zeroes(7);
        encode_gic_region(encoder, msi.region);
        encode_interrupt_range(encoder, msi.interrupt_range);
    } else {
        encoder.bool(false);
        encoder.zeroes(7 + 16 + 8);
    }
    encoder.u64(state.rtc_mmio_layout().base().raw_value());
    encoder.u64(state.rtc_mmio_layout().region_id().raw_value());
}

fn decode_global(payload: &[u8]) -> Result<HvfSnapshotV2GlobalState, HvfSnapshotV2DecodeError> {
    let minimum = GLOBAL_HEADER_BYTES
        .checked_add(GLOBAL_COMPATIBILITY_BYTES)
        .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?;
    if payload.len() < minimum {
        return Err(HvfSnapshotV2DecodeError::Truncated);
    }
    let mut decoder = Decoder::new(payload);
    if decoder.array::<8>()? != GLOBAL_MAGIC
        || decoder.u16()? != COMPONENT_PROFILE
        || usize::from(decoder.u16()?) != GLOBAL_HEADER_BYTES
        || decoder.u32()? != COMPONENT_FLAGS
    {
        return Err(HvfSnapshotV2DecodeError::InvalidHeader);
    }
    let gic_length =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?;
    decoder.zeroes(4)?;
    if gic_length == 0 || gic_length > HVF_SNAPSHOT_V2_GIC_DEVICE_STATE_MAX_BYTES {
        return Err(HvfSnapshotV2DecodeError::InvalidGlobal);
    }
    let expected_length = minimum
        .checked_add(gic_length)
        .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?;
    if payload.len() != expected_length {
        return Err(HvfSnapshotV2DecodeError::InvalidLength);
    }
    let compatibility = decode_compatibility(&mut decoder)?;
    debug_assert_eq!(decoder.position, minimum);
    let source = decoder.slice(gic_length)?;
    decoder.finish()?;
    let mut gic_bytes = Vec::new();
    gic_bytes
        .try_reserve_exact(gic_length)
        .map_err(HvfSnapshotV2DecodeError::Allocation)?;
    gic_bytes.extend_from_slice(source);
    HvfSnapshotV2GlobalState::try_new(compatibility, HvfGicDeviceState::new(gic_bytes))
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidGlobal)
}

fn decode_compatibility(
    decoder: &mut Decoder<'_>,
) -> Result<HvfSnapshotV1CompatibilityState, HvfSnapshotV2DecodeError> {
    let mut identification_values = [0; 11];
    for value in &mut identification_values {
        *value = decoder.u64()?;
    }
    let identification = HvfArm64VcpuIdentificationRegisterState::new(identification_values);
    let optional_present = decoder.bool()?;
    decoder.zeroes(7)?;
    let optional_values = [decoder.u64()?, decoder.u64()?];
    let optional = if optional_present {
        Some(HvfArm64VcpuSveSmeIdentificationRegisterState::new(
            optional_values[0],
            optional_values[1],
        ))
    } else {
        if optional_values != [0, 0] {
            return Err(HvfSnapshotV2DecodeError::InvalidGlobal);
        }
        None
    };

    let configuration =
        HvfArm64VcpuCacheConfiguration::new([decoder.u64()?, decoder.u64()?, decoder.u64()?]);
    let mut geometry = [[0; 8]; 2];
    for values in &mut geometry {
        for value in values {
            *value = decoder.u64()?;
        }
    }
    let cache =
        HvfArm64VcpuCacheManifest::new(configuration, HvfArm64VcpuCacheGeometry::new(geometry));
    let primary_mpidr = decoder.u64()?;
    let distributor = decode_gic_region(decoder)?;
    let redistributor_region = decode_gic_region(decoder)?;
    let redistributor_size = decoder.u64()?;
    let spi_interrupt_range = decode_interrupt_range(decoder)?;
    let timer_interrupts = HvfGicTimerInterrupts {
        el1_virtual_timer_intid: decoder.u32()?,
        el1_physical_timer_intid: decoder.u32()?,
    };
    let msi_present = decoder.bool()?;
    decoder.zeroes(7)?;
    let msi_region = decode_gic_region(decoder)?;
    let msi_range = decode_interrupt_range(decoder)?;
    let msi = if msi_present {
        Some(HvfGicMsiMetadata {
            region: msi_region,
            interrupt_range: msi_range,
        })
    } else {
        if msi_region != (HvfGicRegion { base: 0, size: 0 })
            || msi_range != (HvfGicInterruptRange { base: 0, count: 0 })
        {
            return Err(HvfSnapshotV2DecodeError::InvalidGlobal);
        }
        None
    };
    let rtc = RtcMmioLayout::new(
        GuestAddress::new(decoder.u64()?),
        bangbang_runtime::mmio::MmioRegionId::new(decoder.u64()?),
    );
    let state = HvfSnapshotV1CompatibilityState::new(
        identification,
        optional,
        cache,
        primary_mpidr,
        HvfGicMetadata {
            distributor,
            redistributor: HvfGicRedistributor {
                region: redistributor_region,
                single_redistributor_size: redistributor_size,
            },
            spi_interrupt_range,
            timer_interrupts,
            msi,
        },
        rtc,
    );
    validate_compatibility(&state).map_err(|_| HvfSnapshotV2DecodeError::InvalidGlobal)?;
    Ok(state)
}

fn encode_gic_region(encoder: &mut Encoder, region: HvfGicRegion) {
    encoder.u64(region.base);
    encoder.u64(region.size);
}

fn decode_gic_region(decoder: &mut Decoder<'_>) -> Result<HvfGicRegion, HvfSnapshotV2DecodeError> {
    Ok(HvfGicRegion {
        base: decoder.u64()?,
        size: decoder.u64()?,
    })
}

fn encode_interrupt_range(encoder: &mut Encoder, range: HvfGicInterruptRange) {
    encoder.u32(range.base);
    encoder.u32(range.count);
}

fn decode_interrupt_range(
    decoder: &mut Decoder<'_>,
) -> Result<HvfGicInterruptRange, HvfSnapshotV2DecodeError> {
    Ok(HvfGicInterruptRange {
        base: decoder.u32()?,
        count: decoder.u32()?,
    })
}

fn encode_time(state: &HvfSnapshotV2TimeState) -> Result<Vec<u8>, HvfSnapshotV2EncodeError> {
    let entry_bytes = state
        .pvtime_vcpus
        .len()
        .checked_mul(TIME_PVTIME_ENTRY_BYTES)
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let capacity = TIME_HEADER_BYTES
        .checked_add(entry_bytes)
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let mut encoder =
        Encoder::with_capacity(capacity).map_err(HvfSnapshotV2EncodeError::Allocation)?;
    encoder.bytes(&TIME_MAGIC);
    encoder.u16(COMPONENT_PROFILE);
    encoder.u16(
        u16::try_from(TIME_HEADER_BYTES).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder.u32(COMPONENT_FLAGS);
    encoder.u8(TIME_RTC_POLICY_DESTINATION_SYSTEM_TIME);
    encoder.u8(TIME_PVTIME_POLICY_PRESERVE_EXCLUDE_DOWNTIME);
    encoder.u8(TIME_VMGENID_POLICY_REGENERATE_NOTIFY);
    encoder.u8(TIME_VMCLOCK_POLICY_INCREMENT_NOTIFY);
    encoder.zeroes(4);
    encoder.u32(
        u32::try_from(state.pvtime_vcpus.len())
            .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder.u32(
        u32::try_from(TIME_PVTIME_ENTRY_BYTES)
            .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder.u64(state.rtc_layout.base().raw_value());
    encoder.u64(state.rtc_layout.region_id().raw_value());
    encode_platform_metadata(&mut encoder, state.vmgenid);
    encode_platform_metadata(&mut encoder, state.vmclock);
    encoder.bytes(&state.vmclock_abi.to_bytes());
    debug_assert_eq!(encoder.len(), TIME_HEADER_BYTES);
    for vcpu in &state.pvtime_vcpus {
        encoder.u32(vcpu.index);
        encoder.zeroes(4);
        encoder.u64(vcpu.record_ipa.raw_value());
        encoder.u64(vcpu.stolen_time_ns);
    }
    debug_assert_eq!(encoder.len(), capacity);
    Ok(encoder.finish())
}

fn encode_platform_metadata(encoder: &mut Encoder, metadata: SnapshotV1PlatformDeviceMetadata) {
    encoder.u64(metadata.range().start().raw_value());
    encoder.u64(metadata.range().size());
    encoder.u64(metadata.fdt_region().base);
    encoder.u64(metadata.fdt_region().size);
    encoder.u32(metadata.interrupt_line().raw_value());
    encoder.zeroes(4);
}

fn decode_time(payload: &[u8]) -> Result<HvfSnapshotV2TimeState, HvfSnapshotV2DecodeError> {
    if payload.len() < TIME_HEADER_BYTES {
        return Err(HvfSnapshotV2DecodeError::Truncated);
    }
    let mut decoder = Decoder::new(payload);
    if decoder.array::<8>()? != TIME_MAGIC
        || decoder.u16()? != COMPONENT_PROFILE
        || usize::from(decoder.u16()?) != TIME_HEADER_BYTES
        || decoder.u32()? != COMPONENT_FLAGS
    {
        return Err(HvfSnapshotV2DecodeError::InvalidHeader);
    }
    if decoder.u8()? != TIME_RTC_POLICY_DESTINATION_SYSTEM_TIME
        || decoder.u8()? != TIME_PVTIME_POLICY_PRESERVE_EXCLUDE_DOWNTIME
        || decoder.u8()? != TIME_VMGENID_POLICY_REGENERATE_NOTIFY
        || decoder.u8()? != TIME_VMCLOCK_POLICY_INCREMENT_NOTIFY
    {
        return Err(HvfSnapshotV2DecodeError::InvalidTime);
    }
    decoder.zeroes(4)?;
    let vcpu_count =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?;
    if vcpu_count == 0
        || vcpu_count > usize::from(MAX_SUPPORTED_VCPUS)
        || usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?
            != TIME_PVTIME_ENTRY_BYTES
    {
        return Err(HvfSnapshotV2DecodeError::InvalidLength);
    }
    let expected_length = TIME_HEADER_BYTES
        .checked_add(
            vcpu_count
                .checked_mul(TIME_PVTIME_ENTRY_BYTES)
                .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?,
        )
        .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?;
    if payload.len() != expected_length {
        return Err(HvfSnapshotV2DecodeError::InvalidLength);
    }
    let rtc_layout = RtcMmioLayout::new(
        GuestAddress::new(decoder.u64()?),
        bangbang_runtime::mmio::MmioRegionId::new(decoder.u64()?),
    );
    let vmgenid = decode_platform_metadata(&mut decoder)?;
    let vmclock = decode_platform_metadata(&mut decoder)?;
    let vmclock_abi = VmClockAbi::from_bytes(decoder.array::<VMCLOCK_ABI_SIZE>()?)
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidTime)?;
    debug_assert_eq!(decoder.position, TIME_HEADER_BYTES);

    let mut pvtime_vcpus = Vec::new();
    pvtime_vcpus
        .try_reserve_exact(vcpu_count)
        .map_err(HvfSnapshotV2DecodeError::Allocation)?;
    for _ in 0..vcpu_count {
        let index = decoder.u32()?;
        decoder.zeroes(4)?;
        let record_ipa = GuestAddress::new(decoder.u64()?);
        let stolen_time_ns = decoder.u64()?;
        pvtime_vcpus.push(
            HvfSnapshotV2PvTimeVcpuState::try_new(index, record_ipa, stolen_time_ns)
                .map_err(|_| HvfSnapshotV2DecodeError::InvalidTime)?,
        );
    }
    decoder.finish()?;
    HvfSnapshotV2TimeState::try_new(rtc_layout, vmgenid, vmclock, vmclock_abi, pvtime_vcpus)
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidTime)
}

fn decode_platform_metadata(
    decoder: &mut Decoder<'_>,
) -> Result<SnapshotV1PlatformDeviceMetadata, HvfSnapshotV2DecodeError> {
    let range = GuestMemoryRange::new(GuestAddress::new(decoder.u64()?), decoder.u64()?)
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidTime)?;
    let fdt_region = Arm64FdtRegion {
        base: decoder.u64()?,
        size: decoder.u64()?,
    };
    let interrupt_line = GuestInterruptLine::new(decoder.u32()?)
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidTime)?;
    decoder.zeroes(4)?;
    Ok(SnapshotV1PlatformDeviceMetadata::new(
        range,
        fdt_region,
        interrupt_line,
    ))
}

fn encode_topology(
    state: &HvfArm64StablePausedTopologyState,
) -> Result<Vec<u8>, HvfSnapshotV2EncodeError> {
    let members_bytes = state
        .members()
        .len()
        .checked_mul(TOPOLOGY_MEMBER_BYTES)
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let capacity = TOPOLOGY_HEADER_BYTES
        .checked_add(members_bytes)
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let mut encoder =
        Encoder::with_capacity(capacity).map_err(HvfSnapshotV2EncodeError::Allocation)?;
    encoder.bytes(&TOPOLOGY_MAGIC);
    encoder.u16(COMPONENT_PROFILE);
    encoder.u16(
        u16::try_from(TOPOLOGY_HEADER_BYTES)
            .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder.u32(COMPONENT_FLAGS);
    encoder.u32(state.virtual_timer_intid());
    encoder.u32(
        u32::try_from(state.members().len())
            .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder.u64(0);
    for member in state.members() {
        encoder.u32(
            u32::try_from(member.index()).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
        );
        match member.disposition() {
            HvfArm64StableVcpuDisposition::Offline => {
                encoder.u8(0);
                encoder.u8(0);
                encoder.zeroes(2);
                encoder.u64(member.mpidr());
                encoder.zeroes(32);
            }
            HvfArm64StableVcpuDisposition::Runnable => {
                encoder.u8(1);
                encoder.u8(0);
                encoder.zeroes(2);
                encoder.u64(member.mpidr());
                encoder.zeroes(32);
            }
            HvfArm64StableVcpuDisposition::Suspended(suspended) => {
                encoder.u8(2);
                encoder.u8(match suspended.convention() {
                    HvfArm64CpuSuspendConvention::Call32 => 1,
                    HvfArm64CpuSuspendConvention::Call64 => 2,
                });
                encoder.zeroes(2);
                encoder.u64(member.mpidr());
                for argument in suspended.arguments() {
                    encoder.u64(argument);
                }
                encoder.u64(suspended.return_pc());
            }
        }
    }
    debug_assert_eq!(encoder.len(), capacity);
    Ok(encoder.finish())
}

fn decode_topology(
    payload: &[u8],
) -> Result<HvfArm64StablePausedTopologyState, HvfSnapshotV2DecodeError> {
    if payload.len() < TOPOLOGY_HEADER_BYTES {
        return Err(HvfSnapshotV2DecodeError::Truncated);
    }
    let mut decoder = Decoder::new(payload);
    if decoder.array::<8>()? != TOPOLOGY_MAGIC
        || decoder.u16()? != COMPONENT_PROFILE
        || usize::from(decoder.u16()?) != TOPOLOGY_HEADER_BYTES
        || decoder.u32()? != COMPONENT_FLAGS
    {
        return Err(HvfSnapshotV2DecodeError::InvalidHeader);
    }
    let virtual_timer_intid = decoder.u32()?;
    let count =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?;
    decoder.zeroes(8)?;
    if count == 0 || count > usize::from(MAX_SUPPORTED_VCPUS) {
        return Err(HvfSnapshotV2DecodeError::InvalidTopology);
    }
    let expected_length = TOPOLOGY_HEADER_BYTES
        .checked_add(
            count
                .checked_mul(TOPOLOGY_MEMBER_BYTES)
                .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?,
        )
        .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?;
    if payload.len() != expected_length {
        return Err(HvfSnapshotV2DecodeError::InvalidLength);
    }
    let mut members = Vec::new();
    members
        .try_reserve_exact(count)
        .map_err(HvfSnapshotV2DecodeError::Allocation)?;
    for _ in 0..count {
        let index = usize::try_from(decoder.u32()?)
            .map_err(|_| HvfSnapshotV2DecodeError::InvalidTopology)?;
        let disposition = decoder.u8()?;
        let convention = decoder.u8()?;
        decoder.zeroes(2)?;
        let mpidr = decoder.u64()?;
        let arguments = [decoder.u64()?, decoder.u64()?, decoder.u64()?];
        let return_pc = decoder.u64()?;
        let disposition = match disposition {
            0 if convention == 0 && arguments == [0; 3] && return_pc == 0 => {
                HvfArm64StableVcpuDisposition::Offline
            }
            1 if convention == 0 && arguments == [0; 3] && return_pc == 0 => {
                HvfArm64StableVcpuDisposition::Runnable
            }
            2 => {
                let convention = match convention {
                    1 => HvfArm64CpuSuspendConvention::Call32,
                    2 => HvfArm64CpuSuspendConvention::Call64,
                    _ => return Err(HvfSnapshotV2DecodeError::InvalidTopology),
                };
                let suspended =
                    HvfArm64StableCpuSuspendState::new(convention, arguments, return_pc)
                        .map_err(|_| HvfSnapshotV2DecodeError::InvalidTopology)?;
                HvfArm64StableVcpuDisposition::Suspended(suspended)
            }
            _ => return Err(HvfSnapshotV2DecodeError::InvalidTopology),
        };
        members.push(HvfArm64StablePausedTopologyMember::new(
            index,
            mpidr,
            disposition,
        ));
    }
    decoder.finish()?;
    HvfArm64StablePausedTopologyState::new(virtual_timer_intid, members)
        .map_err(|_| HvfSnapshotV2DecodeError::InvalidTopology)
}

fn encode_platform_vcpu(
    state: &HvfSnapshotV2VcpuState,
) -> Result<Vec<u8>, HvfSnapshotV2EncodeError> {
    let mandatory = encode_vcpu(&state.mandatory).map_err(HvfSnapshotV2EncodeError::Mandatory)?;
    let optional = encode_optional(&state.reviewed_optional)?;
    let capacity = VCPU_HEADER_BYTES
        .checked_add(mandatory.len())
        .and_then(|value| value.checked_add(VCPU_INTERRUPT_BYTES))
        .and_then(|value| value.checked_add(optional.len()))
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let mut encoder =
        Encoder::with_capacity(capacity).map_err(HvfSnapshotV2EncodeError::Allocation)?;
    encoder.bytes(&VCPU_MAGIC);
    encoder.u16(COMPONENT_PROFILE);
    encoder.u16(
        u16::try_from(VCPU_HEADER_BYTES).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder.u32(COMPONENT_FLAGS);
    encoder.u32(state.index);
    encoder.u32(0);
    encoder.u64(state.mpidr);
    encoder
        .u32(u32::try_from(mandatory.len()).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?);
    encoder.u32(
        u32::try_from(VCPU_INTERRUPT_BYTES)
            .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder
        .u32(u32::try_from(optional.len()).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?);
    encoder.u32(0);
    debug_assert_eq!(encoder.len(), VCPU_HEADER_BYTES);
    encoder.bytes(&mandatory);
    encode_vcpu_interrupts(&mut encoder, state);
    encoder.bytes(&optional);
    debug_assert_eq!(encoder.len(), capacity);
    Ok(encoder.finish())
}

fn encode_vcpu_interrupts(encoder: &mut Encoder, state: &HvfSnapshotV2VcpuState) {
    encoder.bool(state.timer.virtual_timer_exit_masked());
    encoder.zeroes(7);
    for value in [
        state.timer.cntkctl_el1(),
        state.timer.virtual_count(),
        state.timer.virtual_control(),
        state.timer.virtual_compare_value(),
        state.timer.physical_control(),
        state.timer.physical_compare_delta(),
    ] {
        encoder.u64(value);
    }
    encoder.bool(state.pending_interrupts.irq_pending());
    encoder.bool(state.pending_interrupts.fiq_pending());
    encoder.zeroes(6);
    for value in [
        state.gic_icc.pmr_el1(),
        state.gic_icc.bpr0_el1(),
        state.gic_icc.ap0r0_el1(),
        state.gic_icc.ap1r0_el1(),
        state.gic_icc.rpr_el1(),
        state.gic_icc.bpr1_el1(),
        state.gic_icc.ctlr_el1(),
        state.gic_icc.sre_el1(),
        state.gic_icc.igrpen0_el1(),
        state.gic_icc.igrpen1_el1(),
    ] {
        encoder.u64(value);
    }
}

fn decode_platform_vcpu(
    payload: &[u8],
) -> Result<HvfSnapshotV2VcpuState, HvfSnapshotV2DecodeError> {
    if payload.len() < VCPU_HEADER_BYTES {
        return Err(HvfSnapshotV2DecodeError::Truncated);
    }
    let mut decoder = Decoder::new(payload);
    if decoder.array::<8>()? != VCPU_MAGIC
        || decoder.u16()? != COMPONENT_PROFILE
        || usize::from(decoder.u16()?) != VCPU_HEADER_BYTES
        || decoder.u32()? != COMPONENT_FLAGS
    {
        return Err(HvfSnapshotV2DecodeError::InvalidHeader);
    }
    let index = decoder.u32()?;
    decoder.zeroes(4)?;
    let mpidr = decoder.u64()?;
    let mandatory_length =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?;
    let interrupt_length =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?;
    let optional_length =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidLength)?;
    decoder.zeroes(4)?;
    if mandatory_length == 0
        || interrupt_length != VCPU_INTERRUPT_BYTES
        || optional_length < OPTIONAL_HEADER_BYTES
    {
        return Err(HvfSnapshotV2DecodeError::InvalidLength);
    }
    let expected_length = VCPU_HEADER_BYTES
        .checked_add(mandatory_length)
        .and_then(|value| value.checked_add(interrupt_length))
        .and_then(|value| value.checked_add(optional_length))
        .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?;
    if payload.len() != expected_length {
        return Err(HvfSnapshotV2DecodeError::InvalidLength);
    }
    let mandatory = decode_vcpu(decoder.slice(mandatory_length)?)
        .map_err(HvfSnapshotV2DecodeError::Mandatory)?;
    let (timer, pending_interrupts, gic_icc) =
        decode_vcpu_interrupts(decoder.slice(interrupt_length)?)?;
    let reviewed_optional = decode_optional(decoder.slice(optional_length)?, &mandatory.simd_fp)?;
    decoder.finish()?;
    HvfSnapshotV2VcpuState::try_new(
        index,
        mpidr,
        mandatory,
        timer,
        pending_interrupts,
        gic_icc,
        reviewed_optional,
    )
    .map_err(|_| HvfSnapshotV2DecodeError::InvalidVcpu)
}

fn decode_vcpu_interrupts(
    payload: &[u8],
) -> Result<
    (
        HvfArm64SnapshotTimerState,
        HvfArm64VcpuPendingInterruptState,
        HvfArm64GicIccRegisterState,
    ),
    HvfSnapshotV2DecodeError,
> {
    if payload.len() != VCPU_INTERRUPT_BYTES {
        return Err(HvfSnapshotV2DecodeError::InvalidLength);
    }
    let mut decoder = Decoder::new(payload);
    let virtual_timer_exit_masked = decoder.bool()?;
    decoder.zeroes(7)?;
    let timer = HvfArm64SnapshotTimerState::try_new(
        virtual_timer_exit_masked,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
    )
    .map_err(|_| HvfSnapshotV2DecodeError::InvalidVcpu)?;
    let pending = HvfArm64VcpuPendingInterruptState::new(decoder.bool()?, decoder.bool()?);
    decoder.zeroes(6)?;
    let mut icc = [0; 10];
    for value in &mut icc {
        *value = decoder.u64()?;
    }
    decoder.finish()?;
    Ok((timer, pending, HvfArm64GicIccRegisterState::new(icc)))
}

fn encode_optional(
    state: &HvfArm64ReviewedOptionalStateRestore,
) -> Result<Vec<u8>, HvfSnapshotV2EncodeError> {
    let registry_capacity = optional_registry_capacity(state)?;
    let mut registry =
        Encoder::with_capacity(registry_capacity).map_err(HvfSnapshotV2EncodeError::Allocation)?;
    let mut record_count = 0_usize;
    encode_debug_registry(
        &mut registry,
        state.breakpoints(),
        OPTIONAL_TAG_BREAKPOINT_VALUE,
        OPTIONAL_TAG_BREAKPOINT_CONTROL,
        &mut record_count,
    )?;
    encode_debug_registry(
        &mut registry,
        state.watchpoints(),
        OPTIONAL_TAG_WATCHPOINT_VALUE,
        OPTIONAL_TAG_WATCHPOINT_CONTROL,
        &mut record_count,
    )?;
    if let Some(sme) = state.sme() {
        encode_pstate_record(&mut registry, OPTIONAL_TAG_SME_PSTATE, sme.pstate())?;
        record_count += 1;
        for (index, value) in sme.system_registers().iter().enumerate() {
            let tag = OPTIONAL_TAG_SME_SMCR
                .checked_add(
                    u16::try_from(index).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
                )
                .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
            encode_u64_record(&mut registry, tag, *value)?;
            record_count += 1;
        }
        let target = optional_target_pstate(sme.pstate());
        if target.streaming_sve_mode_enabled() {
            for (index, value) in sme
                .z_registers()
                .ok_or(HvfSnapshotV2EncodeError::Build(
                    HvfSnapshotV2BuildError::Optional,
                ))?
                .iter()
                .enumerate()
            {
                encode_bytes_record(
                    &mut registry,
                    OPTIONAL_TAG_SME_Z
                        + u16::try_from(index)
                            .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
                    sme.maximum_svl_bytes(),
                    value,
                )?;
                record_count += 1;
            }
            let predicate_width = sme.maximum_svl_bytes() / 8;
            for (index, value) in sme
                .p_registers()
                .ok_or(HvfSnapshotV2EncodeError::Build(
                    HvfSnapshotV2BuildError::Optional,
                ))?
                .iter()
                .enumerate()
            {
                encode_bytes_record(
                    &mut registry,
                    OPTIONAL_TAG_SME_P
                        + u16::try_from(index)
                            .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
                    predicate_width,
                    value,
                )?;
                record_count += 1;
            }
        }
        if target.za_storage_enabled() {
            let za_width = sme
                .maximum_svl_bytes()
                .checked_mul(sme.maximum_svl_bytes())
                .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
            encode_bytes_record(
                &mut registry,
                OPTIONAL_TAG_SME_ZA,
                za_width,
                sme.za_register().ok_or(HvfSnapshotV2EncodeError::Build(
                    HvfSnapshotV2BuildError::Optional,
                ))?,
            )?;
            record_count += 1;
            if sme.version() >= OPTIONAL_SME_VERSION_SME2 {
                encode_zt0_record(
                    &mut registry,
                    OPTIONAL_TAG_SME_ZT0,
                    *sme.zt0_register().ok_or(HvfSnapshotV2EncodeError::Build(
                        HvfSnapshotV2BuildError::Optional,
                    ))?,
                )?;
                record_count += 1;
            }
        }
    }
    if registry.len() != registry_capacity
        || registry_capacity > OPTIONAL_MAX_REGISTRY_BYTES
        || record_count > OPTIONAL_MAX_RECORDS
    {
        return Err(HvfSnapshotV2EncodeError::Build(
            HvfSnapshotV2BuildError::Optional,
        ));
    }

    let capacity = OPTIONAL_HEADER_BYTES
        .checked_add(registry.len())
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let mut encoder =
        Encoder::with_capacity(capacity).map_err(HvfSnapshotV2EncodeError::Allocation)?;
    encoder.bytes(&OPTIONAL_MAGIC);
    encoder.u16(COMPONENT_PROFILE);
    encoder.u16(
        u16::try_from(OPTIONAL_HEADER_BYTES)
            .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
    );
    encoder.u32(COMPONENT_FLAGS);
    encoder.u64(state.expected_id_aa64dfr0_el1());
    encoder.u8(state.breakpoints().implemented_count());
    encoder.u8(state.watchpoints().implemented_count());
    if let Some(sme) = state.sme() {
        encoder.bool(true);
        encoder.u8(sme.version());
        encoder.u32(
            u32::try_from(sme.maximum_svl_bytes())
                .map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
        );
        encoder.u32(
            u32::try_from(record_count).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
        );
        encoder.u32(
            u32::try_from(registry.len()).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
        );
        encoder.u64(sme.identification().id_aa64zfr0_el1());
        encoder.u64(sme.identification().id_aa64smfr0_el1());
    } else {
        encoder.bool(false);
        encoder.u8(0);
        encoder.u32(0);
        encoder.u32(
            u32::try_from(record_count).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
        );
        encoder.u32(
            u32::try_from(registry.len()).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
        );
        encoder.zeroes(16);
    }
    encoder.zeroes(8);
    debug_assert_eq!(encoder.len(), OPTIONAL_HEADER_BYTES);
    encoder.bytes(&registry.finish());
    debug_assert_eq!(encoder.len(), capacity);
    Ok(encoder.finish())
}

fn optional_registry_capacity(
    state: &HvfArm64ReviewedOptionalStateRestore,
) -> Result<usize, HvfSnapshotV2EncodeError> {
    let debug_records = usize::from(state.breakpoints().implemented_count())
        .checked_add(usize::from(state.watchpoints().implemented_count()))
        .and_then(|value| value.checked_mul(2))
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
    let mut records = debug_records;
    let mut payload_bytes = 0_usize;
    for debug in [state.breakpoints(), state.watchpoints()] {
        let count = usize::from(debug.implemented_count());
        let values = debug
            .values()
            .get(..count)
            .ok_or(HvfSnapshotV2EncodeError::Build(
                HvfSnapshotV2BuildError::Optional,
            ))?;
        let controls = debug
            .controls()
            .get(..count)
            .ok_or(HvfSnapshotV2EncodeError::Build(
                HvfSnapshotV2BuildError::Optional,
            ))?;
        for value in values.iter().chain(controls) {
            if matches!(value, HvfArm64OptionalStateValue::Explicit(_)) {
                payload_bytes = payload_bytes
                    .checked_add(size_of::<u64>())
                    .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
            }
        }
    }
    if let Some(sme) = state.sme() {
        validate_sme_codec_bounds(sme).map_err(HvfSnapshotV2EncodeError::Build)?;
        records = records
            .checked_add(4)
            .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
        if matches!(sme.pstate(), HvfArm64OptionalStateValue::Explicit(_)) {
            payload_bytes = payload_bytes
                .checked_add(2)
                .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
        }
        for value in sme.system_registers() {
            if matches!(value, HvfArm64OptionalStateValue::Explicit(_)) {
                payload_bytes = payload_bytes
                    .checked_add(size_of::<u64>())
                    .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
            }
        }
        let target = optional_target_pstate(sme.pstate());
        if target.streaming_sve_mode_enabled() {
            let z = sme.z_registers().ok_or(HvfSnapshotV2EncodeError::Build(
                HvfSnapshotV2BuildError::Optional,
            ))?;
            let p = sme.p_registers().ok_or(HvfSnapshotV2EncodeError::Build(
                HvfSnapshotV2BuildError::Optional,
            ))?;
            records = records
                .checked_add(z.len())
                .and_then(|value| value.checked_add(p.len()))
                .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
            for value in z.iter().chain(p) {
                if let HvfArm64OptionalStateValue::Explicit(bytes) = value {
                    payload_bytes = payload_bytes
                        .checked_add(bytes.len())
                        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
                }
            }
        }
        if target.za_storage_enabled() {
            records = records
                .checked_add(1 + usize::from(sme.version() >= OPTIONAL_SME_VERSION_SME2))
                .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
            if let Some(HvfArm64OptionalStateValue::Explicit(bytes)) = sme.za_register() {
                payload_bytes = payload_bytes
                    .checked_add(bytes.len())
                    .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
            }
            if let Some(HvfArm64OptionalStateValue::Explicit(_)) = sme.zt0_register() {
                payload_bytes = payload_bytes
                    .checked_add(64)
                    .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)?;
            }
        }
    }
    if records > OPTIONAL_MAX_RECORDS {
        return Err(HvfSnapshotV2EncodeError::Build(
            HvfSnapshotV2BuildError::Optional,
        ));
    }
    records
        .checked_mul(OPTIONAL_RECORD_HEADER_BYTES)
        .and_then(|value| value.checked_add(payload_bytes))
        .ok_or(HvfSnapshotV2EncodeError::LengthOverflow)
}

fn encode_debug_registry(
    encoder: &mut Encoder,
    state: &HvfArm64DebugRegisterRestoreState,
    value_base: u16,
    control_base: u16,
    record_count: &mut usize,
) -> Result<(), HvfSnapshotV2EncodeError> {
    let count = usize::from(state.implemented_count());
    let values = state
        .values()
        .get(..count)
        .ok_or(HvfSnapshotV2EncodeError::Build(
            HvfSnapshotV2BuildError::Optional,
        ))?;
    let controls = state
        .controls()
        .get(..count)
        .ok_or(HvfSnapshotV2EncodeError::Build(
            HvfSnapshotV2BuildError::Optional,
        ))?;
    for (index, value) in values.iter().enumerate() {
        encode_u64_record(
            encoder,
            value_base
                + u16::try_from(index).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
            *value,
        )?;
        *record_count += 1;
    }
    for (index, value) in controls.iter().enumerate() {
        encode_u64_record(
            encoder,
            control_base
                + u16::try_from(index).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?,
            *value,
        )?;
        *record_count += 1;
    }
    Ok(())
}

fn encode_record_header(
    encoder: &mut Encoder,
    tag: u16,
    width: usize,
    explicit: bool,
) -> Result<(), HvfSnapshotV2EncodeError> {
    encoder.u16(tag);
    encoder.u8(if explicit {
        OPTIONAL_DISPOSITION_EXPLICIT
    } else {
        OPTIONAL_DISPOSITION_DESTINATION_DEFAULT
    });
    encoder.zeroes(1);
    encoder.u32(u32::try_from(width).map_err(|_| HvfSnapshotV2EncodeError::LengthOverflow)?);
    encoder.u32(0);
    Ok(())
}

fn encode_u64_record(
    encoder: &mut Encoder,
    tag: u16,
    value: HvfArm64OptionalStateValue<u64>,
) -> Result<(), HvfSnapshotV2EncodeError> {
    match value {
        HvfArm64OptionalStateValue::Explicit(value) => {
            encode_record_header(encoder, tag, size_of::<u64>(), true)?;
            encoder.u64(value);
        }
        HvfArm64OptionalStateValue::DestinationDefault => {
            encode_record_header(encoder, tag, size_of::<u64>(), false)?;
        }
    }
    Ok(())
}

fn encode_pstate_record(
    encoder: &mut Encoder,
    tag: u16,
    value: HvfArm64OptionalStateValue<HvfArm64VcpuSmePstate>,
) -> Result<(), HvfSnapshotV2EncodeError> {
    match value {
        HvfArm64OptionalStateValue::Explicit(value) => {
            encode_record_header(encoder, tag, 2, true)?;
            encoder.bool(value.streaming_sve_mode_enabled());
            encoder.bool(value.za_storage_enabled());
        }
        HvfArm64OptionalStateValue::DestinationDefault => {
            encode_record_header(encoder, tag, 2, false)?;
        }
    }
    Ok(())
}

fn encode_bytes_record(
    encoder: &mut Encoder,
    tag: u16,
    width: usize,
    value: &HvfArm64OptionalStateValue<Box<[u8]>>,
) -> Result<(), HvfSnapshotV2EncodeError> {
    match value {
        HvfArm64OptionalStateValue::Explicit(bytes) if bytes.len() == width => {
            encode_record_header(encoder, tag, width, true)?;
            encoder.bytes(bytes);
        }
        HvfArm64OptionalStateValue::DestinationDefault => {
            encode_record_header(encoder, tag, width, false)?;
        }
        HvfArm64OptionalStateValue::Explicit(_) => {
            return Err(HvfSnapshotV2EncodeError::Build(
                HvfSnapshotV2BuildError::Optional,
            ));
        }
    }
    Ok(())
}

fn encode_zt0_record(
    encoder: &mut Encoder,
    tag: u16,
    value: HvfArm64OptionalStateValue<[u8; 64]>,
) -> Result<(), HvfSnapshotV2EncodeError> {
    match value {
        HvfArm64OptionalStateValue::Explicit(bytes) => {
            encode_record_header(encoder, tag, 64, true)?;
            encoder.bytes(&bytes);
        }
        HvfArm64OptionalStateValue::DestinationDefault => {
            encode_record_header(encoder, tag, 64, false)?;
        }
    }
    Ok(())
}

fn optional_target_pstate(
    value: HvfArm64OptionalStateValue<HvfArm64VcpuSmePstate>,
) -> HvfArm64VcpuSmePstate {
    match value {
        HvfArm64OptionalStateValue::Explicit(value) => value,
        HvfArm64OptionalStateValue::DestinationDefault => HvfArm64VcpuSmePstate::new(false, false),
    }
}

fn validate_sme_codec_bounds(
    state: &HvfArm64SmeRestoreState,
) -> Result<(), HvfSnapshotV2BuildError> {
    if state.version() > 2
        || state.maximum_svl_bytes() == 0
        || state.maximum_svl_bytes() > HVF_SNAPSHOT_V2_MAX_SME_SVL_BYTES
        || !state.maximum_svl_bytes().is_multiple_of(8)
    {
        Err(HvfSnapshotV2BuildError::Optional)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct OptionalHeader {
    expected_dfr0: u64,
    breakpoint_count: u8,
    watchpoint_count: u8,
    sme_present: bool,
    sme_version: u8,
    maximum_svl_bytes: usize,
    record_count: usize,
    identification: [u64; 2],
}

#[derive(Clone, Copy)]
struct BorrowedOptionalRecord<'a> {
    explicit: Option<&'a [u8]>,
}

fn decode_optional(
    payload: &[u8],
    simd_fp: &HvfArm64VcpuSimdFpState,
) -> Result<HvfArm64ReviewedOptionalStateRestore, HvfSnapshotV2DecodeError> {
    let (header, registry) = decode_optional_header(payload)?;
    scan_optional_registry(header, registry)?;

    let mut decoder = Decoder::new(registry);
    let (breakpoints, mut decoded_records) = decode_debug_registry(
        &mut decoder,
        header.breakpoint_count,
        OPTIONAL_TAG_BREAKPOINT_VALUE,
        OPTIONAL_TAG_BREAKPOINT_CONTROL,
    )?;
    let (watchpoints, watchpoint_records) = decode_debug_registry(
        &mut decoder,
        header.watchpoint_count,
        OPTIONAL_TAG_WATCHPOINT_VALUE,
        OPTIONAL_TAG_WATCHPOINT_CONTROL,
    )?;
    decoded_records += watchpoint_records;

    let sme = if header.sme_present {
        let pstate_record = decode_expected_record(&mut decoder, OPTIONAL_TAG_SME_PSTATE, 2)?;
        decoded_records += 1;
        let pstate = decode_pstate_value(pstate_record)?;
        let mut system_registers = [HvfArm64OptionalStateValue::DestinationDefault; 3];
        for (index, value) in system_registers.iter_mut().enumerate() {
            *value = decode_u64_value(decode_expected_record(
                &mut decoder,
                OPTIONAL_TAG_SME_SMCR
                    + u16::try_from(index)
                        .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
                size_of::<u64>(),
            )?)?;
            decoded_records += 1;
        }
        let target = optional_target_pstate(pstate);
        let mut input = HvfArm64SmeRestoreStateInput::new(
            header.sme_version,
            HvfArm64VcpuSveSmeIdentificationRegisterState::new(
                header.identification[0],
                header.identification[1],
            ),
            header.maximum_svl_bytes,
            pstate,
            system_registers,
        );
        if target.streaming_sve_mode_enabled() {
            let mut z_registers = Vec::new();
            z_registers
                .try_reserve_exact(OPTIONAL_SME_Z_COUNT)
                .map_err(HvfSnapshotV2DecodeError::Allocation)?;
            for index in 0..OPTIONAL_SME_Z_COUNT {
                z_registers.push(decode_bytes_value(decode_expected_record(
                    &mut decoder,
                    OPTIONAL_TAG_SME_Z
                        + u16::try_from(index)
                            .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
                    header.maximum_svl_bytes,
                )?)?);
                decoded_records += 1;
            }
            let predicate_width = header.maximum_svl_bytes / 8;
            let mut p_registers = Vec::new();
            p_registers
                .try_reserve_exact(OPTIONAL_SME_P_COUNT)
                .map_err(HvfSnapshotV2DecodeError::Allocation)?;
            for index in 0..OPTIONAL_SME_P_COUNT {
                p_registers.push(decode_bytes_value(decode_expected_record(
                    &mut decoder,
                    OPTIONAL_TAG_SME_P
                        + u16::try_from(index)
                            .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
                    predicate_width,
                )?)?);
                decoded_records += 1;
            }
            input = input.with_streaming_registers(z_registers, p_registers);
        }
        if target.za_storage_enabled() {
            let za_width = header
                .maximum_svl_bytes
                .checked_mul(header.maximum_svl_bytes)
                .ok_or(HvfSnapshotV2DecodeError::InvalidOptional)?;
            let za = decode_bytes_value(decode_expected_record(
                &mut decoder,
                OPTIONAL_TAG_SME_ZA,
                za_width,
            )?)?;
            decoded_records += 1;
            let zt0 = if header.sme_version >= OPTIONAL_SME_VERSION_SME2 {
                decoded_records += 1;
                Some(decode_zt0_value(decode_expected_record(
                    &mut decoder,
                    OPTIONAL_TAG_SME_ZT0,
                    64,
                )?)?)
            } else {
                None
            };
            input = input.with_za_register(za, zt0);
        }
        Some(
            HvfArm64SmeRestoreState::try_new(input, simd_fp)
                .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
        )
    } else {
        None
    };
    decoder.finish()?;
    if decoded_records != header.record_count {
        return Err(HvfSnapshotV2DecodeError::InvalidOptional);
    }
    HvfArm64ReviewedOptionalStateRestore::try_new(
        header.expected_dfr0,
        header.sme_present.then_some(header.sme_version),
        breakpoints,
        watchpoints,
        sme,
        simd_fp.clone(),
    )
    .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)
}

fn decode_optional_header(
    payload: &[u8],
) -> Result<(OptionalHeader, &[u8]), HvfSnapshotV2DecodeError> {
    if payload.len() < OPTIONAL_HEADER_BYTES {
        return Err(HvfSnapshotV2DecodeError::Truncated);
    }
    let mut decoder = Decoder::new(payload);
    if decoder.array::<8>()? != OPTIONAL_MAGIC
        || decoder.u16()? != COMPONENT_PROFILE
        || usize::from(decoder.u16()?) != OPTIONAL_HEADER_BYTES
        || decoder.u32()? != COMPONENT_FLAGS
    {
        return Err(HvfSnapshotV2DecodeError::InvalidHeader);
    }
    let expected_dfr0 = decoder.u64()?;
    let breakpoint_count = decoder.u8()?;
    let watchpoint_count = decoder.u8()?;
    let sme_present = decoder.bool()?;
    let sme_version = decoder.u8()?;
    let maximum_svl_bytes =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?;
    let record_count =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?;
    let registry_length =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?;
    let identification = [decoder.u64()?, decoder.u64()?];
    decoder.zeroes(8)?;
    if breakpoint_count == 0
        || usize::from(breakpoint_count) > OPTIONAL_DEBUG_CAPACITY
        || watchpoint_count == 0
        || usize::from(watchpoint_count) > OPTIONAL_DEBUG_CAPACITY
        || record_count > OPTIONAL_MAX_RECORDS
        || registry_length > OPTIONAL_MAX_REGISTRY_BYTES
        || payload.len()
            != OPTIONAL_HEADER_BYTES
                .checked_add(registry_length)
                .ok_or(HvfSnapshotV2DecodeError::InvalidLength)?
    {
        return Err(HvfSnapshotV2DecodeError::InvalidOptional);
    }
    if sme_present {
        if sme_version > 2
            || maximum_svl_bytes == 0
            || maximum_svl_bytes > HVF_SNAPSHOT_V2_MAX_SME_SVL_BYTES
            || !maximum_svl_bytes.is_multiple_of(8)
        {
            return Err(HvfSnapshotV2DecodeError::InvalidOptional);
        }
    } else if sme_version != 0 || maximum_svl_bytes != 0 || identification != [0, 0] {
        return Err(HvfSnapshotV2DecodeError::InvalidOptional);
    }
    let registry = decoder.slice(registry_length)?;
    decoder.finish()?;
    Ok((
        OptionalHeader {
            expected_dfr0,
            breakpoint_count,
            watchpoint_count,
            sme_present,
            sme_version,
            maximum_svl_bytes,
            record_count,
            identification,
        },
        registry,
    ))
}

fn scan_optional_registry(
    header: OptionalHeader,
    registry: &[u8],
) -> Result<(), HvfSnapshotV2DecodeError> {
    let mut decoder = Decoder::new(registry);
    let mut records = 0_usize;
    scan_debug_registry(
        &mut decoder,
        header.breakpoint_count,
        OPTIONAL_TAG_BREAKPOINT_VALUE,
        OPTIONAL_TAG_BREAKPOINT_CONTROL,
        &mut records,
    )?;
    scan_debug_registry(
        &mut decoder,
        header.watchpoint_count,
        OPTIONAL_TAG_WATCHPOINT_VALUE,
        OPTIONAL_TAG_WATCHPOINT_CONTROL,
        &mut records,
    )?;
    if header.sme_present {
        let pstate = decode_pstate_value(decode_expected_record(
            &mut decoder,
            OPTIONAL_TAG_SME_PSTATE,
            2,
        )?)?;
        records += 1;
        for index in 0..3 {
            decode_expected_record(
                &mut decoder,
                OPTIONAL_TAG_SME_SMCR
                    + u16::try_from(index)
                        .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
                size_of::<u64>(),
            )?;
            records += 1;
        }
        let target = optional_target_pstate(pstate);
        if target.streaming_sve_mode_enabled() {
            for index in 0..OPTIONAL_SME_Z_COUNT {
                decode_expected_record(
                    &mut decoder,
                    OPTIONAL_TAG_SME_Z
                        + u16::try_from(index)
                            .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
                    header.maximum_svl_bytes,
                )?;
                records += 1;
            }
            for index in 0..OPTIONAL_SME_P_COUNT {
                decode_expected_record(
                    &mut decoder,
                    OPTIONAL_TAG_SME_P
                        + u16::try_from(index)
                            .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
                    header.maximum_svl_bytes / 8,
                )?;
                records += 1;
            }
        }
        if target.za_storage_enabled() {
            decode_expected_record(
                &mut decoder,
                OPTIONAL_TAG_SME_ZA,
                header
                    .maximum_svl_bytes
                    .checked_mul(header.maximum_svl_bytes)
                    .ok_or(HvfSnapshotV2DecodeError::InvalidOptional)?,
            )?;
            records += 1;
            if header.sme_version >= OPTIONAL_SME_VERSION_SME2 {
                decode_expected_record(&mut decoder, OPTIONAL_TAG_SME_ZT0, 64)?;
                records += 1;
            }
        }
    }
    decoder.finish()?;
    if records != header.record_count {
        return Err(HvfSnapshotV2DecodeError::InvalidOptional);
    }
    Ok(())
}

fn scan_debug_registry(
    decoder: &mut Decoder<'_>,
    count: u8,
    value_base: u16,
    control_base: u16,
    records: &mut usize,
) -> Result<(), HvfSnapshotV2DecodeError> {
    for index in 0..u16::from(count) {
        decode_expected_record(decoder, value_base + index, size_of::<u64>())?;
        *records += 1;
    }
    for index in 0..u16::from(count) {
        decode_expected_record(decoder, control_base + index, size_of::<u64>())?;
        *records += 1;
    }
    Ok(())
}

fn decode_debug_registry(
    decoder: &mut Decoder<'_>,
    count: u8,
    value_base: u16,
    control_base: u16,
) -> Result<(HvfArm64DebugRegisterRestoreState, usize), HvfSnapshotV2DecodeError> {
    let mut values = [HvfArm64OptionalStateValue::DestinationDefault; OPTIONAL_DEBUG_CAPACITY];
    let mut controls = [HvfArm64OptionalStateValue::DestinationDefault; OPTIONAL_DEBUG_CAPACITY];
    for (index, value) in values.iter_mut().take(usize::from(count)).enumerate() {
        *value = decode_u64_value(decode_expected_record(
            decoder,
            value_base
                + u16::try_from(index).map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
            size_of::<u64>(),
        )?)?;
    }
    for (index, value) in controls.iter_mut().take(usize::from(count)).enumerate() {
        *value = decode_u64_value(decode_expected_record(
            decoder,
            control_base
                + u16::try_from(index).map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
            size_of::<u64>(),
        )?)?;
    }
    Ok((
        HvfArm64DebugRegisterRestoreState::try_new(count, values, controls)
            .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
        usize::from(count) * 2,
    ))
}

fn decode_expected_record<'a>(
    decoder: &mut Decoder<'a>,
    expected_tag: u16,
    expected_width: usize,
) -> Result<BorrowedOptionalRecord<'a>, HvfSnapshotV2DecodeError> {
    let tag = decoder.u16()?;
    let disposition = decoder.u8()?;
    decoder.zeroes(1)?;
    let width =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?;
    decoder.zeroes(4)?;
    if tag != expected_tag || width != expected_width {
        return Err(HvfSnapshotV2DecodeError::InvalidOptional);
    }
    let explicit = match disposition {
        OPTIONAL_DISPOSITION_EXPLICIT => Some(decoder.slice(width)?),
        OPTIONAL_DISPOSITION_DESTINATION_DEFAULT => None,
        _ => return Err(HvfSnapshotV2DecodeError::InvalidOptional),
    };
    Ok(BorrowedOptionalRecord { explicit })
}

fn decode_u64_value(
    record: BorrowedOptionalRecord<'_>,
) -> Result<HvfArm64OptionalStateValue<u64>, HvfSnapshotV2DecodeError> {
    record.explicit.map_or(
        Ok(HvfArm64OptionalStateValue::DestinationDefault),
        |bytes| {
            let bytes: [u8; 8] = bytes
                .try_into()
                .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?;
            Ok(HvfArm64OptionalStateValue::Explicit(u64::from_le_bytes(
                bytes,
            )))
        },
    )
}

fn decode_pstate_value(
    record: BorrowedOptionalRecord<'_>,
) -> Result<HvfArm64OptionalStateValue<HvfArm64VcpuSmePstate>, HvfSnapshotV2DecodeError> {
    record.explicit.map_or(
        Ok(HvfArm64OptionalStateValue::DestinationDefault),
        |bytes| match bytes {
            [streaming @ 0..=1, za @ 0..=1] => Ok(HvfArm64OptionalStateValue::Explicit(
                HvfArm64VcpuSmePstate::new(*streaming != 0, *za != 0),
            )),
            _ => Err(HvfSnapshotV2DecodeError::InvalidOptional),
        },
    )
}

fn decode_bytes_value(
    record: BorrowedOptionalRecord<'_>,
) -> Result<HvfArm64OptionalStateValue<Box<[u8]>>, HvfSnapshotV2DecodeError> {
    record.explicit.map_or(
        Ok(HvfArm64OptionalStateValue::DestinationDefault),
        |bytes| {
            Ok(HvfArm64OptionalStateValue::Explicit(
                copy_boxed(bytes).map_err(HvfSnapshotV2DecodeError::Allocation)?,
            ))
        },
    )
}

fn decode_zt0_value(
    record: BorrowedOptionalRecord<'_>,
) -> Result<HvfArm64OptionalStateValue<[u8; 64]>, HvfSnapshotV2DecodeError> {
    record.explicit.map_or(
        Ok(HvfArm64OptionalStateValue::DestinationDefault),
        |bytes| {
            Ok(HvfArm64OptionalStateValue::Explicit(
                bytes
                    .try_into()
                    .map_err(|_| HvfSnapshotV2DecodeError::InvalidOptional)?,
            ))
        },
    )
}

#[cfg(test)]
mod tests;
