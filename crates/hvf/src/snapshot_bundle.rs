//! Complete native-v1 Hypervisor.framework snapshot-state bundle.

use std::fmt;

use bangbang_runtime::machine::{MachineConfig, MachineConfigInput};
use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange, aarch64};
use bangbang_runtime::mmio::MmioRegionId;
use bangbang_runtime::rtc::{RTC_MMIO_DEVICE_WINDOW_SIZE, RtcMmioLayout};
use bangbang_runtime::snapshot_commit::{
    NATIVE_V1_SNAPSHOT_COMPOSITE_STATE_MAX_BYTES, SnapshotCommitError, SnapshotCommitKind,
    SnapshotCommitRecord,
};
use bangbang_runtime::snapshot_device::{
    SnapshotV1DeviceDecodeError, SnapshotV1DeviceEncodeError, SnapshotV1DeviceState,
    decode_snapshot_v1_device_state, encode_snapshot_v1_device_state,
};
use bangbang_runtime::snapshot_format::NATIVE_V1_SNAPSHOT_VERSION;
use bangbang_runtime::snapshot_memory::SnapshotMemoryBinding;

use crate::gic::{
    HvfArm64GicIccRegisterState, HvfGicDeviceState, HvfGicInterruptRange, HvfGicMetadata,
    HvfGicMsiMetadata, HvfGicRedistributor, HvfGicRegion, HvfGicTimerInterrupts,
};
use crate::snapshot::{
    HvfArm64SnapshotTimerPolicyError, HvfArm64SnapshotTimerState,
    validate_native_v1_arm64_snapshot_optional_state,
};
use crate::vcpu::{
    HvfArm64VcpuCacheSelectionRegisterState, HvfArm64VcpuCoreSystemRegisterState,
    HvfArm64VcpuDebugControlRegisterState, HvfArm64VcpuDebugTrapState,
    HvfArm64VcpuExceptionRegisterState, HvfArm64VcpuExecutionControlRegisterState,
    HvfArm64VcpuGeneralRegisterState, HvfArm64VcpuIdentificationRegisterState,
    HvfArm64VcpuPendingInterruptState, HvfArm64VcpuPointerAuthenticationKeyState,
    HvfArm64VcpuSimdFpState, HvfArm64VcpuSveSmeIdentificationRegisterState,
    HvfArm64VcpuSystemContextRegisterState, HvfArm64VcpuThreadContextRegisterState,
    HvfArm64VcpuTranslationRegisterState,
};
use crate::vcpu_config::{
    HvfArm64VcpuCacheConfiguration, HvfArm64VcpuCacheGeometry, HvfArm64VcpuCacheManifest,
};

const SNAPSHOT_MAGIC: [u8; 8] = *b"BANGHVF\0";
const SNAPSHOT_HEADER_BYTES: usize = 32;
const COMPONENT_HEADER_BYTES: usize = 8;
const SNAPSHOT_PROFILE: u16 = 1;
const SNAPSHOT_FLAGS: u32 = 0;
const SNAPSHOT_COMPONENT_COUNT: u16 = 5;
const SNAPSHOT_RESERVED_U16: u16 = 0;
const SNAPSHOT_RESERVED_U32: u32 = 0;
const SNAPSHOT_INACTIVE_OPTIONAL_STATE_POLICY: u16 = 1;
const SNAPSHOT_FRESH_SYSTEM_RTC_POLICY: u16 = 1;
const SNAPSHOT_RTC_MMIO_BASE: u64 = 0x4000_1000;
const SNAPSHOT_RTC_MMIO_REGION_ID: u64 = 10;
const SNAPSHOT_VIRTUAL_TIMER_INTID: u32 = 27;
const SNAPSHOT_PHYSICAL_TIMER_INTID: u32 = 30;
const SNAPSHOT_SPI_INTERRUPT_BASE: u32 = 32;
const MACHINE_COMPONENT: u16 = 1;
const COMPATIBILITY_COMPONENT: u16 = 2;
const VCPU_COMPONENT: u16 = 3;
const INTERRUPT_COMPONENT: u16 = 4;
const DEVICE_COMPONENT: u16 = 5;
const COMPONENT_FLAGS: u16 = 0;
const INTERRUPT_COMPONENT_FIXED_BYTES: usize = 144;
const MIB: u64 = 1024 * 1024;
const REDACTED: &str = "<redacted>";

/// Maximum opaque GIC bytes admitted before native-v1 bundle allocation.
///
/// Sixty-four KiB is retained for the fixed HVF components, component headers,
/// and the separately bounded native-v1 device payload. Final encoding still
/// enforces the exact composite commit budget.
pub const HVF_SNAPSHOT_V1_GIC_DEVICE_STATE_MAX_BYTES: usize =
    NATIVE_V1_SNAPSHOT_COMPOSITE_STATE_MAX_BYTES - 64 * 1024;

/// Source-host compatibility metadata required by the native-HVF profile.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV1CompatibilityState {
    identification: HvfArm64VcpuIdentificationRegisterState,
    optional_sve_sme_identification: Option<HvfArm64VcpuSveSmeIdentificationRegisterState>,
    cache_manifest: HvfArm64VcpuCacheManifest,
    primary_mpidr: u64,
    gic_metadata: HvfGicMetadata,
    rtc_mmio_layout: RtcMmioLayout,
}

impl HvfSnapshotV1CompatibilityState {
    pub fn new(
        identification: HvfArm64VcpuIdentificationRegisterState,
        optional_sve_sme_identification: Option<HvfArm64VcpuSveSmeIdentificationRegisterState>,
        cache_manifest: HvfArm64VcpuCacheManifest,
        primary_mpidr: u64,
        gic_metadata: HvfGicMetadata,
        rtc_mmio_layout: RtcMmioLayout,
    ) -> Self {
        Self {
            identification,
            optional_sve_sme_identification,
            cache_manifest,
            primary_mpidr,
            gic_metadata,
            rtc_mmio_layout,
        }
    }

    pub const fn identification(&self) -> HvfArm64VcpuIdentificationRegisterState {
        self.identification
    }

    pub const fn optional_sve_sme_identification(
        &self,
    ) -> Option<HvfArm64VcpuSveSmeIdentificationRegisterState> {
        self.optional_sve_sme_identification
    }

    pub const fn cache_manifest(&self) -> HvfArm64VcpuCacheManifest {
        self.cache_manifest
    }

    pub const fn primary_mpidr(&self) -> u64 {
        self.primary_mpidr
    }

    pub const fn gic_metadata(&self) -> HvfGicMetadata {
        self.gic_metadata
    }

    pub const fn rtc_mmio_layout(&self) -> RtcMmioLayout {
        self.rtc_mmio_layout
    }
}

impl fmt::Debug for HvfSnapshotV1CompatibilityState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV1CompatibilityState")
            .field("profile", &"native-v1")
            .field("compatibility", &REDACTED)
            .finish()
    }
}

/// Complete mutable single-vCPU state supported by the native-HVF profile.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV1VcpuState {
    pub general: HvfArm64VcpuGeneralRegisterState,
    pub core: HvfArm64VcpuCoreSystemRegisterState,
    pub exception: HvfArm64VcpuExceptionRegisterState,
    pub execution: HvfArm64VcpuExecutionControlRegisterState,
    pub cache_selection: HvfArm64VcpuCacheSelectionRegisterState,
    pub debug_control: HvfArm64VcpuDebugControlRegisterState,
    pub debug_trap: HvfArm64VcpuDebugTrapState,
    pub system_context: HvfArm64VcpuSystemContextRegisterState,
    pub translation: HvfArm64VcpuTranslationRegisterState,
    pub pointer_authentication: HvfArm64VcpuPointerAuthenticationKeyState,
    pub thread_context: HvfArm64VcpuThreadContextRegisterState,
    pub simd_fp: HvfArm64VcpuSimdFpState,
}

impl fmt::Debug for HvfSnapshotV1VcpuState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV1VcpuState")
            .field("registers", &REDACTED)
            .finish()
    }
}

/// Complete interrupt-controller and normalized timer state for one vCPU.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV1InterruptState {
    pub timer: HvfArm64SnapshotTimerState,
    pub pending_interrupts: HvfArm64VcpuPendingInterruptState,
    pub gic_device: HvfGicDeviceState,
    pub gic_icc: HvfArm64GicIccRegisterState,
}

impl fmt::Debug for HvfSnapshotV1InterruptState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV1InterruptState")
            .field("state", &REDACTED)
            .field("gic_device_bytes", &self.gic_device.len())
            .finish()
    }
}

/// Typed complete state stored behind a native-v1 composite commit record.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV1State {
    machine: MachineConfig,
    compatibility: HvfSnapshotV1CompatibilityState,
    vcpu: HvfSnapshotV1VcpuState,
    interrupts: HvfSnapshotV1InterruptState,
    device: SnapshotV1DeviceState,
}

impl HvfSnapshotV1State {
    pub fn new(
        machine: MachineConfig,
        compatibility: HvfSnapshotV1CompatibilityState,
        vcpu: HvfSnapshotV1VcpuState,
        interrupts: HvfSnapshotV1InterruptState,
        device: SnapshotV1DeviceState,
    ) -> Self {
        Self {
            machine,
            compatibility,
            vcpu,
            interrupts,
            device,
        }
    }

    pub const fn machine(&self) -> MachineConfig {
        self.machine
    }

    pub const fn compatibility(&self) -> &HvfSnapshotV1CompatibilityState {
        &self.compatibility
    }

    pub const fn vcpu(&self) -> &HvfSnapshotV1VcpuState {
        &self.vcpu
    }

    pub const fn interrupts(&self) -> &HvfSnapshotV1InterruptState {
        &self.interrupts
    }

    pub const fn device(&self) -> &SnapshotV1DeviceState {
        &self.device
    }

    pub fn into_parts(
        self,
    ) -> (
        MachineConfig,
        HvfSnapshotV1CompatibilityState,
        HvfSnapshotV1VcpuState,
        HvfSnapshotV1InterruptState,
        SnapshotV1DeviceState,
    ) {
        (
            self.machine,
            self.compatibility,
            self.vcpu,
            self.interrupts,
            self.device,
        )
    }
}

impl fmt::Debug for HvfSnapshotV1State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV1State")
            .field("profile", &"native-v1")
            .field("state", &REDACTED)
            .finish()
    }
}

/// One validated state/memory pair ready for commit-record publication.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfSnapshotV1Bundle {
    commit_record: SnapshotCommitRecord,
    state: HvfSnapshotV1State,
}

impl HvfSnapshotV1Bundle {
    pub fn try_new(
        memory_binding: SnapshotMemoryBinding,
        state: HvfSnapshotV1State,
    ) -> Result<Self, HvfSnapshotV1BundleError> {
        validate_state_binding(&memory_binding, &state)?;
        let encoded = encode_hvf_snapshot_v1_state(&state)?;
        let commit_record = SnapshotCommitRecord::try_new_composite(memory_binding, encoded)?;
        Ok(Self {
            commit_record,
            state,
        })
    }

    pub fn try_from_commit_record(
        commit_record: SnapshotCommitRecord,
    ) -> Result<Self, HvfSnapshotV1BundleError> {
        if commit_record.kind() != SnapshotCommitKind::Composite {
            return Err(HvfSnapshotV1BundleError::MemoryOnlyCommit);
        }
        let encoded = commit_record
            .composite_state()
            .ok_or(HvfSnapshotV1BundleError::MemoryOnlyCommit)?;
        let state = decode_hvf_snapshot_v1_state(encoded)?;
        validate_state_binding(commit_record.memory_binding(), &state)?;
        Ok(Self {
            commit_record,
            state,
        })
    }

    pub const fn commit_record(&self) -> &SnapshotCommitRecord {
        &self.commit_record
    }

    pub const fn state(&self) -> &HvfSnapshotV1State {
        &self.state
    }

    pub fn into_commit_record(self) -> SnapshotCommitRecord {
        self.commit_record
    }

    pub fn into_state(self) -> HvfSnapshotV1State {
        self.state
    }
}

impl fmt::Debug for HvfSnapshotV1Bundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfSnapshotV1Bundle")
            .field("profile", &"native-v1")
            .field("state", &REDACTED)
            .finish()
    }
}

#[derive(Debug)]
pub enum HvfSnapshotV1BundleError {
    MemoryOnlyCommit,
    BindingMismatch(&'static str),
    Encode(HvfSnapshotV1EncodeError),
    Decode(HvfSnapshotV1DecodeError),
    Commit(SnapshotCommitError),
}

impl fmt::Display for HvfSnapshotV1BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MemoryOnlyCommit => {
                f.write_str("native-HVF snapshot requires a composite commit record")
            }
            Self::BindingMismatch(message) => {
                write!(
                    f,
                    "native-HVF snapshot state does not match memory binding: {message}"
                )
            }
            Self::Encode(source) => write!(f, "failed to encode native-HVF state: {source}"),
            Self::Decode(source) => write!(f, "failed to decode native-HVF state: {source}"),
            Self::Commit(source) => write!(f, "invalid native-HVF commit record: {source}"),
        }
    }
}

impl std::error::Error for HvfSnapshotV1BundleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Encode(source) => Some(source),
            Self::Decode(source) => Some(source),
            Self::Commit(source) => Some(source),
            Self::MemoryOnlyCommit | Self::BindingMismatch(_) => None,
        }
    }
}

impl From<HvfSnapshotV1EncodeError> for HvfSnapshotV1BundleError {
    fn from(source: HvfSnapshotV1EncodeError) -> Self {
        Self::Encode(source)
    }
}

impl From<HvfSnapshotV1DecodeError> for HvfSnapshotV1BundleError {
    fn from(source: HvfSnapshotV1DecodeError) -> Self {
        Self::Decode(source)
    }
}

impl From<SnapshotCommitError> for HvfSnapshotV1BundleError {
    fn from(source: SnapshotCommitError) -> Self {
        Self::Commit(source)
    }
}

#[derive(Debug)]
pub enum HvfSnapshotV1EncodeError {
    Allocation,
    InvalidMachine,
    InvalidCompatibility(&'static str),
    EmptyGicState,
    ComponentTooLarge,
    StateTooLarge,
    Device(SnapshotV1DeviceEncodeError),
}

impl fmt::Display for HvfSnapshotV1EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocation => f.write_str("failed to allocate native-HVF state"),
            Self::InvalidMachine => f.write_str("machine is outside the native-HVF profile"),
            Self::InvalidCompatibility(message) => {
                write!(f, "native-HVF compatibility state is invalid: {message}")
            }
            Self::EmptyGicState => f.write_str("native-HVF GIC device state is empty"),
            Self::ComponentTooLarge => f.write_str("native-HVF component exceeds u32 length"),
            Self::StateTooLarge => f.write_str("native-HVF state exceeds the commit budget"),
            Self::Device(source) => write!(f, "invalid native-v1 device state: {source}"),
        }
    }
}

impl std::error::Error for HvfSnapshotV1EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Device(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum HvfSnapshotV1DecodeError {
    TooSmall,
    TooLarge,
    InvalidMagic,
    UnsupportedVersion,
    UnsupportedProfile,
    UnsupportedFlags,
    NonzeroReserved,
    LengthMismatch,
    Truncated,
    TrailingData,
    InvalidComponentOrder,
    UnsupportedComponent(u16),
    InvalidBoolean,
    InvalidMachine,
    InvalidCompatibility(&'static str),
    InvalidTimer(HvfArm64SnapshotTimerPolicyError),
    InvalidGicState,
    Allocation,
    Device(SnapshotV1DeviceDecodeError),
}

impl fmt::Display for HvfSnapshotV1DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall => f.write_str("native-HVF state is smaller than its header"),
            Self::TooLarge => f.write_str("native-HVF state exceeds the commit budget"),
            Self::InvalidMagic => f.write_str("native-HVF state magic is invalid"),
            Self::UnsupportedVersion => f.write_str("native-HVF state version is unsupported"),
            Self::UnsupportedProfile => f.write_str("native-HVF state profile is unsupported"),
            Self::UnsupportedFlags => f.write_str("native-HVF state flags are unsupported"),
            Self::NonzeroReserved => f.write_str("native-HVF reserved field is nonzero"),
            Self::LengthMismatch => f.write_str("native-HVF declared length does not match input"),
            Self::Truncated => f.write_str("native-HVF state is truncated"),
            Self::TrailingData => f.write_str("native-HVF component has trailing data"),
            Self::InvalidComponentOrder => {
                f.write_str("native-HVF required components are missing, duplicated, or reordered")
            }
            Self::UnsupportedComponent(kind) => {
                write!(f, "native-HVF component kind {kind} is unsupported")
            }
            Self::InvalidBoolean => f.write_str("native-HVF boolean tag is invalid"),
            Self::InvalidMachine => f.write_str("native-HVF machine component is invalid"),
            Self::InvalidCompatibility(message) => {
                write!(
                    f,
                    "native-HVF compatibility component is invalid: {message}"
                )
            }
            Self::InvalidTimer(source) => write!(f, "native-HVF timer state is invalid: {source}"),
            Self::InvalidGicState => f.write_str("native-HVF GIC device state is invalid"),
            Self::Allocation => f.write_str("failed to allocate decoded native-HVF state"),
            Self::Device(source) => write!(f, "invalid native-v1 device state: {source}"),
        }
    }
}

impl std::error::Error for HvfSnapshotV1DecodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidTimer(source) => Some(source),
            Self::Device(source) => Some(source),
            _ => None,
        }
    }
}

/// Encode complete typed native-HVF state with deterministic component order.
pub fn encode_hvf_snapshot_v1_state(
    state: &HvfSnapshotV1State,
) -> Result<Vec<u8>, HvfSnapshotV1EncodeError> {
    validate_machine(state.machine)?;
    validate_compatibility(&state.compatibility)
        .map_err(HvfSnapshotV1EncodeError::InvalidCompatibility)?;
    validate_native_v1_arm64_snapshot_optional_state(state.vcpu.execution, None, None, None)
        .map_err(|_| HvfSnapshotV1EncodeError::InvalidCompatibility("optional state is active"))?;

    let components = [
        (MACHINE_COMPONENT, encode_machine(state.machine)?),
        (
            COMPATIBILITY_COMPONENT,
            encode_compatibility(&state.compatibility)?,
        ),
        (VCPU_COMPONENT, encode_vcpu(&state.vcpu)?),
        (INTERRUPT_COMPONENT, encode_interrupts(&state.interrupts)?),
        (
            DEVICE_COMPONENT,
            encode_snapshot_v1_device_state(&state.device)
                .map_err(HvfSnapshotV1EncodeError::Device)?,
        ),
    ];

    let mut total_length = SNAPSHOT_HEADER_BYTES;
    for (_, payload) in &components {
        u32::try_from(payload.len()).map_err(|_| HvfSnapshotV1EncodeError::ComponentTooLarge)?;
        total_length = total_length
            .checked_add(COMPONENT_HEADER_BYTES)
            .and_then(|length| length.checked_add(payload.len()))
            .ok_or(HvfSnapshotV1EncodeError::StateTooLarge)?;
    }
    if total_length > NATIVE_V1_SNAPSHOT_COMPOSITE_STATE_MAX_BYTES
        || u32::try_from(total_length).is_err()
    {
        return Err(HvfSnapshotV1EncodeError::StateTooLarge);
    }

    let mut encoder = Encoder::with_capacity(total_length)?;
    encoder.bytes(&SNAPSHOT_MAGIC);
    encoder.u16(NATIVE_V1_SNAPSHOT_VERSION.major());
    encoder.u16(NATIVE_V1_SNAPSHOT_VERSION.minor());
    encoder.u16(NATIVE_V1_SNAPSHOT_VERSION.patch());
    encoder.u16(SNAPSHOT_PROFILE);
    encoder.u32(SNAPSHOT_FLAGS);
    encoder.u16(SNAPSHOT_COMPONENT_COUNT);
    encoder.u16(SNAPSHOT_RESERVED_U16);
    encoder.u32(total_length as u32);
    encoder.u32(SNAPSHOT_RESERVED_U32);
    for (kind, payload) in components {
        encoder.u16(kind);
        encoder.u16(COMPONENT_FLAGS);
        encoder.u32(payload.len() as u32);
        encoder.bytes(&payload);
    }
    debug_assert_eq!(encoder.len(), total_length);
    Ok(encoder.finish())
}

fn encode_machine(machine: MachineConfig) -> Result<Vec<u8>, HvfSnapshotV1EncodeError> {
    let mut encoder = Encoder::with_capacity(16)?;
    encoder.u8(machine.vcpu_count());
    encoder.bool(machine.smt());
    encoder.bool(machine.track_dirty_pages());
    encoder.u8(match machine.huge_pages() {
        bangbang_runtime::machine::MachineConfigHugePages::None => 0,
        _ => return Err(HvfSnapshotV1EncodeError::InvalidMachine),
    });
    encoder.bool(machine.cpu_template().is_some());
    encoder.zeros(3);
    encoder.u64(machine.mem_size_mib());
    Ok(encoder.finish())
}

fn encode_compatibility(
    state: &HvfSnapshotV1CompatibilityState,
) -> Result<Vec<u8>, HvfSnapshotV1EncodeError> {
    let mut encoder = Encoder::with_capacity(512)?;
    let identification = state.identification;
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
    if let Some(optional) = state.optional_sve_sme_identification {
        encoder.u8(1);
        encoder.zeros(7);
        encoder.u64(optional.id_aa64zfr0_el1());
        encoder.u64(optional.id_aa64smfr0_el1());
    } else {
        encoder.u8(0);
        encoder.zeros(7);
        encoder.u64(0);
        encoder.u64(0);
    }
    encoder.u16(SNAPSHOT_INACTIVE_OPTIONAL_STATE_POLICY);
    encoder.zeros(6);

    let cache = state.cache_manifest;
    let configuration = cache.configuration();
    for value in [
        configuration.ctr_el0(),
        configuration.clidr_el1(),
        configuration.dczid_el0(),
    ] {
        encoder.u64(value);
    }
    let geometry = cache.geometry();
    for value in geometry.data_or_unified_ccsidr_el1() {
        encoder.u64(*value);
    }
    for value in geometry.instruction_ccsidr_el1() {
        encoder.u64(*value);
    }
    encoder.u64(state.primary_mpidr);

    let gic = state.gic_metadata;
    encode_gic_region(&mut encoder, gic.distributor);
    encode_gic_region(&mut encoder, gic.redistributor.region);
    encoder.u64(gic.redistributor.single_redistributor_size);
    encode_interrupt_range(&mut encoder, gic.spi_interrupt_range);
    encoder.u32(gic.timer_interrupts.el1_virtual_timer_intid);
    encoder.u32(gic.timer_interrupts.el1_physical_timer_intid);
    if let Some(msi) = gic.msi {
        encoder.u8(1);
        encoder.zeros(7);
        encode_gic_region(&mut encoder, msi.region);
        encode_interrupt_range(&mut encoder, msi.interrupt_range);
    } else {
        encoder.u8(0);
        encoder.zeros(7);
        encode_gic_region(&mut encoder, HvfGicRegion { base: 0, size: 0 });
        encode_interrupt_range(&mut encoder, HvfGicInterruptRange { base: 0, count: 0 });
    }

    encoder.u64(state.rtc_mmio_layout.base().raw_value());
    encoder.u64(state.rtc_mmio_layout.region_id().raw_value());
    encoder.u16(SNAPSHOT_FRESH_SYSTEM_RTC_POLICY);
    encoder.zeros(6);
    Ok(encoder.finish())
}

fn encode_vcpu(state: &HvfSnapshotV1VcpuState) -> Result<Vec<u8>, HvfSnapshotV1EncodeError> {
    let mut encoder = Encoder::with_capacity(2048)?;
    for value in state.general.general_purpose_registers() {
        encoder.u64(*value);
    }
    encoder.u64(state.general.pc());
    encoder.u64(state.general.cpsr());
    for value in [
        state.core.sp_el0(),
        state.core.sp_el1(),
        state.core.elr_el1(),
        state.core.spsr_el1(),
        state.exception.afsr0_el1(),
        state.exception.afsr1_el1(),
        state.exception.esr_el1(),
        state.exception.far_el1(),
        state.exception.par_el1(),
        state.exception.vbar_el1(),
        state.execution.actlr_el1(),
        state.execution.cpacr_el1(),
        state.cache_selection.csselr_el1(),
        state.debug_control.mdccint_el1(),
        state.debug_control.mdscr_el1(),
    ] {
        encoder.u64(value);
    }
    encoder.bool(state.debug_trap.trap_debug_exceptions());
    encoder.bool(state.debug_trap.trap_debug_reg_accesses());
    encoder.zeros(6);
    for value in [
        state.system_context.scxtnum_el0(),
        state.system_context.scxtnum_el1(),
        state.translation.sctlr_el1(),
        state.translation.ttbr0_el1(),
        state.translation.ttbr1_el1(),
        state.translation.tcr_el1(),
        state.translation.mair_el1(),
        state.translation.amair_el1(),
        state.translation.contextidr_el1(),
    ] {
        encoder.u64(value);
    }
    for key in [
        state.pointer_authentication.apia_key(),
        state.pointer_authentication.apib_key(),
        state.pointer_authentication.apda_key(),
        state.pointer_authentication.apdb_key(),
        state.pointer_authentication.apga_key(),
    ] {
        encoder.u64(key as u64);
        encoder.u64((key >> 64) as u64);
    }
    for value in [
        state.thread_context.tpidr_el0(),
        state.thread_context.tpidrro_el0(),
        state.thread_context.tpidr_el1(),
    ] {
        encoder.u64(value);
    }
    for value in state.simd_fp.q_registers() {
        encoder.bytes(value);
    }
    encoder.u64(state.simd_fp.fpcr());
    encoder.u64(state.simd_fp.fpsr());
    Ok(encoder.finish())
}

fn encode_interrupts(
    state: &HvfSnapshotV1InterruptState,
) -> Result<Vec<u8>, HvfSnapshotV1EncodeError> {
    if state.gic_device.is_empty() {
        return Err(HvfSnapshotV1EncodeError::EmptyGicState);
    }
    if state.gic_device.len() > HVF_SNAPSHOT_V1_GIC_DEVICE_STATE_MAX_BYTES {
        return Err(HvfSnapshotV1EncodeError::StateTooLarge);
    }
    let capacity = INTERRUPT_COMPONENT_FIXED_BYTES
        .checked_add(state.gic_device.len())
        .ok_or(HvfSnapshotV1EncodeError::StateTooLarge)?;
    let mut encoder = Encoder::with_capacity(capacity)?;
    encoder.bool(state.timer.virtual_timer_exit_masked());
    encoder.zeros(7);
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
    encoder.zeros(2);
    let gic_length = u32::try_from(state.gic_device.len())
        .map_err(|_| HvfSnapshotV1EncodeError::ComponentTooLarge)?;
    encoder.u32(gic_length);
    encoder.bytes(state.gic_device.as_bytes());
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
    Ok(encoder.finish())
}

fn encode_gic_region(encoder: &mut Encoder, region: HvfGicRegion) {
    encoder.u64(region.base);
    encoder.u64(region.size);
}

fn encode_interrupt_range(encoder: &mut Encoder, range: HvfGicInterruptRange) {
    encoder.u32(range.base);
    encoder.u32(range.count);
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn with_capacity(capacity: usize) -> Result<Self, HvfSnapshotV1EncodeError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| HvfSnapshotV1EncodeError::Allocation)?;
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

    fn bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    fn zeros(&mut self, count: usize) {
        self.bytes.resize(self.bytes.len() + count, 0);
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

/// Decode complete typed native-HVF state after strict structural validation.
pub fn decode_hvf_snapshot_v1_state(
    encoded: &[u8],
) -> Result<HvfSnapshotV1State, HvfSnapshotV1DecodeError> {
    if encoded.len() < SNAPSHOT_HEADER_BYTES {
        return Err(HvfSnapshotV1DecodeError::TooSmall);
    }
    if encoded.len() > NATIVE_V1_SNAPSHOT_COMPOSITE_STATE_MAX_BYTES {
        return Err(HvfSnapshotV1DecodeError::TooLarge);
    }

    let mut decoder = Decoder::new(encoded);
    if decoder.array::<8>()? != SNAPSHOT_MAGIC {
        return Err(HvfSnapshotV1DecodeError::InvalidMagic);
    }
    let version = (decoder.u16()?, decoder.u16()?, decoder.u16()?);
    if version
        != (
            NATIVE_V1_SNAPSHOT_VERSION.major(),
            NATIVE_V1_SNAPSHOT_VERSION.minor(),
            NATIVE_V1_SNAPSHOT_VERSION.patch(),
        )
    {
        return Err(HvfSnapshotV1DecodeError::UnsupportedVersion);
    }
    if decoder.u16()? != SNAPSHOT_PROFILE {
        return Err(HvfSnapshotV1DecodeError::UnsupportedProfile);
    }
    if decoder.u32()? != SNAPSHOT_FLAGS {
        return Err(HvfSnapshotV1DecodeError::UnsupportedFlags);
    }
    if decoder.u16()? != SNAPSHOT_COMPONENT_COUNT {
        return Err(HvfSnapshotV1DecodeError::InvalidComponentOrder);
    }
    if decoder.u16()? != SNAPSHOT_RESERVED_U16 {
        return Err(HvfSnapshotV1DecodeError::NonzeroReserved);
    }
    let declared_length =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV1DecodeError::LengthMismatch)?;
    if decoder.u32()? != SNAPSHOT_RESERVED_U32 {
        return Err(HvfSnapshotV1DecodeError::NonzeroReserved);
    }
    if declared_length != encoded.len() {
        return Err(HvfSnapshotV1DecodeError::LengthMismatch);
    }

    let machine = decode_required_component(&mut decoder, MACHINE_COMPONENT, decode_machine)?;
    let compatibility =
        decode_required_component(&mut decoder, COMPATIBILITY_COMPONENT, decode_compatibility)?;
    let vcpu = decode_required_component(&mut decoder, VCPU_COMPONENT, decode_vcpu)?;
    let interrupts =
        decode_required_component(&mut decoder, INTERRUPT_COMPONENT, decode_interrupts)?;
    let device = decode_required_component(&mut decoder, DEVICE_COMPONENT, |payload| {
        decode_snapshot_v1_device_state(payload).map_err(HvfSnapshotV1DecodeError::Device)
    })?;
    decoder.finish()?;

    let state = HvfSnapshotV1State::new(machine, compatibility, vcpu, interrupts, device);
    validate_native_v1_arm64_snapshot_optional_state(state.vcpu.execution, None, None, None)
        .map_err(|_| HvfSnapshotV1DecodeError::InvalidCompatibility("optional state is active"))?;
    Ok(state)
}

fn decode_required_component<T>(
    decoder: &mut Decoder<'_>,
    expected_kind: u16,
    decode: impl FnOnce(&[u8]) -> Result<T, HvfSnapshotV1DecodeError>,
) -> Result<T, HvfSnapshotV1DecodeError> {
    let kind = decoder.u16()?;
    if kind != expected_kind {
        return if (MACHINE_COMPONENT..=DEVICE_COMPONENT).contains(&kind) {
            Err(HvfSnapshotV1DecodeError::InvalidComponentOrder)
        } else {
            Err(HvfSnapshotV1DecodeError::UnsupportedComponent(kind))
        };
    }
    if decoder.u16()? != COMPONENT_FLAGS {
        return Err(HvfSnapshotV1DecodeError::UnsupportedFlags);
    }
    let length =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV1DecodeError::Truncated)?;
    let payload = decoder.slice(length)?;
    decode(payload)
}

fn decode_machine(payload: &[u8]) -> Result<MachineConfig, HvfSnapshotV1DecodeError> {
    let mut decoder = Decoder::new(payload);
    let vcpu_count = decoder.u8()?;
    let smt = decoder.bool()?;
    let track_dirty_pages = decoder.bool()?;
    let huge_pages = decoder.u8()?;
    let cpu_template_present = decoder.bool()?;
    decoder.zeroes(3)?;
    let memory_mib = decoder.u64()?;
    decoder.finish()?;
    if smt || huge_pages != 0 || cpu_template_present {
        return Err(HvfSnapshotV1DecodeError::InvalidMachine);
    }
    let machine = MachineConfigInput::new(vcpu_count, memory_mib)
        .with_track_dirty_pages(track_dirty_pages)
        .validate()
        .map_err(|_| HvfSnapshotV1DecodeError::InvalidMachine)?;
    validate_machine(machine).map_err(|_| HvfSnapshotV1DecodeError::InvalidMachine)?;
    Ok(machine)
}

fn decode_compatibility(
    payload: &[u8],
) -> Result<HvfSnapshotV1CompatibilityState, HvfSnapshotV1DecodeError> {
    let mut decoder = Decoder::new(payload);
    let mut identification_values = [0; 11];
    for value in &mut identification_values {
        *value = decoder.u64()?;
    }
    let identification = HvfArm64VcpuIdentificationRegisterState::new(identification_values);
    let optional_present = decoder.bool()?;
    decoder.zeroes(7)?;
    let optional_values = [decoder.u64()?, decoder.u64()?];
    let optional_sve_sme_identification = if optional_present {
        Some(HvfArm64VcpuSveSmeIdentificationRegisterState::new(
            optional_values[0],
            optional_values[1],
        ))
    } else {
        if optional_values != [0, 0] {
            return Err(HvfSnapshotV1DecodeError::InvalidCompatibility(
                "absent optional identification carries data",
            ));
        }
        None
    };
    if decoder.u16()? != SNAPSHOT_INACTIVE_OPTIONAL_STATE_POLICY {
        return Err(HvfSnapshotV1DecodeError::InvalidCompatibility(
            "optional-state policy is unsupported",
        ));
    }
    decoder.zeroes(6)?;

    let configuration =
        HvfArm64VcpuCacheConfiguration::new([decoder.u64()?, decoder.u64()?, decoder.u64()?]);
    let mut geometry_values = [[0; 8]; 2];
    for array in &mut geometry_values {
        for value in array {
            *value = decoder.u64()?;
        }
    }
    let cache_manifest = HvfArm64VcpuCacheManifest::new(
        configuration,
        HvfArm64VcpuCacheGeometry::new(geometry_values),
    );
    let primary_mpidr = decoder.u64()?;

    let distributor = decode_gic_region(&mut decoder)?;
    let redistributor_region = decode_gic_region(&mut decoder)?;
    let single_redistributor_size = decoder.u64()?;
    let spi_interrupt_range = decode_interrupt_range(&mut decoder)?;
    let timer_interrupts = HvfGicTimerInterrupts {
        el1_virtual_timer_intid: decoder.u32()?,
        el1_physical_timer_intid: decoder.u32()?,
    };
    let msi_present = decoder.bool()?;
    decoder.zeroes(7)?;
    let msi_region = decode_gic_region(&mut decoder)?;
    let msi_interrupt_range = decode_interrupt_range(&mut decoder)?;
    let msi = if msi_present {
        Some(HvfGicMsiMetadata {
            region: msi_region,
            interrupt_range: msi_interrupt_range,
        })
    } else {
        if msi_region != (HvfGicRegion { base: 0, size: 0 })
            || msi_interrupt_range != (HvfGicInterruptRange { base: 0, count: 0 })
        {
            return Err(HvfSnapshotV1DecodeError::InvalidCompatibility(
                "absent MSI metadata carries data",
            ));
        }
        None
    };
    let gic_metadata = HvfGicMetadata {
        distributor,
        redistributor: HvfGicRedistributor {
            region: redistributor_region,
            single_redistributor_size,
        },
        spi_interrupt_range,
        timer_interrupts,
        msi,
    };

    let rtc_mmio_layout = RtcMmioLayout::new(
        GuestAddress::new(decoder.u64()?),
        MmioRegionId::new(decoder.u64()?),
    );
    if decoder.u16()? != SNAPSHOT_FRESH_SYSTEM_RTC_POLICY {
        return Err(HvfSnapshotV1DecodeError::InvalidCompatibility(
            "RTC policy is unsupported",
        ));
    }
    decoder.zeroes(6)?;
    decoder.finish()?;

    let state = HvfSnapshotV1CompatibilityState::new(
        identification,
        optional_sve_sme_identification,
        cache_manifest,
        primary_mpidr,
        gic_metadata,
        rtc_mmio_layout,
    );
    validate_compatibility(&state).map_err(HvfSnapshotV1DecodeError::InvalidCompatibility)?;
    Ok(state)
}

fn decode_vcpu(payload: &[u8]) -> Result<HvfSnapshotV1VcpuState, HvfSnapshotV1DecodeError> {
    let mut decoder = Decoder::new(payload);
    let mut general_purpose_registers = [0; 31];
    for value in &mut general_purpose_registers {
        *value = decoder.u64()?;
    }
    let general = HvfArm64VcpuGeneralRegisterState::new(
        general_purpose_registers,
        decoder.u64()?,
        decoder.u64()?,
    );
    let core = HvfArm64VcpuCoreSystemRegisterState::new(
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
    );
    let exception = HvfArm64VcpuExceptionRegisterState::new(
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
    );
    let execution = HvfArm64VcpuExecutionControlRegisterState::new(decoder.u64()?, decoder.u64()?);
    let cache_selection = HvfArm64VcpuCacheSelectionRegisterState::new(decoder.u64()?);
    let debug_control = HvfArm64VcpuDebugControlRegisterState::new(decoder.u64()?, decoder.u64()?);
    let debug_trap = HvfArm64VcpuDebugTrapState::new(decoder.bool()?, decoder.bool()?);
    decoder.zeroes(6)?;
    let system_context =
        HvfArm64VcpuSystemContextRegisterState::new(decoder.u64()?, decoder.u64()?);
    let translation = HvfArm64VcpuTranslationRegisterState::new(
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
        decoder.u64()?,
    );
    let mut pointer_authentication_halves = [0; 10];
    for value in &mut pointer_authentication_halves {
        *value = decoder.u64()?;
    }
    let pointer_authentication =
        HvfArm64VcpuPointerAuthenticationKeyState::new(pointer_authentication_halves);
    let thread_context =
        HvfArm64VcpuThreadContextRegisterState::new(decoder.u64()?, decoder.u64()?, decoder.u64()?);
    let mut q_registers = [[0; 16]; 32];
    for value in &mut q_registers {
        *value = decoder.array::<16>()?;
    }
    let simd_fp = HvfArm64VcpuSimdFpState::new(q_registers, decoder.u64()?, decoder.u64()?);
    decoder.finish()?;

    Ok(HvfSnapshotV1VcpuState {
        general,
        core,
        exception,
        execution,
        cache_selection,
        debug_control,
        debug_trap,
        system_context,
        translation,
        pointer_authentication,
        thread_context,
        simd_fp,
    })
}

fn decode_interrupts(
    payload: &[u8],
) -> Result<HvfSnapshotV1InterruptState, HvfSnapshotV1DecodeError> {
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
    .map_err(HvfSnapshotV1DecodeError::InvalidTimer)?;
    let pending_interrupts =
        HvfArm64VcpuPendingInterruptState::new(decoder.bool()?, decoder.bool()?);
    decoder.zeroes(2)?;
    let gic_length =
        usize::try_from(decoder.u32()?).map_err(|_| HvfSnapshotV1DecodeError::InvalidGicState)?;
    if gic_length == 0 || gic_length > HVF_SNAPSHOT_V1_GIC_DEVICE_STATE_MAX_BYTES {
        return Err(HvfSnapshotV1DecodeError::InvalidGicState);
    }
    let gic_bytes = decoder.slice(gic_length)?;
    let mut detached_gic_bytes = Vec::new();
    detached_gic_bytes
        .try_reserve_exact(gic_length)
        .map_err(|_| HvfSnapshotV1DecodeError::Allocation)?;
    detached_gic_bytes.extend_from_slice(gic_bytes);
    let gic_device = HvfGicDeviceState::new(detached_gic_bytes);
    let mut icc_values = [0; 10];
    for value in &mut icc_values {
        *value = decoder.u64()?;
    }
    let gic_icc = HvfArm64GicIccRegisterState::new(icc_values);
    decoder.finish()?;
    Ok(HvfSnapshotV1InterruptState {
        timer,
        pending_interrupts,
        gic_device,
        gic_icc,
    })
}

fn decode_gic_region(decoder: &mut Decoder<'_>) -> Result<HvfGicRegion, HvfSnapshotV1DecodeError> {
    Ok(HvfGicRegion {
        base: decoder.u64()?,
        size: decoder.u64()?,
    })
}

fn decode_interrupt_range(
    decoder: &mut Decoder<'_>,
) -> Result<HvfGicInterruptRange, HvfSnapshotV1DecodeError> {
    Ok(HvfGicInterruptRange {
        base: decoder.u32()?,
        count: decoder.u32()?,
    })
}

struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn slice(&mut self, length: usize) -> Result<&'a [u8], HvfSnapshotV1DecodeError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(HvfSnapshotV1DecodeError::Truncated)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(HvfSnapshotV1DecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], HvfSnapshotV1DecodeError> {
        self.slice(N)?
            .try_into()
            .map_err(|_| HvfSnapshotV1DecodeError::Truncated)
    }

    fn u8(&mut self) -> Result<u8, HvfSnapshotV1DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn bool(&mut self) -> Result<bool, HvfSnapshotV1DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(HvfSnapshotV1DecodeError::InvalidBoolean),
        }
    }

    fn u16(&mut self) -> Result<u16, HvfSnapshotV1DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, HvfSnapshotV1DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, HvfSnapshotV1DecodeError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn zeroes(&mut self, length: usize) -> Result<(), HvfSnapshotV1DecodeError> {
        if self.slice(length)?.iter().any(|byte| *byte != 0) {
            Err(HvfSnapshotV1DecodeError::NonzeroReserved)
        } else {
            Ok(())
        }
    }

    fn finish(self) -> Result<(), HvfSnapshotV1DecodeError> {
        if self.position == self.bytes.len() {
            Ok(())
        } else {
            Err(HvfSnapshotV1DecodeError::TrailingData)
        }
    }
}

fn validate_machine(machine: MachineConfig) -> Result<(), HvfSnapshotV1EncodeError> {
    if machine.vcpu_count() != 1
        || machine.smt()
        || machine.cpu_template().is_some()
        || machine.huge_pages() != bangbang_runtime::machine::MachineConfigHugePages::None
    {
        Err(HvfSnapshotV1EncodeError::InvalidMachine)
    } else {
        Ok(())
    }
}

fn validate_compatibility(state: &HvfSnapshotV1CompatibilityState) -> Result<(), &'static str> {
    if state.primary_mpidr != state.identification.mpidr_el1() {
        return Err("primary MPIDR differs from vCPU identification");
    }
    let sve_present = ((state.identification.id_aa64pfr0_el1() >> 32) & 0xf) != 0xf;
    let sme_present = ((state.identification.id_aa64pfr1_el1() >> 24) & 0xf) != 0xf;
    if (sve_present || sme_present) != state.optional_sve_sme_identification.is_some() {
        return Err("optional SVE/SME identification presence disagrees with feature registers");
    }

    let gic = state.gic_metadata;
    validate_gic_region(gic.distributor, "GIC distributor")?;
    validate_gic_region(gic.redistributor.region, "GIC redistributor")?;
    if gic.redistributor.single_redistributor_size == 0
        || gic.redistributor.single_redistributor_size > gic.redistributor.region.size
        || !gic
            .redistributor
            .region
            .size
            .is_multiple_of(gic.redistributor.single_redistributor_size)
    {
        return Err("GIC redistributor stride is invalid");
    }
    if regions_overlap(gic.distributor, gic.redistributor.region) {
        return Err("GIC distributor and redistributor overlap");
    }
    validate_interrupt_range(gic.spi_interrupt_range)?;
    let virtual_timer = gic.timer_interrupts.el1_virtual_timer_intid;
    let physical_timer = gic.timer_interrupts.el1_physical_timer_intid;
    if virtual_timer != SNAPSHOT_VIRTUAL_TIMER_INTID
        || physical_timer != SNAPSHOT_PHYSICAL_TIMER_INTID
    {
        return Err("GIC timer interrupt metadata is invalid");
    }
    if gic.spi_interrupt_range.base != SNAPSHOT_SPI_INTERRUPT_BASE {
        return Err("GIC SPI interrupt range does not start at the baseline INTID");
    }
    if gic.msi.is_some() {
        return Err("GIC MSI state is outside the native-v1 profile");
    }

    let rtc_base = state.rtc_mmio_layout.base().raw_value();
    if rtc_base != SNAPSHOT_RTC_MMIO_BASE
        || state.rtc_mmio_layout.region_id().raw_value() != SNAPSHOT_RTC_MMIO_REGION_ID
    {
        return Err("RTC MMIO metadata differs from the native-v1 fixed mapping");
    }
    let rtc = HvfGicRegion {
        base: rtc_base,
        size: RTC_MMIO_DEVICE_WINDOW_SIZE,
    };
    validate_gic_region(rtc, "RTC")?;
    if regions_overlap(rtc, gic.distributor)
        || regions_overlap(rtc, gic.redistributor.region)
        || gic.msi.is_some_and(|msi| regions_overlap(rtc, msi.region))
    {
        return Err("RTC MMIO region overlaps GIC metadata");
    }
    Ok(())
}

fn validate_gic_region(region: HvfGicRegion, _name: &'static str) -> Result<(), &'static str> {
    if region.size == 0 || region.base.checked_add(region.size).is_none() {
        return Err("platform MMIO region is empty or overflows");
    }
    if region.end_exclusive() > aarch64::DRAM_MEM_START {
        return Err("platform MMIO region overlaps guest DRAM");
    }
    Ok(())
}

fn validate_interrupt_range(range: HvfGicInterruptRange) -> Result<(), &'static str> {
    if range.base < 32 || range.count == 0 || range.base.checked_add(range.count).is_none() {
        Err("GIC SPI interrupt range is invalid")
    } else {
        Ok(())
    }
}

fn regions_overlap(first: HvfGicRegion, second: HvfGicRegion) -> bool {
    first.base < second.end_exclusive() && second.base < first.end_exclusive()
}

fn validate_state_binding(
    binding: &SnapshotMemoryBinding,
    state: &HvfSnapshotV1State,
) -> Result<(), HvfSnapshotV1BundleError> {
    validate_machine(state.machine).map_err(|_| {
        HvfSnapshotV1BundleError::BindingMismatch("machine is outside the single-vCPU profile")
    })?;
    validate_compatibility(&state.compatibility)
        .map_err(HvfSnapshotV1BundleError::BindingMismatch)?;

    let expected_data_length = state.machine.mem_size_mib().checked_mul(MIB).ok_or(
        HvfSnapshotV1BundleError::BindingMismatch("machine memory size overflows bytes"),
    )?;
    if binding.data_length() != expected_data_length {
        return Err(HvfSnapshotV1BundleError::BindingMismatch(
            "machine memory size differs from the image",
        ));
    }
    let [range_binding] = binding.ranges() else {
        return Err(HvfSnapshotV1BundleError::BindingMismatch(
            "native-HVF profile requires one contiguous DRAM range",
        ));
    };
    let memory_range = range_binding.range();
    if memory_range.start().raw_value() != aarch64::DRAM_MEM_START
        || memory_range.size() != expected_data_length
    {
        return Err(HvfSnapshotV1BundleError::BindingMismatch(
            "memory range differs from the native arm64 DRAM layout",
        ));
    }

    for platform_range in [
        state.device.vmgenid().range(),
        state.device.vmclock().range(),
    ] {
        if !range_contains(memory_range, platform_range) {
            return Err(HvfSnapshotV1BundleError::BindingMismatch(
                "platform device state points outside the memory image",
            ));
        }
    }
    for queue in state.device.root_block().runtime().transport().queues() {
        if !queue.ready() {
            continue;
        }
        let size = u64::from(queue.size());
        for (address, length) in [
            (queue.descriptor_table(), size.saturating_mul(16)),
            (
                queue.driver_ring(),
                6u64.saturating_add(size.saturating_mul(2)),
            ),
            (
                queue.device_ring(),
                6u64.saturating_add(size.saturating_mul(8)),
            ),
        ] {
            if !range_contains_address(memory_range, address, length) {
                return Err(HvfSnapshotV1BundleError::BindingMismatch(
                    "virtio queue state points outside the memory image",
                ));
            }
        }
    }
    Ok(())
}

fn range_contains(outer: GuestMemoryRange, inner: GuestMemoryRange) -> bool {
    outer.start() <= inner.start() && inner.end_exclusive() <= outer.end_exclusive()
}

fn range_contains_address(outer: GuestMemoryRange, start: GuestAddress, size: u64) -> bool {
    size != 0
        && start
            .checked_add(size)
            .is_some_and(|end| outer.start() <= start && end <= outer.end_exclusive())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Cursor;

    use bangbang_runtime::fdt::Arm64FdtRegion;
    use bangbang_runtime::interrupt::GuestInterruptLine;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::{GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::snapshot_commit::SnapshotCommitKind;
    use bangbang_runtime::snapshot_device::{
        SnapshotV1DeviceState, SnapshotV1PlatformDeviceMetadata, decode_snapshot_v1_device_state,
    };
    use bangbang_runtime::snapshot_memory::{SnapshotMemoryBinding, write_snapshot_memory_image};

    use super::*;

    const DEVICE_FIXTURE_HEX: &str = r#"
42414e4744455600010000000000010000000000160200000000000000000000010001000100010000000000000000000600726f6f7466730f002f746d702f726f6f7466732e696d6701000900726f6f742d7061727400000101726f6f746673000000000000000000000000000001000000000000000100000000000000020000000000000000020000000000002481000000000000030000000000000004000000000000000500000000000000060000000000000001000000000000000000005000000000001000000000000020000000000000000200000000000000200000300100000000000000000000000000000000000000000000000000000000000000000101000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000014000000000000000020004000000000001000000000000021000000000000000183035a341200000010000000000000100000000000000000100000000000001000000000000000220000000000000000200000000000000010000000000000002000000000000000100000000000002300000000000000
"#;

    pub(crate) fn fixture() -> HvfSnapshotV1State {
        let machine = MachineConfigInput::new(1, 1)
            .validate()
            .expect("fixture machine should validate");
        let primary_mpidr = 0x8000_0000;
        let identification = HvfArm64VcpuIdentificationRegisterState::new([
            0x1111,
            primary_mpidr,
            0xf << 32,
            0xf << 24,
            0x2222,
            0x3333,
            0x4444,
            0x5555,
            0x6666,
            0x7777,
            0x8888,
        ]);
        let cache_manifest = HvfArm64VcpuCacheManifest::new(
            HvfArm64VcpuCacheConfiguration::new([0x101, 0x102, 0x103]),
            HvfArm64VcpuCacheGeometry::new([
                std::array::from_fn(|index| 0x200 + index as u64),
                std::array::from_fn(|index| 0x300 + index as u64),
            ]),
        );
        let gic_metadata = HvfGicMetadata {
            distributor: HvfGicRegion {
                base: 0x2f00_0000,
                size: 0x1_0000,
            },
            redistributor: HvfGicRedistributor {
                region: HvfGicRegion {
                    base: 0x3000_0000,
                    size: 0x2_0000,
                },
                single_redistributor_size: 0x2_0000,
            },
            spi_interrupt_range: HvfGicInterruptRange {
                base: 32,
                count: 64,
            },
            timer_interrupts: HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 27,
                el1_physical_timer_intid: 30,
            },
            msi: None,
        };
        let compatibility = HvfSnapshotV1CompatibilityState::new(
            identification,
            None,
            cache_manifest,
            primary_mpidr,
            gic_metadata,
            RtcMmioLayout::new(GuestAddress::new(0x4000_1000), MmioRegionId::new(10)),
        );

        let general = HvfArm64VcpuGeneralRegisterState::new(
            std::array::from_fn(|index| 0x1000 + index as u64),
            0x2000,
            0x3c5,
        );
        let q_registers = std::array::from_fn(|index| [index as u8; 16]);
        let vcpu = HvfSnapshotV1VcpuState {
            general,
            core: HvfArm64VcpuCoreSystemRegisterState::new(1, 2, 3, 4),
            exception: HvfArm64VcpuExceptionRegisterState::new(5, 6, 7, 8, 9, 10),
            execution: HvfArm64VcpuExecutionControlRegisterState::new(11, 0),
            cache_selection: HvfArm64VcpuCacheSelectionRegisterState::new(12),
            debug_control: HvfArm64VcpuDebugControlRegisterState::new(13, 0),
            debug_trap: HvfArm64VcpuDebugTrapState::new(false, true),
            system_context: HvfArm64VcpuSystemContextRegisterState::new(14, 15),
            translation: HvfArm64VcpuTranslationRegisterState::new(16, 17, 18, 19, 20, 21, 22),
            pointer_authentication: HvfArm64VcpuPointerAuthenticationKeyState::new([
                23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
            ]),
            thread_context: HvfArm64VcpuThreadContextRegisterState::new(33, 34, 35),
            simd_fp: HvfArm64VcpuSimdFpState::new(q_registers, 36, 37),
        };
        let interrupts = HvfSnapshotV1InterruptState {
            timer: HvfArm64SnapshotTimerState::try_new(false, 38, 39, 0, 40, 0, 41)
                .expect("fixture timer should validate"),
            pending_interrupts: HvfArm64VcpuPendingInterruptState::new(true, false),
            gic_device: HvfGicDeviceState::new(b"sensitive-gic-state".to_vec()),
            gic_icc: HvfArm64GicIccRegisterState::new([42, 43, 44, 45, 46, 47, 48, 49, 50, 51]),
        };

        HvfSnapshotV1State::new(machine, compatibility, vcpu, interrupts, device_fixture())
    }

    fn device_fixture() -> SnapshotV1DeviceState {
        let bytes = decode_hex(DEVICE_FIXTURE_HEX);
        let original = decode_snapshot_v1_device_state(&bytes)
            .expect("embedded native-v1 device fixture should decode");
        SnapshotV1DeviceState::new(
            original.root_block().clone(),
            original.block_retry(),
            original.serial_mmio(),
            original.serial_state(),
            platform(aarch64::DRAM_MEM_START + 0x1000, 16, 34),
            platform(aarch64::DRAM_MEM_START + 0x2000, 4096, 35),
        )
    }

    fn platform(base: u64, size: u64, interrupt: u32) -> SnapshotV1PlatformDeviceMetadata {
        SnapshotV1PlatformDeviceMetadata::new(
            GuestMemoryRange::new(GuestAddress::new(base), size)
                .expect("fixture platform range should validate"),
            Arm64FdtRegion { base, size },
            GuestInterruptLine::new(interrupt).expect("fixture interrupt should validate"),
        )
    }

    fn decode_hex(hex: &str) -> Vec<u8> {
        let digits: Vec<_> = hex
            .bytes()
            .filter(|byte| !byte.is_ascii_whitespace())
            .collect();
        assert!(digits.len().is_multiple_of(2));
        digits
            .chunks_exact(2)
            .map(|pair| (hex_digit(pair[0]) << 4) | hex_digit(pair[1]))
            .collect()
    }

    fn hex_digit(value: u8) -> u8 {
        match value {
            b'0'..=b'9' => value - b'0',
            b'a'..=b'f' => value - b'a' + 10,
            _ => panic!("invalid fixture hex"),
        }
    }

    fn memory_binding(memory_mib: u64) -> SnapshotMemoryBinding {
        let size = memory_mib * MIB;
        memory_binding_for_ranges(vec![
            GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START), size)
                .expect("fixture memory range should validate"),
        ])
    }

    fn memory_binding_for_ranges(ranges: Vec<GuestMemoryRange>) -> SnapshotMemoryBinding {
        let layout = GuestMemoryLayout::new(ranges).expect("fixture memory layout should validate");
        let memory = GuestMemory::allocate(&layout).expect("fixture memory should allocate");
        write_snapshot_memory_image(&memory, &mut Cursor::new(Vec::new()))
            .expect("fixture memory should encode")
    }

    fn component_offsets(encoded: &[u8]) -> Vec<usize> {
        let mut offsets = Vec::new();
        let mut offset = SNAPSHOT_HEADER_BYTES;
        while offset < encoded.len() {
            offsets.push(offset);
            let length = u32::from_le_bytes(
                encoded[offset + 4..offset + 8]
                    .try_into()
                    .expect("component length should exist"),
            ) as usize;
            offset += COMPONENT_HEADER_BYTES + length;
        }
        offsets
    }

    #[test]
    fn complete_state_codec_is_deterministic_and_exact() {
        let state = fixture();
        let first = encode_hvf_snapshot_v1_state(&state).expect("fixture should encode");
        let second = encode_hvf_snapshot_v1_state(&state).expect("fixture should re-encode");
        let decoded = decode_hvf_snapshot_v1_state(&first).expect("fixture should decode");

        assert_eq!(first, second);
        assert_eq!(decoded, state);
        assert_eq!(&first[..8], &SNAPSHOT_MAGIC);
        assert_eq!(component_offsets(&first).len(), 5);
        assert_eq!(
            u32::from_le_bytes(first[24..28].try_into().unwrap()) as usize,
            first.len()
        );
    }

    #[test]
    fn component_inventory_rejects_missing_duplicate_reordered_and_unknown_values() {
        let encoded = encode_hvf_snapshot_v1_state(&fixture()).expect("fixture should encode");
        let offsets = component_offsets(&encoded);

        let mut missing = encoded[..offsets[4]].to_vec();
        let missing_length =
            u32::try_from(missing.len()).expect("missing-component fixture length should fit u32");
        missing[24..28].copy_from_slice(&missing_length.to_le_bytes());
        assert!(matches!(
            decode_hvf_snapshot_v1_state(&missing),
            Err(HvfSnapshotV1DecodeError::Truncated)
        ));

        let mut duplicate = encoded.clone();
        duplicate[offsets[1]..offsets[1] + 2].copy_from_slice(&MACHINE_COMPONENT.to_le_bytes());
        assert!(matches!(
            decode_hvf_snapshot_v1_state(&duplicate),
            Err(HvfSnapshotV1DecodeError::InvalidComponentOrder)
        ));

        let mut reordered = encoded.clone();
        reordered[offsets[0]..offsets[0] + 2]
            .copy_from_slice(&COMPATIBILITY_COMPONENT.to_le_bytes());
        assert!(matches!(
            decode_hvf_snapshot_v1_state(&reordered),
            Err(HvfSnapshotV1DecodeError::InvalidComponentOrder)
        ));

        let mut unknown = encoded;
        unknown[offsets[0]..offsets[0] + 2].copy_from_slice(&99u16.to_le_bytes());
        assert!(matches!(
            decode_hvf_snapshot_v1_state(&unknown),
            Err(HvfSnapshotV1DecodeError::UnsupportedComponent(99))
        ));
    }

    #[test]
    fn headers_flags_and_policy_markers_fail_closed() {
        let encoded = encode_hvf_snapshot_v1_state(&fixture()).expect("fixture should encode");
        let component = component_offsets(&encoded)[0];

        for (offset, bytes, expected) in [
            (0, vec![0], HvfSnapshotV1DecodeError::InvalidMagic),
            (
                8,
                2u16.to_le_bytes().to_vec(),
                HvfSnapshotV1DecodeError::UnsupportedVersion,
            ),
            (
                14,
                2u16.to_le_bytes().to_vec(),
                HvfSnapshotV1DecodeError::UnsupportedProfile,
            ),
            (
                16,
                1u32.to_le_bytes().to_vec(),
                HvfSnapshotV1DecodeError::UnsupportedFlags,
            ),
            (
                22,
                1u16.to_le_bytes().to_vec(),
                HvfSnapshotV1DecodeError::NonzeroReserved,
            ),
            (
                28,
                1u32.to_le_bytes().to_vec(),
                HvfSnapshotV1DecodeError::NonzeroReserved,
            ),
            (
                component + 2,
                1u16.to_le_bytes().to_vec(),
                HvfSnapshotV1DecodeError::UnsupportedFlags,
            ),
        ] {
            let mut malformed = encoded.clone();
            malformed[offset..offset + bytes.len()].copy_from_slice(&bytes);
            let error = decode_hvf_snapshot_v1_state(&malformed)
                .expect_err("malformed fixed field should reject");
            assert_eq!(
                std::mem::discriminant(&error),
                std::mem::discriminant(&expected)
            );
        }

        let machine = component + COMPONENT_HEADER_BYTES;
        for offset in [machine + 1, machine + 3, machine + 4] {
            let mut nonbaseline = encoded.clone();
            nonbaseline[offset] = 1;
            assert!(matches!(
                decode_hvf_snapshot_v1_state(&nonbaseline),
                Err(HvfSnapshotV1DecodeError::InvalidMachine)
            ));
        }
        let mut tracked = encoded.clone();
        tracked[machine + 2] = 1;
        assert!(
            decode_hvf_snapshot_v1_state(&tracked)
                .expect("tracked machine bit should decode")
                .machine()
                .track_dirty_pages()
        );

        let compatibility = component_offsets(&encoded)[1] + COMPONENT_HEADER_BYTES;
        let optional_policy = compatibility + 11 * 8 + 1 + 7 + 2 * 8;
        let mut unsupported_policy = encoded;
        unsupported_policy[optional_policy..optional_policy + 2]
            .copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            decode_hvf_snapshot_v1_state(&unsupported_policy),
            Err(HvfSnapshotV1DecodeError::InvalidCompatibility(
                "optional-state policy is unsupported"
            ))
        ));
    }

    #[test]
    fn native_profile_rejects_nonbaseline_gic_rtc_and_optional_metadata() {
        let mut invalid_redistributor = fixture();
        invalid_redistributor
            .compatibility
            .gic_metadata
            .redistributor
            .single_redistributor_size += 1;
        assert!(matches!(
            encode_hvf_snapshot_v1_state(&invalid_redistributor),
            Err(HvfSnapshotV1EncodeError::InvalidCompatibility(_))
        ));

        let mut msi = fixture();
        msi.compatibility.gic_metadata.msi = Some(HvfGicMsiMetadata {
            region: HvfGicRegion {
                base: 0x3f00_0000,
                size: 0x1_0000,
            },
            interrupt_range: HvfGicInterruptRange {
                base: 128,
                count: 32,
            },
        });
        assert!(matches!(
            encode_hvf_snapshot_v1_state(&msi),
            Err(HvfSnapshotV1EncodeError::InvalidCompatibility(_))
        ));

        let mut timer = fixture();
        timer
            .compatibility
            .gic_metadata
            .timer_interrupts
            .el1_virtual_timer_intid = 26;
        assert!(matches!(
            encode_hvf_snapshot_v1_state(&timer),
            Err(HvfSnapshotV1EncodeError::InvalidCompatibility(_))
        ));

        let mut rtc = fixture();
        rtc.compatibility.rtc_mmio_layout = RtcMmioLayout::new(
            GuestAddress::new(SNAPSHOT_RTC_MMIO_BASE),
            MmioRegionId::new(SNAPSHOT_RTC_MMIO_REGION_ID + 1),
        );
        assert!(matches!(
            encode_hvf_snapshot_v1_state(&rtc),
            Err(HvfSnapshotV1EncodeError::InvalidCompatibility(_))
        ));

        let mut optional = fixture();
        optional.compatibility.optional_sve_sme_identification =
            Some(HvfArm64VcpuSveSmeIdentificationRegisterState::new(1, 2));
        assert!(matches!(
            encode_hvf_snapshot_v1_state(&optional),
            Err(HvfSnapshotV1EncodeError::InvalidCompatibility(_))
        ));
    }

    #[test]
    fn interrupt_decoder_rejects_gic_length_before_allocating_or_reading_bytes() {
        let encoded = encode_interrupts(fixture().interrupts())
            .expect("fixture interrupt state should encode");
        let mut prefix = encoded[..64].to_vec();
        prefix[60..64].copy_from_slice(
            &u32::try_from(HVF_SNAPSHOT_V1_GIC_DEVICE_STATE_MAX_BYTES + 1)
                .expect("GIC policy length should fit u32")
                .to_le_bytes(),
        );

        assert!(matches!(
            decode_interrupts(&prefix),
            Err(HvfSnapshotV1DecodeError::InvalidGicState)
        ));
    }

    #[test]
    fn codec_rejects_truncation_trailing_data_and_incompatible_metadata() {
        let state = fixture();
        let encoded = encode_hvf_snapshot_v1_state(&state).expect("fixture should encode");
        for length in [0, SNAPSHOT_HEADER_BYTES - 1, encoded.len() - 1] {
            assert!(decode_hvf_snapshot_v1_state(&encoded[..length]).is_err());
        }
        let mut trailing = encoded;
        trailing.push(0);
        assert!(matches!(
            decode_hvf_snapshot_v1_state(&trailing),
            Err(HvfSnapshotV1DecodeError::LengthMismatch)
        ));

        let mut incompatible = fixture();
        incompatible.compatibility.primary_mpidr ^= 1;
        assert!(matches!(
            encode_hvf_snapshot_v1_state(&incompatible),
            Err(HvfSnapshotV1EncodeError::InvalidCompatibility(_))
        ));

        let mut optional_active = fixture();
        optional_active.vcpu.execution =
            HvfArm64VcpuExecutionControlRegisterState::new(0, 0b11 << 16);
        assert!(matches!(
            encode_hvf_snapshot_v1_state(&optional_active),
            Err(HvfSnapshotV1EncodeError::InvalidCompatibility(_))
        ));
    }

    #[test]
    fn bundle_binds_complete_state_to_the_exact_memory_image() {
        let state = fixture();
        let bundle = HvfSnapshotV1Bundle::try_new(memory_binding(1), state.clone())
            .expect("matching state and memory should bundle");
        assert_eq!(bundle.commit_record().kind(), SnapshotCommitKind::Composite);
        let decoded = HvfSnapshotV1Bundle::try_from_commit_record(bundle.commit_record.clone())
            .expect("composite record should decode");
        assert_eq!(decoded.state(), &state);

        assert!(matches!(
            HvfSnapshotV1Bundle::try_new(memory_binding(2), state),
            Err(HvfSnapshotV1BundleError::BindingMismatch(_))
        ));

        let half = MIB / 2;
        let split_binding = memory_binding_for_ranges(vec![
            GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START), half)
                .expect("first split range should validate"),
            GuestMemoryRange::new(GuestAddress::new(aarch64::DRAM_MEM_START + half), half)
                .expect("second split range should validate"),
        ]);
        assert!(matches!(
            HvfSnapshotV1Bundle::try_new(split_binding, fixture()),
            Err(HvfSnapshotV1BundleError::BindingMismatch(
                "native-HVF profile requires one contiguous DRAM range"
            ))
        ));
    }

    #[test]
    fn complete_state_and_errors_redact_sensitive_values() {
        let state = fixture();
        let debug = format!("{state:?}");
        assert!(!debug.contains("sensitive-gic-state"));
        assert!(!debug.contains("rootfs.img"));
        assert!(!debug.contains("2147483648"));

        let mut incompatible = state;
        incompatible.compatibility.primary_mpidr ^= 1;
        let error =
            encode_hvf_snapshot_v1_state(&incompatible).expect_err("MPIDR mismatch should reject");
        let rendered = format!("{error}\n{error:?}");
        assert!(!rendered.contains("2147483648"));
        assert!(!rendered.contains("rootfs.img"));
        assert!(!rendered.contains("sensitive"));
    }
}
