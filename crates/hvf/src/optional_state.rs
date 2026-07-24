//! Checked restore policy for reviewed optional arm64 vCPU state.

use std::fmt;

use bangbang_runtime::BackendError;

use crate::vcpu::{
    HvfArm64VcpuSimdFpState, HvfArm64VcpuSmePstate, HvfArm64VcpuSveSmeIdentificationRegisterState,
    HvfRegister, HvfSimdFpRegister, HvfSystemRegister,
};

const DEBUG_CONTROL_ENABLE: u64 = 1;
const DEBUG_REGISTER_CAPACITY: usize = 16;
const SME_Z_REGISTER_COUNT: usize = 32;
const SME_P_REGISTER_COUNT: usize = 16;
const SME_VERSION_SME: u8 = 0;
const SME_VERSION_SME2: u8 = 1;
const SME_VERSION_SME2P1: u8 = 2;
const SME_VERSION_ABSENT: u8 = 0xf;

/// One reviewed optional value supplied explicitly or reset to the fresh
/// destination value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64OptionalStateValue<T> {
    /// Restore the supplied value.
    Explicit(T),
    /// Retain the value validated on the fresh destination owner.
    DestinationDefault,
}

impl<T> fmt::Debug for HvfArm64OptionalStateValue<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Explicit(_) => f.write_str("Explicit(<redacted>)"),
            Self::DestinationDefault => f.write_str("DestinationDefault"),
        }
    }
}

impl<T: Copy> HvfArm64OptionalStateValue<T> {
    const fn resolve(self, destination_default: T) -> T {
        match self {
            Self::Explicit(value) => value,
            Self::DestinationDefault => destination_default,
        }
    }
}

/// Value-free rejection while constructing reviewed optional restore state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64ReviewedOptionalStateBuildError {
    /// A debug-register count was zero, exceeded the architectural limit, or
    /// disagreed with `ID_AA64DFR0_EL1`.
    DebugRegisterCount,
    /// An explicit debug register appeared beyond the implemented count.
    DebugRegisterInventory,
    /// SME state and the requested `ID_AA64PFR1_EL1.SME` version disagreed.
    SmeVersion,
    /// Maximum SVL was zero, incorrectly aligned, or too small for streaming
    /// Q/Z alias validation.
    SmeMaximumSvl,
    /// Conditional Z, P, ZA, or ZT0 state disagreed with the target PSTATE.
    SmeConditionalInventory,
    /// A variable-width SME register did not have its exact SDK width.
    SmeRegisterWidth,
    /// The maximum-SVL square required for ZA overflowed `usize`.
    SmeRegisterSizeOverflow,
    /// ZT0 presence disagreed with the SME version or target ZA state.
    SmeZt0Dependency,
    /// A streaming Z low lane disagreed with the final Q register.
    SmeSimdAlias,
}

impl fmt::Display for HvfArm64ReviewedOptionalStateBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::DebugRegisterCount => "debug-register count",
            Self::DebugRegisterInventory => "debug-register inventory",
            Self::SmeVersion => "SME feature version",
            Self::SmeMaximumSvl => "SME maximum streaming vector length",
            Self::SmeConditionalInventory => "SME conditional-state inventory",
            Self::SmeRegisterWidth => "SME register width",
            Self::SmeRegisterSizeOverflow => "SME register size",
            Self::SmeZt0Dependency => "SME2 ZT0 dependency",
            Self::SmeSimdAlias => "SME/SIMD alias",
        };
        write!(f, "invalid reviewed optional arm64 state: {category}")
    }
}

impl std::error::Error for HvfArm64ReviewedOptionalStateBuildError {}

/// Exact presence-aware breakpoint or watchpoint restore inventory.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64DebugRegisterRestoreState {
    implemented_count: u8,
    values: [HvfArm64OptionalStateValue<u64>; DEBUG_REGISTER_CAPACITY],
    controls: [HvfArm64OptionalStateValue<u64>; DEBUG_REGISTER_CAPACITY],
}

impl fmt::Debug for HvfArm64DebugRegisterRestoreState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64DebugRegisterRestoreState")
            .field("implemented_count", &self.implemented_count)
            .field("registers", &"<redacted>")
            .finish()
    }
}

impl HvfArm64DebugRegisterRestoreState {
    /// Build one exact, presence-aware breakpoint or watchpoint inventory.
    pub fn try_new(
        implemented_count: u8,
        values: [HvfArm64OptionalStateValue<u64>; DEBUG_REGISTER_CAPACITY],
        controls: [HvfArm64OptionalStateValue<u64>; DEBUG_REGISTER_CAPACITY],
    ) -> Result<Self, HvfArm64ReviewedOptionalStateBuildError> {
        if implemented_count == 0 || usize::from(implemented_count) > DEBUG_REGISTER_CAPACITY {
            return Err(HvfArm64ReviewedOptionalStateBuildError::DebugRegisterCount);
        }
        let implemented = usize::from(implemented_count);
        let remaining_values = values
            .get(implemented..)
            .ok_or(HvfArm64ReviewedOptionalStateBuildError::DebugRegisterCount)?;
        let remaining_controls = controls
            .get(implemented..)
            .ok_or(HvfArm64ReviewedOptionalStateBuildError::DebugRegisterCount)?;
        if remaining_values
            .iter()
            .chain(remaining_controls)
            .any(|value| matches!(value, HvfArm64OptionalStateValue::Explicit(_)))
        {
            return Err(HvfArm64ReviewedOptionalStateBuildError::DebugRegisterInventory);
        }
        Ok(Self {
            implemented_count,
            values,
            controls,
        })
    }
}

type OptionalSmeBytes = HvfArm64OptionalStateValue<Box<[u8]>>;
type OptionalSmeRegisterInventory = Option<Box<[OptionalSmeBytes]>>;

/// Unchecked inputs for a reviewed SME restore.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64SmeRestoreStateInput {
    version: u8,
    identification: HvfArm64VcpuSveSmeIdentificationRegisterState,
    maximum_svl_bytes: usize,
    pstate: HvfArm64OptionalStateValue<HvfArm64VcpuSmePstate>,
    system_registers: [HvfArm64OptionalStateValue<u64>; 3],
    z_registers: Option<Vec<OptionalSmeBytes>>,
    p_registers: Option<Vec<OptionalSmeBytes>>,
    za_register: Option<OptionalSmeBytes>,
    zt0_register: Option<HvfArm64OptionalStateValue<[u8; 64]>>,
}

impl fmt::Debug for HvfArm64SmeRestoreStateInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64SmeRestoreStateInput")
            .field("version", &self.version)
            .field("maximum_svl_bytes", &self.maximum_svl_bytes)
            .field("pstate", &self.pstate)
            .field("registers", &"<redacted>")
            .finish()
    }
}

impl HvfArm64SmeRestoreStateInput {
    /// Start one SME input with feature evidence, PSTATE, and system state.
    pub fn new(
        version: u8,
        identification: HvfArm64VcpuSveSmeIdentificationRegisterState,
        maximum_svl_bytes: usize,
        pstate: HvfArm64OptionalStateValue<HvfArm64VcpuSmePstate>,
        system_registers: [HvfArm64OptionalStateValue<u64>; 3],
    ) -> Self {
        Self {
            version,
            identification,
            maximum_svl_bytes,
            pstate,
            system_registers,
            z_registers: None,
            p_registers: None,
            za_register: None,
            zt0_register: None,
        }
    }

    /// Supply the exact streaming Z and predicate inventories.
    #[must_use]
    pub fn with_streaming_registers(
        mut self,
        z_registers: Vec<HvfArm64OptionalStateValue<Box<[u8]>>>,
        p_registers: Vec<HvfArm64OptionalStateValue<Box<[u8]>>>,
    ) -> Self {
        self.z_registers = Some(z_registers);
        self.p_registers = Some(p_registers);
        self
    }

    /// Supply ZA and the optional SME2 ZT0 state.
    #[must_use]
    pub fn with_za_register(
        mut self,
        za_register: HvfArm64OptionalStateValue<Box<[u8]>>,
        zt0_register: Option<HvfArm64OptionalStateValue<[u8; 64]>>,
    ) -> Self {
        self.za_register = Some(za_register);
        self.zt0_register = zt0_register;
        self
    }
}

/// Checked presence-aware SME state for one compatible destination.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64SmeRestoreState {
    version: u8,
    identification: HvfArm64VcpuSveSmeIdentificationRegisterState,
    maximum_svl_bytes: usize,
    pstate: HvfArm64OptionalStateValue<HvfArm64VcpuSmePstate>,
    system_registers: [HvfArm64OptionalStateValue<u64>; 3],
    z_registers: OptionalSmeRegisterInventory,
    p_registers: OptionalSmeRegisterInventory,
    za_register: Option<OptionalSmeBytes>,
    zt0_register: Option<HvfArm64OptionalStateValue<[u8; 64]>>,
}

impl fmt::Debug for HvfArm64SmeRestoreState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64SmeRestoreState")
            .field("version", &self.version)
            .field("maximum_svl_bytes", &self.maximum_svl_bytes)
            .field("pstate", &self.pstate)
            .field("registers", &"<redacted>")
            .finish()
    }
}

struct SmeRestoreValidation<'a> {
    version: u8,
    maximum_svl_bytes: usize,
    pstate: HvfArm64OptionalStateValue<HvfArm64VcpuSmePstate>,
    z_registers: Option<&'a [OptionalSmeBytes]>,
    p_registers: Option<&'a [OptionalSmeBytes]>,
    za_register: Option<&'a OptionalSmeBytes>,
    zt0_register: Option<&'a HvfArm64OptionalStateValue<[u8; 64]>>,
}

fn validate_sme_restore_state(
    state: SmeRestoreValidation<'_>,
    simd_fp: &HvfArm64VcpuSimdFpState,
) -> Result<(), HvfArm64ReviewedOptionalStateBuildError> {
    if !matches!(
        state.version,
        SME_VERSION_SME | SME_VERSION_SME2 | SME_VERSION_SME2P1
    ) {
        return Err(HvfArm64ReviewedOptionalStateBuildError::SmeVersion);
    }
    if state.maximum_svl_bytes == 0 || !state.maximum_svl_bytes.is_multiple_of(8) {
        return Err(HvfArm64ReviewedOptionalStateBuildError::SmeMaximumSvl);
    }

    let target_pstate = state
        .pstate
        .resolve(HvfArm64VcpuSmePstate::new(false, false));
    if target_pstate.streaming_sve_mode_enabled() {
        if state.maximum_svl_bytes < 16 {
            return Err(HvfArm64ReviewedOptionalStateBuildError::SmeMaximumSvl);
        }
        let z_registers = state
            .z_registers
            .ok_or(HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory)?;
        let p_registers = state
            .p_registers
            .ok_or(HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory)?;
        if z_registers.len() != SME_Z_REGISTER_COUNT || p_registers.len() != SME_P_REGISTER_COUNT {
            return Err(HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory);
        }
        let predicate_width = state.maximum_svl_bytes / 8;
        for (index, value) in z_registers.iter().enumerate() {
            match value {
                HvfArm64OptionalStateValue::Explicit(bytes) => {
                    if bytes.len() != state.maximum_svl_bytes {
                        return Err(HvfArm64ReviewedOptionalStateBuildError::SmeRegisterWidth);
                    }
                    let q_register = simd_fp
                        .q_register(index)
                        .ok_or(HvfArm64ReviewedOptionalStateBuildError::SmeSimdAlias)?;
                    if bytes.get(..16) != Some(q_register.as_slice()) {
                        return Err(HvfArm64ReviewedOptionalStateBuildError::SmeSimdAlias);
                    }
                }
                HvfArm64OptionalStateValue::DestinationDefault => {
                    if simd_fp.q_register(index) != Some([0; 16]) {
                        return Err(HvfArm64ReviewedOptionalStateBuildError::SmeSimdAlias);
                    }
                }
            }
        }
        if p_registers.iter().any(|value| {
            matches!(
                value,
                HvfArm64OptionalStateValue::Explicit(bytes)
                    if bytes.len() != predicate_width
            )
        }) {
            return Err(HvfArm64ReviewedOptionalStateBuildError::SmeRegisterWidth);
        }
    } else if state.z_registers.is_some() || state.p_registers.is_some() {
        return Err(HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory);
    }

    if target_pstate.za_storage_enabled() {
        let za_register = state
            .za_register
            .ok_or(HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory)?;
        let za_size = state
            .maximum_svl_bytes
            .checked_mul(state.maximum_svl_bytes)
            .ok_or(HvfArm64ReviewedOptionalStateBuildError::SmeRegisterSizeOverflow)?;
        if matches!(
            za_register,
            HvfArm64OptionalStateValue::Explicit(bytes) if bytes.len() != za_size
        ) {
            return Err(HvfArm64ReviewedOptionalStateBuildError::SmeRegisterWidth);
        }
        if state.version >= SME_VERSION_SME2 {
            if state.zt0_register.is_none() {
                return Err(HvfArm64ReviewedOptionalStateBuildError::SmeZt0Dependency);
            }
        } else if state.zt0_register.is_some() {
            return Err(HvfArm64ReviewedOptionalStateBuildError::SmeZt0Dependency);
        }
    } else if state.za_register.is_some() || state.zt0_register.is_some() {
        return Err(HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory);
    }

    Ok(())
}

impl HvfArm64SmeRestoreState {
    /// Build checked presence-aware SME state for one destination feature level.
    pub fn try_new(
        input: HvfArm64SmeRestoreStateInput,
        simd_fp: &HvfArm64VcpuSimdFpState,
    ) -> Result<Self, HvfArm64ReviewedOptionalStateBuildError> {
        validate_sme_restore_state(
            SmeRestoreValidation {
                version: input.version,
                maximum_svl_bytes: input.maximum_svl_bytes,
                pstate: input.pstate,
                z_registers: input.z_registers.as_deref(),
                p_registers: input.p_registers.as_deref(),
                za_register: input.za_register.as_ref(),
                zt0_register: input.zt0_register.as_ref(),
            },
            simd_fp,
        )?;

        Ok(Self {
            version: input.version,
            identification: input.identification,
            maximum_svl_bytes: input.maximum_svl_bytes,
            pstate: input.pstate,
            system_registers: input.system_registers,
            z_registers: input.z_registers.map(Vec::into_boxed_slice),
            p_registers: input.p_registers.map(Vec::into_boxed_slice),
            za_register: input.za_register,
            zt0_register: input.zt0_register,
        })
    }

    fn target_pstate(&self) -> HvfArm64VcpuSmePstate {
        self.pstate
            .resolve(HvfArm64VcpuSmePstate::new(false, false))
    }

    fn validate(
        &self,
        simd_fp: &HvfArm64VcpuSimdFpState,
    ) -> Result<(), HvfArm64ReviewedOptionalStateBuildError> {
        validate_sme_restore_state(
            SmeRestoreValidation {
                version: self.version,
                maximum_svl_bytes: self.maximum_svl_bytes,
                pstate: self.pstate,
                z_registers: self.z_registers.as_deref(),
                p_registers: self.p_registers.as_deref(),
                za_register: self.za_register.as_ref(),
                zt0_register: self.zt0_register.as_ref(),
            },
            simd_fp,
        )
    }
}

/// Checked, detached optional arm64 state consumed by one never-run owner.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64ReviewedOptionalStateRestore {
    expected_id_aa64dfr0_el1: u64,
    expected_sme_version: Option<u8>,
    breakpoints: HvfArm64DebugRegisterRestoreState,
    watchpoints: HvfArm64DebugRegisterRestoreState,
    sme: Option<HvfArm64SmeRestoreState>,
    simd_fp: HvfArm64VcpuSimdFpState,
}

impl fmt::Debug for HvfArm64ReviewedOptionalStateRestore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64ReviewedOptionalStateRestore")
            .field("breakpoint_count", &self.breakpoints.implemented_count)
            .field("watchpoint_count", &self.watchpoints.implemented_count)
            .field("sme_present", &self.sme.is_some())
            .field("state", &"<redacted>")
            .finish()
    }
}

impl HvfArm64ReviewedOptionalStateRestore {
    /// Build a checked reviewed-optional restore request.
    pub fn try_new(
        expected_id_aa64dfr0_el1: u64,
        expected_sme_version: Option<u8>,
        breakpoints: HvfArm64DebugRegisterRestoreState,
        watchpoints: HvfArm64DebugRegisterRestoreState,
        sme: Option<HvfArm64SmeRestoreState>,
        simd_fp: HvfArm64VcpuSimdFpState,
    ) -> Result<Self, HvfArm64ReviewedOptionalStateBuildError> {
        if breakpoint_count(expected_id_aa64dfr0_el1) != breakpoints.implemented_count
            || watchpoint_count(expected_id_aa64dfr0_el1) != watchpoints.implemented_count
        {
            return Err(HvfArm64ReviewedOptionalStateBuildError::DebugRegisterCount);
        }
        match (expected_sme_version, sme.as_ref()) {
            (None, None) => {}
            (Some(version), Some(state)) if version == state.version => {
                state.validate(&simd_fp)?;
            }
            _ => return Err(HvfArm64ReviewedOptionalStateBuildError::SmeVersion),
        }
        Ok(Self {
            expected_id_aa64dfr0_el1,
            expected_sme_version,
            breakpoints,
            watchpoints,
            sme,
            simd_fp,
        })
    }
}

const fn breakpoint_count(id_aa64dfr0_el1: u64) -> u8 {
    (((id_aa64dfr0_el1 >> 12) & 0xf) as u8) + 1
}

const fn watchpoint_count(id_aa64dfr0_el1: u64) -> u8 {
    (((id_aa64dfr0_el1 >> 20) & 0xf) as u8) + 1
}

const fn sme_version(id_aa64pfr1_el1: u64) -> Option<u8> {
    let version = ((id_aa64pfr1_el1 >> 24) & 0xf) as u8;
    if version == SME_VERSION_ABSENT {
        None
    } else {
        Some(version)
    }
}

/// Optional-state family associated with a restore failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfArm64ReviewedOptionalStateRestoreFamily {
    /// Cross-family destination validation.
    Destination,
    /// Breakpoint values and controls.
    Breakpoint,
    /// Watchpoint values and controls.
    Watchpoint,
    /// SME feature, control, and conditional register state.
    Sme,
    /// Final authoritative Q, FPCR, and FPSR state.
    SimdFp,
}

impl fmt::Display for HvfArm64ReviewedOptionalStateRestoreFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Destination => "destination",
            Self::Breakpoint => "breakpoint",
            Self::Watchpoint => "watchpoint",
            Self::Sme => "SME",
            Self::SimdFp => "SIMD/FP",
        };
        f.write_str(name)
    }
}

/// Value-free stage associated with a reviewed optional restore failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HvfArm64ReviewedOptionalStateRestoreStage {
    /// Read immutable destination feature evidence.
    FeatureRead,
    /// Read a destination value selected by `DestinationDefault`.
    ValueDefaultRead,
    /// Read and validate a fresh debug control.
    ControlDefaultRead,
    /// Validate already-read destination evidence or request state.
    Validation,
    /// Publish one disabled debug control before comparator values.
    DisableControl,
    /// Write one debug comparator value.
    ValueWrite,
    /// Publish one final debug control.
    FinalControl,
    /// Write one SME system register.
    SystemWrite,
    /// Change SME PSTATE.
    PstateWrite,
    /// Read conditional SME state exposed after the PSTATE transition.
    ConditionalDefaultRead,
    /// Write one conditional SME register.
    ConditionalWrite,
    /// Write one authoritative SIMD/FP register.
    FinalWrite,
}

impl fmt::Display for HvfArm64ReviewedOptionalStateRestoreStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::FeatureRead => "feature read",
            Self::ValueDefaultRead => "value-default read",
            Self::ControlDefaultRead => "control-default read",
            Self::Validation => "validation",
            Self::DisableControl => "control disable",
            Self::ValueWrite => "value write",
            Self::FinalControl => "final control write",
            Self::SystemWrite => "system write",
            Self::PstateWrite => "PSTATE write",
            Self::ConditionalDefaultRead => "conditional-default read",
            Self::ConditionalWrite => "conditional write",
            Self::FinalWrite => "final write",
        };
        f.write_str(name)
    }
}

/// Value-free destination rejection during reviewed optional restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64ReviewedOptionalStateRestoreRejection {
    /// Full debug identification differed from the request.
    DebugIdentification,
    /// Implemented debug-register counts differed from the request.
    DebugRegisterCount,
    /// A supposedly fresh debug control was enabled.
    FreshDebugControl,
    /// The destination SME version differed from the request.
    SmeFeature,
    /// SVE/SME identification registers differed from the request.
    SmeIdentification,
    /// The destination maximum SVL differed from the request.
    SmeMaximumSvl,
    /// The fresh destination entered restore with SME PSTATE active.
    FreshSmePstate,
    /// An SDK-documented zero transition default was nonzero.
    SmeTransitionDefault,
    /// The detached request failed its defensive validation.
    Request,
}

impl fmt::Display for HvfArm64ReviewedOptionalStateRestoreRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match self {
            Self::DebugIdentification => "debug identification",
            Self::DebugRegisterCount => "debug-register count",
            Self::FreshDebugControl => "fresh debug control",
            Self::SmeFeature => "SME feature",
            Self::SmeIdentification => "SME identification",
            Self::SmeMaximumSvl => "SME maximum streaming vector length",
            Self::FreshSmePstate => "fresh SME PSTATE",
            Self::SmeTransitionDefault => "SME transition default",
            Self::Request => "restore request",
        };
        write!(
            f,
            "reviewed optional arm64 destination rejected: {category}"
        )
    }
}

impl std::error::Error for HvfArm64ReviewedOptionalStateRestoreRejection {}

#[derive(Clone, PartialEq, Eq)]
enum HvfArm64ReviewedOptionalStateRestoreErrorSource {
    Backend(BackendError),
    Rejection(HvfArm64ReviewedOptionalStateRestoreRejection),
}

impl fmt::Debug for HvfArm64ReviewedOptionalStateRestoreErrorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(_) => f.write_str("Backend(<redacted>)"),
            Self::Rejection(rejection) => f.debug_tuple("Rejection").field(rejection).finish(),
        }
    }
}

/// Redacted failure from one ordered reviewed optional-state restore.
///
/// `Display` and `Debug` omit guest values, raw register identifiers, and
/// backend text. The concrete backend or rejection remains available through
/// [`std::error::Error::source`].
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64ReviewedOptionalStateRestoreError {
    family: HvfArm64ReviewedOptionalStateRestoreFamily,
    stage: HvfArm64ReviewedOptionalStateRestoreStage,
    index: Option<u8>,
    completed_writes: usize,
    source: HvfArm64ReviewedOptionalStateRestoreErrorSource,
}

impl HvfArm64ReviewedOptionalStateRestoreError {
    fn backend(
        family: HvfArm64ReviewedOptionalStateRestoreFamily,
        stage: HvfArm64ReviewedOptionalStateRestoreStage,
        index: Option<u8>,
        completed_writes: usize,
        source: BackendError,
    ) -> Self {
        Self {
            family,
            stage,
            index,
            completed_writes,
            source: HvfArm64ReviewedOptionalStateRestoreErrorSource::Backend(source),
        }
    }

    fn rejection(
        family: HvfArm64ReviewedOptionalStateRestoreFamily,
        stage: HvfArm64ReviewedOptionalStateRestoreStage,
        index: Option<u8>,
        completed_writes: usize,
        source: HvfArm64ReviewedOptionalStateRestoreRejection,
    ) -> Self {
        Self {
            family,
            stage,
            index,
            completed_writes,
            source: HvfArm64ReviewedOptionalStateRestoreErrorSource::Rejection(source),
        }
    }

    /// Return the state family whose operation failed.
    pub const fn family(&self) -> HvfArm64ReviewedOptionalStateRestoreFamily {
        self.family
    }

    /// Return the ordered restore stage that failed.
    pub const fn stage(&self) -> HvfArm64ReviewedOptionalStateRestoreStage {
        self.stage
    }

    /// Return the value-free operation index, when the stage addresses one
    /// member.
    ///
    /// Debug indices are family-local slots. SME conditional indices are
    /// Z0-Z31, P0-P15 shifted to 32-47, ZA at 48, and ZT0 at 49. SIMD/FP
    /// indices are Q0-Q31, FPCR at 32, and FPSR at 33.
    pub const fn index(&self) -> Option<u8> {
        self.index
    }

    /// Return the number of destination writes that completed successfully
    /// before this failure.
    pub const fn completed_writes(&self) -> usize {
        self.completed_writes
    }
}

impl fmt::Debug for HvfArm64ReviewedOptionalStateRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64ReviewedOptionalStateRestoreError")
            .field("family", &self.family)
            .field("stage", &self.stage)
            .field("index", &self.index)
            .field("completed_writes", &self.completed_writes)
            .field("source", &self.source)
            .finish()
    }
}

impl fmt::Display for HvfArm64ReviewedOptionalStateRestoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "reviewed optional arm64 {} {} failed",
            self.family, self.stage
        )?;
        if let Some(index) = self.index {
            write!(f, " at index {index}")?;
        }
        write!(f, " after {} successful writes", self.completed_writes)
    }
}

impl std::error::Error for HvfArm64ReviewedOptionalStateRestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.source {
            HvfArm64ReviewedOptionalStateRestoreErrorSource::Backend(source) => Some(source),
            HvfArm64ReviewedOptionalStateRestoreErrorSource::Rejection(source) => Some(source),
        }
    }
}

pub(crate) trait HvfArm64ReviewedOptionalStateAccess {
    fn read_system_register(&mut self, register: HvfSystemRegister) -> Result<u64, BackendError>;
    fn write_system_register(
        &mut self,
        register: HvfSystemRegister,
        value: u64,
    ) -> Result<(), BackendError>;
    fn get_sme_pstate(&mut self) -> Result<(bool, bool), BackendError>;
    fn set_sme_pstate(
        &mut self,
        streaming_sve_mode_enabled: bool,
        za_storage_enabled: bool,
    ) -> Result<(), BackendError>;
    fn get_sme_maximum_svl_bytes(&mut self) -> Result<usize, BackendError>;
    fn get_sme_z_register(&mut self, register: u32, value: &mut [u8]) -> Result<(), BackendError>;
    fn set_sme_z_register(&mut self, register: u32, value: &[u8]) -> Result<(), BackendError>;
    fn get_sme_p_register(&mut self, register: u32, value: &mut [u8]) -> Result<(), BackendError>;
    fn set_sme_p_register(&mut self, register: u32, value: &[u8]) -> Result<(), BackendError>;
    fn get_sme_za_register(&mut self, value: &mut [u8]) -> Result<(), BackendError>;
    fn set_sme_za_register(&mut self, value: &[u8]) -> Result<(), BackendError>;
    fn get_sme_zt0_register(&mut self) -> Result<[u8; 64], BackendError>;
    fn set_sme_zt0_register(&mut self, value: [u8; 64]) -> Result<(), BackendError>;
    fn write_simd_fp_register(
        &mut self,
        register: HvfSimdFpRegister,
        value: [u8; 16],
    ) -> Result<(), BackendError>;
    fn write_scalar_register(
        &mut self,
        register: HvfRegister,
        value: u64,
    ) -> Result<(), BackendError>;
}

fn allocate_zeroed(size: usize) -> Result<Vec<u8>, BackendError> {
    let mut value = Vec::new();
    value
        .try_reserve_exact(size)
        .map_err(|_| BackendError::InvalidState("reviewed optional restore allocation failed"))?;
    value.resize(size, 0);
    Ok(value)
}

fn map_backend(
    family: HvfArm64ReviewedOptionalStateRestoreFamily,
    stage: HvfArm64ReviewedOptionalStateRestoreStage,
    index: Option<u8>,
    completed_writes: usize,
) -> impl FnOnce(BackendError) -> HvfArm64ReviewedOptionalStateRestoreError {
    move |source| {
        HvfArm64ReviewedOptionalStateRestoreError::backend(
            family,
            stage,
            index,
            completed_writes,
            source,
        )
    }
}

fn reject<T>(
    family: HvfArm64ReviewedOptionalStateRestoreFamily,
    stage: HvfArm64ReviewedOptionalStateRestoreStage,
    index: Option<u8>,
    completed_writes: usize,
    source: HvfArm64ReviewedOptionalStateRestoreRejection,
) -> Result<T, HvfArm64ReviewedOptionalStateRestoreError> {
    Err(HvfArm64ReviewedOptionalStateRestoreError::rejection(
        family,
        stage,
        index,
        completed_writes,
        source,
    ))
}

fn write_system<A: HvfArm64ReviewedOptionalStateAccess + ?Sized>(
    access: &mut A,
    register: HvfSystemRegister,
    value: u64,
    family: HvfArm64ReviewedOptionalStateRestoreFamily,
    stage: HvfArm64ReviewedOptionalStateRestoreStage,
    index: u8,
    completed_writes: &mut usize,
) -> Result<(), HvfArm64ReviewedOptionalStateRestoreError> {
    access
        .write_system_register(register, value)
        .map_err(map_backend(family, stage, Some(index), *completed_writes))?;
    *completed_writes += 1;
    Ok(())
}

fn debug_registers(breakpoint: bool, index: u8) -> Option<(HvfSystemRegister, HvfSystemRegister)> {
    if breakpoint {
        Some((
            HvfSystemRegister::debug_breakpoint_value(index)?,
            HvfSystemRegister::debug_breakpoint_control(index)?,
        ))
    } else {
        Some((
            HvfSystemRegister::debug_watchpoint_value(index)?,
            HvfSystemRegister::debug_watchpoint_control(index)?,
        ))
    }
}

fn materialize_debug<A: HvfArm64ReviewedOptionalStateAccess + ?Sized>(
    access: &mut A,
    request: &HvfArm64DebugRegisterRestoreState,
    breakpoint: bool,
    family: HvfArm64ReviewedOptionalStateRestoreFamily,
) -> Result<([u64; 16], [u64; 16]), HvfArm64ReviewedOptionalStateRestoreError> {
    let mut values = [0; DEBUG_REGISTER_CAPACITY];
    let mut controls = [0; DEBUG_REGISTER_CAPACITY];
    for index in 0..request.implemented_count {
        let offset = usize::from(index);
        let requested_value = request.values.get(offset).copied().ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                0,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        let requested_control = request.controls.get(offset).copied().ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                0,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        let (value_register, control_register) =
            debug_registers(breakpoint, index).ok_or_else(|| {
                HvfArm64ReviewedOptionalStateRestoreError::rejection(
                    family,
                    HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                    Some(index),
                    0,
                    HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
                )
            })?;
        let control = access
            .read_system_register(control_register)
            .map_err(map_backend(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::ControlDefaultRead,
                Some(index),
                0,
            ))?;
        if control & DEBUG_CONTROL_ENABLE != 0 {
            return reject(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                0,
                HvfArm64ReviewedOptionalStateRestoreRejection::FreshDebugControl,
            );
        }
        let value = match requested_value {
            HvfArm64OptionalStateValue::Explicit(value) => value,
            HvfArm64OptionalStateValue::DestinationDefault => access
                .read_system_register(value_register)
                .map_err(map_backend(
                    family,
                    HvfArm64ReviewedOptionalStateRestoreStage::ValueDefaultRead,
                    Some(index),
                    0,
                ))?,
        };
        let materialized_value = values.get_mut(offset).ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                0,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        let materialized_control = controls.get_mut(offset).ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                0,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        *materialized_value = value;
        *materialized_control = requested_control.resolve(control);
    }
    Ok((values, controls))
}

fn restore_debug<A: HvfArm64ReviewedOptionalStateAccess + ?Sized>(
    access: &mut A,
    request: &HvfArm64DebugRegisterRestoreState,
    values: &[u64; 16],
    controls: &[u64; 16],
    breakpoint: bool,
    family: HvfArm64ReviewedOptionalStateRestoreFamily,
    completed_writes: &mut usize,
) -> Result<(), HvfArm64ReviewedOptionalStateRestoreError> {
    for index in 0..request.implemented_count {
        let (_, control_register) = debug_registers(breakpoint, index).ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                *completed_writes,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        let control = controls.get(usize::from(index)).copied().ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                *completed_writes,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        write_system(
            access,
            control_register,
            control & !DEBUG_CONTROL_ENABLE,
            family,
            HvfArm64ReviewedOptionalStateRestoreStage::DisableControl,
            index,
            completed_writes,
        )?;
    }
    for index in 0..request.implemented_count {
        let (value_register, _) = debug_registers(breakpoint, index).ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                *completed_writes,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        let value = values.get(usize::from(index)).copied().ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                *completed_writes,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        write_system(
            access,
            value_register,
            value,
            family,
            HvfArm64ReviewedOptionalStateRestoreStage::ValueWrite,
            index,
            completed_writes,
        )?;
    }
    for index in 0..request.implemented_count {
        let (_, control_register) = debug_registers(breakpoint, index).ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                *completed_writes,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        let control = controls.get(usize::from(index)).copied().ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                family,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index),
                *completed_writes,
                HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
            )
        })?;
        write_system(
            access,
            control_register,
            control,
            family,
            HvfArm64ReviewedOptionalStateRestoreStage::FinalControl,
            index,
            completed_writes,
        )?;
    }
    Ok(())
}

/// Validate and restore reviewed optional state through one owner access.
pub(crate) fn restore_arm64_reviewed_optional_state_with<
    A: HvfArm64ReviewedOptionalStateAccess + ?Sized,
>(
    access: &mut A,
    request: &HvfArm64ReviewedOptionalStateRestore,
) -> Result<(), HvfArm64ReviewedOptionalStateRestoreError> {
    request
        .sme
        .as_ref()
        .map_or(Ok(()), |sme| sme.validate(&request.simd_fp))
        .map_err(|_| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                HvfArm64ReviewedOptionalStateRestoreFamily::Destination,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                None,
                0,
                HvfArm64ReviewedOptionalStateRestoreRejection::Request,
            )
        })?;

    let destination_dfr0 = access
        .read_system_register(HvfSystemRegister::ID_AA64DFR0_EL1)
        .map_err(map_backend(
            HvfArm64ReviewedOptionalStateRestoreFamily::Destination,
            HvfArm64ReviewedOptionalStateRestoreStage::FeatureRead,
            None,
            0,
        ))?;
    if destination_dfr0 != request.expected_id_aa64dfr0_el1 {
        return reject(
            HvfArm64ReviewedOptionalStateRestoreFamily::Destination,
            HvfArm64ReviewedOptionalStateRestoreStage::Validation,
            None,
            0,
            HvfArm64ReviewedOptionalStateRestoreRejection::DebugIdentification,
        );
    }
    if breakpoint_count(destination_dfr0) != request.breakpoints.implemented_count
        || watchpoint_count(destination_dfr0) != request.watchpoints.implemented_count
    {
        return reject(
            HvfArm64ReviewedOptionalStateRestoreFamily::Destination,
            HvfArm64ReviewedOptionalStateRestoreStage::Validation,
            None,
            0,
            HvfArm64ReviewedOptionalStateRestoreRejection::DebugRegisterCount,
        );
    }

    let (breakpoint_values, breakpoint_controls) = materialize_debug(
        access,
        &request.breakpoints,
        true,
        HvfArm64ReviewedOptionalStateRestoreFamily::Breakpoint,
    )?;
    let (watchpoint_values, watchpoint_controls) = materialize_debug(
        access,
        &request.watchpoints,
        false,
        HvfArm64ReviewedOptionalStateRestoreFamily::Watchpoint,
    )?;

    let destination_pfr1 = access
        .read_system_register(HvfSystemRegister::ID_AA64PFR1_EL1)
        .map_err(map_backend(
            HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
            HvfArm64ReviewedOptionalStateRestoreStage::FeatureRead,
            None,
            0,
        ))?;
    if sme_version(destination_pfr1) != request.expected_sme_version {
        return reject(
            HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
            HvfArm64ReviewedOptionalStateRestoreStage::Validation,
            None,
            0,
            HvfArm64ReviewedOptionalStateRestoreRejection::SmeFeature,
        );
    }

    let mut materialized_system = None;
    let mut z_scratch = Vec::new();
    let mut p_scratch = Vec::new();
    let mut za_scratch = Vec::new();
    if let Some(sme) = request.sme.as_ref() {
        let destination_zfr0 = access
            .read_system_register(HvfSystemRegister::ID_AA64ZFR0_EL1)
            .map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::FeatureRead,
                Some(0),
                0,
            ))?;
        let destination_smfr0 = access
            .read_system_register(HvfSystemRegister::ID_AA64SMFR0_EL1)
            .map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::FeatureRead,
                Some(1),
                0,
            ))?;
        if destination_zfr0 != sme.identification.id_aa64zfr0_el1()
            || destination_smfr0 != sme.identification.id_aa64smfr0_el1()
        {
            return reject(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                None,
                0,
                HvfArm64ReviewedOptionalStateRestoreRejection::SmeIdentification,
            );
        }
        let maximum_svl_bytes = access.get_sme_maximum_svl_bytes().map_err(map_backend(
            HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
            HvfArm64ReviewedOptionalStateRestoreStage::FeatureRead,
            Some(2),
            0,
        ))?;
        if maximum_svl_bytes != sme.maximum_svl_bytes {
            return reject(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                None,
                0,
                HvfArm64ReviewedOptionalStateRestoreRejection::SmeMaximumSvl,
            );
        }
        let destination_pstate = access.get_sme_pstate().map_err(map_backend(
            HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
            HvfArm64ReviewedOptionalStateRestoreStage::ValueDefaultRead,
            Some(0),
            0,
        ))?;
        if destination_pstate != (false, false) {
            return reject(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                None,
                0,
                HvfArm64ReviewedOptionalStateRestoreRejection::FreshSmePstate,
            );
        }
        let mut resolved_system = [0; 3];
        for (index, ((register, request_value), resolved)) in [
            HvfSystemRegister::SMCR_EL1,
            HvfSystemRegister::SMPRI_EL1,
            HvfSystemRegister::TPIDR2_EL0,
        ]
        .into_iter()
        .zip(sme.system_registers)
        .zip(&mut resolved_system)
        .enumerate()
        {
            *resolved = match request_value {
                HvfArm64OptionalStateValue::Explicit(value) => value,
                HvfArm64OptionalStateValue::DestinationDefault => {
                    access.read_system_register(register).map_err(map_backend(
                        HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                        HvfArm64ReviewedOptionalStateRestoreStage::ValueDefaultRead,
                        Some(index as u8 + 1),
                        0,
                    ))?
                }
            };
        }
        materialized_system = Some(resolved_system);
        let target = sme.target_pstate();
        if target.streaming_sve_mode_enabled() {
            z_scratch = allocate_zeroed(sme.maximum_svl_bytes).map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                None,
                0,
            ))?;
            p_scratch = allocate_zeroed(sme.maximum_svl_bytes / 8).map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                None,
                0,
            ))?;
        }
        if target.za_storage_enabled() {
            let za_size = sme
                .maximum_svl_bytes
                .checked_mul(sme.maximum_svl_bytes)
                .ok_or_else(|| {
                    HvfArm64ReviewedOptionalStateRestoreError::rejection(
                        HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                        HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                        None,
                        0,
                        HvfArm64ReviewedOptionalStateRestoreRejection::Request,
                    )
                })?;
            za_scratch = allocate_zeroed(za_size).map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                None,
                0,
            ))?;
        }
    }

    let mut completed_writes = 0;
    restore_debug(
        access,
        &request.breakpoints,
        &breakpoint_values,
        &breakpoint_controls,
        true,
        HvfArm64ReviewedOptionalStateRestoreFamily::Breakpoint,
        &mut completed_writes,
    )?;
    restore_debug(
        access,
        &request.watchpoints,
        &watchpoint_values,
        &watchpoint_controls,
        false,
        HvfArm64ReviewedOptionalStateRestoreFamily::Watchpoint,
        &mut completed_writes,
    )?;

    if let (Some(sme), Some(system_registers)) = (request.sme.as_ref(), materialized_system) {
        for (index, (register, value)) in [
            HvfSystemRegister::SMCR_EL1,
            HvfSystemRegister::SMPRI_EL1,
            HvfSystemRegister::TPIDR2_EL0,
        ]
        .into_iter()
        .zip(system_registers)
        .enumerate()
        {
            write_system(
                access,
                register,
                value,
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::SystemWrite,
                index as u8,
                &mut completed_writes,
            )?;
        }
        let target_pstate = sme.target_pstate();
        access
            .set_sme_pstate(
                target_pstate.streaming_sve_mode_enabled(),
                target_pstate.za_storage_enabled(),
            )
            .map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::PstateWrite,
                None,
                completed_writes,
            ))?;
        completed_writes += 1;

        if let Some(z_registers) = sme.z_registers.as_deref() {
            for (index, value) in z_registers.iter().enumerate() {
                let bytes = match value {
                    HvfArm64OptionalStateValue::Explicit(bytes) => bytes.as_ref(),
                    HvfArm64OptionalStateValue::DestinationDefault => {
                        z_scratch.fill(0);
                        access
                            .get_sme_z_register(index as u32, &mut z_scratch)
                            .map_err(map_backend(
                                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                                HvfArm64ReviewedOptionalStateRestoreStage::ConditionalDefaultRead,
                                Some(index as u8),
                                completed_writes,
                            ))?;
                        if z_scratch.iter().any(|byte| *byte != 0) {
                            return reject(
                                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                                HvfArm64ReviewedOptionalStateRestoreStage::ConditionalDefaultRead,
                                Some(index as u8),
                                completed_writes,
                                HvfArm64ReviewedOptionalStateRestoreRejection::SmeTransitionDefault,
                            );
                        }
                        &z_scratch
                    }
                };
                access
                    .set_sme_z_register(index as u32, bytes)
                    .map_err(map_backend(
                        HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                        HvfArm64ReviewedOptionalStateRestoreStage::ConditionalWrite,
                        Some(index as u8),
                        completed_writes,
                    ))?;
                completed_writes += 1;
            }
        }
        if let Some(p_registers) = sme.p_registers.as_deref() {
            for (index, value) in p_registers.iter().enumerate() {
                let operation_index = (SME_Z_REGISTER_COUNT + index) as u8;
                let bytes = match value {
                    HvfArm64OptionalStateValue::Explicit(bytes) => bytes.as_ref(),
                    HvfArm64OptionalStateValue::DestinationDefault => {
                        p_scratch.fill(0);
                        access
                            .get_sme_p_register(index as u32, &mut p_scratch)
                            .map_err(map_backend(
                                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                                HvfArm64ReviewedOptionalStateRestoreStage::ConditionalDefaultRead,
                                Some(operation_index),
                                completed_writes,
                            ))?;
                        if p_scratch.iter().any(|byte| *byte != 0) {
                            return reject(
                                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                                HvfArm64ReviewedOptionalStateRestoreStage::ConditionalDefaultRead,
                                Some(operation_index),
                                completed_writes,
                                HvfArm64ReviewedOptionalStateRestoreRejection::SmeTransitionDefault,
                            );
                        }
                        &p_scratch
                    }
                };
                access
                    .set_sme_p_register(index as u32, bytes)
                    .map_err(map_backend(
                        HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                        HvfArm64ReviewedOptionalStateRestoreStage::ConditionalWrite,
                        Some(operation_index),
                        completed_writes,
                    ))?;
                completed_writes += 1;
            }
        }
        if let Some(za_register) = sme.za_register.as_ref() {
            let bytes = match za_register {
                HvfArm64OptionalStateValue::Explicit(bytes) => bytes.as_ref(),
                HvfArm64OptionalStateValue::DestinationDefault => {
                    za_scratch.fill(0);
                    access
                        .get_sme_za_register(&mut za_scratch)
                        .map_err(map_backend(
                            HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                            HvfArm64ReviewedOptionalStateRestoreStage::ConditionalDefaultRead,
                            Some(48),
                            completed_writes,
                        ))?;
                    &za_scratch
                }
            };
            access.set_sme_za_register(bytes).map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::ConditionalWrite,
                Some(48),
                completed_writes,
            ))?;
            completed_writes += 1;
        }
        if let Some(zt0_register) = sme.zt0_register.as_ref() {
            let value = match zt0_register {
                HvfArm64OptionalStateValue::Explicit(value) => *value,
                HvfArm64OptionalStateValue::DestinationDefault => {
                    access.get_sme_zt0_register().map_err(map_backend(
                        HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                        HvfArm64ReviewedOptionalStateRestoreStage::ConditionalDefaultRead,
                        Some(49),
                        completed_writes,
                    ))?
                }
            };
            access.set_sme_zt0_register(value).map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
                HvfArm64ReviewedOptionalStateRestoreStage::ConditionalWrite,
                Some(49),
                completed_writes,
            ))?;
            completed_writes += 1;
        }
    }

    for (index, value) in request.simd_fp.q_registers().iter().enumerate() {
        let register = HvfSimdFpRegister::q(index as u8).ok_or_else(|| {
            HvfArm64ReviewedOptionalStateRestoreError::rejection(
                HvfArm64ReviewedOptionalStateRestoreFamily::SimdFp,
                HvfArm64ReviewedOptionalStateRestoreStage::Validation,
                Some(index as u8),
                completed_writes,
                HvfArm64ReviewedOptionalStateRestoreRejection::Request,
            )
        })?;
        access
            .write_simd_fp_register(register, *value)
            .map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::SimdFp,
                HvfArm64ReviewedOptionalStateRestoreStage::FinalWrite,
                Some(index as u8),
                completed_writes,
            ))?;
        completed_writes += 1;
    }
    for (index, (register, value)) in [
        (HvfRegister::FPCR, request.simd_fp.fpcr()),
        (HvfRegister::FPSR, request.simd_fp.fpsr()),
    ]
    .into_iter()
    .enumerate()
    {
        access
            .write_scalar_register(register, value)
            .map_err(map_backend(
                HvfArm64ReviewedOptionalStateRestoreFamily::SimdFp,
                HvfArm64ReviewedOptionalStateRestoreStage::FinalWrite,
                Some((SME_Z_REGISTER_COUNT + index) as u8),
                completed_writes,
            ))?;
        completed_writes += 1;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::error::Error;

    use super::{
        HvfArm64DebugRegisterRestoreState, HvfArm64OptionalStateValue,
        HvfArm64ReviewedOptionalStateAccess, HvfArm64ReviewedOptionalStateBuildError,
        HvfArm64ReviewedOptionalStateRestore, HvfArm64ReviewedOptionalStateRestoreFamily,
        HvfArm64ReviewedOptionalStateRestoreRejection, HvfArm64ReviewedOptionalStateRestoreStage,
        HvfArm64SmeRestoreState, HvfArm64SmeRestoreStateInput, SME_Z_REGISTER_COUNT,
        restore_arm64_reviewed_optional_state_with,
    };
    use crate::vcpu::{
        HvfArm64VcpuSimdFpState, HvfArm64VcpuSmePstate,
        HvfArm64VcpuSveSmeIdentificationRegisterState, HvfRegister, HvfSimdFpRegister,
        HvfSystemRegister,
    };
    use bangbang_runtime::BackendError;

    const NO_SME_PFR1: u64 = 0xf << 24;
    const SME2_PFR1: u64 = 1 << 24;
    const TEST_ZFR0: u64 = 0x1020_3040_5060_7080;
    const TEST_SMFR0: u64 = 0x8877_6655_4433_2211;
    const TEST_MAX_SVL: usize = 16;
    const SENSITIVE_BACKEND_MESSAGE: &str = "sensitive backend detail";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Call {
        ReadSystem(HvfSystemRegister),
        WriteSystem(HvfSystemRegister),
        GetPstate,
        SetPstate,
        GetMaximumSvl,
        GetZ(u32),
        SetZ(u32),
        GetP(u32),
        SetP(u32),
        GetZa,
        SetZa,
        GetZt0,
        SetZt0,
        WriteQ(HvfSimdFpRegister),
        WriteScalar(HvfRegister),
    }

    impl Call {
        const fn is_write(self) -> bool {
            matches!(
                self,
                Self::WriteSystem(_)
                    | Self::SetPstate
                    | Self::SetZ(_)
                    | Self::SetP(_)
                    | Self::SetZa
                    | Self::SetZt0
                    | Self::WriteQ(_)
                    | Self::WriteScalar(_)
            )
        }
    }

    struct RecordingAccess {
        calls: Vec<Call>,
        fail_at: Option<usize>,
        system_values: HashMap<HvfSystemRegister, u64>,
        pstate: (bool, bool),
        maximum_svl: usize,
        nonzero_z_default: bool,
        nonzero_p_default: bool,
        system_writes: Vec<(HvfSystemRegister, u64)>,
        pstate_write: Option<(bool, bool)>,
        z_writes: Vec<(u32, Vec<u8>)>,
        p_writes: Vec<(u32, Vec<u8>)>,
        za_write: Option<Vec<u8>>,
        zt0_write: Option<[u8; 64]>,
        simd_writes: Vec<(HvfSimdFpRegister, [u8; 16])>,
        scalar_writes: Vec<(HvfRegister, u64)>,
    }

    impl RecordingAccess {
        fn no_sme() -> Self {
            let mut system_values = HashMap::new();
            system_values.insert(HvfSystemRegister::ID_AA64DFR0_EL1, 0);
            system_values.insert(HvfSystemRegister::ID_AA64PFR1_EL1, NO_SME_PFR1);
            Self {
                calls: Vec::new(),
                fail_at: None,
                system_values,
                pstate: (false, false),
                maximum_svl: TEST_MAX_SVL,
                nonzero_z_default: false,
                nonzero_p_default: false,
                system_writes: Vec::new(),
                pstate_write: None,
                z_writes: Vec::new(),
                p_writes: Vec::new(),
                za_write: None,
                zt0_write: None,
                simd_writes: Vec::new(),
                scalar_writes: Vec::new(),
            }
        }

        fn sme2() -> Self {
            let mut access = Self::no_sme();
            access
                .system_values
                .insert(HvfSystemRegister::ID_AA64PFR1_EL1, SME2_PFR1);
            access
                .system_values
                .insert(HvfSystemRegister::ID_AA64ZFR0_EL1, TEST_ZFR0);
            access
                .system_values
                .insert(HvfSystemRegister::ID_AA64SMFR0_EL1, TEST_SMFR0);
            access
                .system_values
                .insert(HvfSystemRegister::SMCR_EL1, 0x1111);
            access
                .system_values
                .insert(HvfSystemRegister::SMPRI_EL1, 0x2222);
            access
                .system_values
                .insert(HvfSystemRegister::TPIDR2_EL0, 0x3333);
            access
        }

        fn record(&mut self, call: Call) -> Result<(), BackendError> {
            let index = self.calls.len();
            self.calls.push(call);
            if self.fail_at == Some(index) {
                Err(BackendError::InvalidState(SENSITIVE_BACKEND_MESSAGE))
            } else {
                Ok(())
            }
        }
    }

    impl HvfArm64ReviewedOptionalStateAccess for RecordingAccess {
        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.record(Call::ReadSystem(register))?;
            Ok(*self.system_values.get(&register).unwrap_or(&0))
        }

        fn write_system_register(
            &mut self,
            register: HvfSystemRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            self.record(Call::WriteSystem(register))?;
            self.system_writes.push((register, value));
            Ok(())
        }

        fn get_sme_pstate(&mut self) -> Result<(bool, bool), BackendError> {
            self.record(Call::GetPstate)?;
            Ok(self.pstate)
        }

        fn set_sme_pstate(
            &mut self,
            streaming_sve_mode_enabled: bool,
            za_storage_enabled: bool,
        ) -> Result<(), BackendError> {
            self.record(Call::SetPstate)?;
            self.pstate_write = Some((streaming_sve_mode_enabled, za_storage_enabled));
            Ok(())
        }

        fn get_sme_maximum_svl_bytes(&mut self) -> Result<usize, BackendError> {
            self.record(Call::GetMaximumSvl)?;
            Ok(self.maximum_svl)
        }

        fn get_sme_z_register(
            &mut self,
            register: u32,
            value: &mut [u8],
        ) -> Result<(), BackendError> {
            self.record(Call::GetZ(register))?;
            value.fill(u8::from(self.nonzero_z_default));
            Ok(())
        }

        fn set_sme_z_register(&mut self, register: u32, value: &[u8]) -> Result<(), BackendError> {
            self.record(Call::SetZ(register))?;
            self.z_writes.push((register, value.to_vec()));
            Ok(())
        }

        fn get_sme_p_register(
            &mut self,
            register: u32,
            value: &mut [u8],
        ) -> Result<(), BackendError> {
            self.record(Call::GetP(register))?;
            value.fill(u8::from(self.nonzero_p_default));
            Ok(())
        }

        fn set_sme_p_register(&mut self, register: u32, value: &[u8]) -> Result<(), BackendError> {
            self.record(Call::SetP(register))?;
            self.p_writes.push((register, value.to_vec()));
            Ok(())
        }

        fn get_sme_za_register(&mut self, value: &mut [u8]) -> Result<(), BackendError> {
            self.record(Call::GetZa)?;
            value.fill(0x5a);
            Ok(())
        }

        fn set_sme_za_register(&mut self, value: &[u8]) -> Result<(), BackendError> {
            self.record(Call::SetZa)?;
            self.za_write = Some(value.to_vec());
            Ok(())
        }

        fn get_sme_zt0_register(&mut self) -> Result<[u8; 64], BackendError> {
            self.record(Call::GetZt0)?;
            Ok([0xa5; 64])
        }

        fn set_sme_zt0_register(&mut self, value: [u8; 64]) -> Result<(), BackendError> {
            self.record(Call::SetZt0)?;
            self.zt0_write = Some(value);
            Ok(())
        }

        fn write_simd_fp_register(
            &mut self,
            register: HvfSimdFpRegister,
            value: [u8; 16],
        ) -> Result<(), BackendError> {
            self.record(Call::WriteQ(register))?;
            self.simd_writes.push((register, value));
            Ok(())
        }

        fn write_scalar_register(
            &mut self,
            register: HvfRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            self.record(Call::WriteScalar(register))?;
            self.scalar_writes.push((register, value));
            Ok(())
        }
    }

    fn expected_failure_location(
        call: Call,
        breakpoint_control_writes: &mut usize,
        watchpoint_control_writes: &mut usize,
    ) -> (
        HvfArm64ReviewedOptionalStateRestoreFamily,
        HvfArm64ReviewedOptionalStateRestoreStage,
        Option<u8>,
    ) {
        use HvfArm64ReviewedOptionalStateRestoreFamily as Family;
        use HvfArm64ReviewedOptionalStateRestoreStage as Stage;

        match call {
            Call::ReadSystem(register) if register == HvfSystemRegister::ID_AA64DFR0_EL1 => {
                (Family::Destination, Stage::FeatureRead, None)
            }
            Call::ReadSystem(register) if register == HvfSystemRegister::ID_AA64PFR1_EL1 => {
                (Family::Sme, Stage::FeatureRead, None)
            }
            Call::ReadSystem(register) if register == HvfSystemRegister::ID_AA64ZFR0_EL1 => {
                (Family::Sme, Stage::FeatureRead, Some(0))
            }
            Call::ReadSystem(register) if register == HvfSystemRegister::ID_AA64SMFR0_EL1 => {
                (Family::Sme, Stage::FeatureRead, Some(1))
            }
            Call::ReadSystem(register)
                if register
                    == HvfSystemRegister::debug_breakpoint_control(0)
                        .expect("breakpoint control should exist") =>
            {
                (Family::Breakpoint, Stage::ControlDefaultRead, Some(0))
            }
            Call::ReadSystem(register)
                if register
                    == HvfSystemRegister::debug_breakpoint_value(0)
                        .expect("breakpoint value should exist") =>
            {
                (Family::Breakpoint, Stage::ValueDefaultRead, Some(0))
            }
            Call::ReadSystem(register)
                if register
                    == HvfSystemRegister::debug_watchpoint_control(0)
                        .expect("watchpoint control should exist") =>
            {
                (Family::Watchpoint, Stage::ControlDefaultRead, Some(0))
            }
            Call::ReadSystem(register)
                if register
                    == HvfSystemRegister::debug_watchpoint_value(0)
                        .expect("watchpoint value should exist") =>
            {
                (Family::Watchpoint, Stage::ValueDefaultRead, Some(0))
            }
            Call::ReadSystem(HvfSystemRegister::SMCR_EL1) => {
                (Family::Sme, Stage::ValueDefaultRead, Some(1))
            }
            Call::ReadSystem(HvfSystemRegister::SMPRI_EL1) => {
                (Family::Sme, Stage::ValueDefaultRead, Some(2))
            }
            Call::ReadSystem(HvfSystemRegister::TPIDR2_EL0) => {
                (Family::Sme, Stage::ValueDefaultRead, Some(3))
            }
            Call::WriteSystem(register)
                if register
                    == HvfSystemRegister::debug_breakpoint_control(0)
                        .expect("breakpoint control should exist") =>
            {
                let stage = if *breakpoint_control_writes == 0 {
                    Stage::DisableControl
                } else {
                    Stage::FinalControl
                };
                *breakpoint_control_writes += 1;
                (Family::Breakpoint, stage, Some(0))
            }
            Call::WriteSystem(register)
                if register
                    == HvfSystemRegister::debug_breakpoint_value(0)
                        .expect("breakpoint value should exist") =>
            {
                (Family::Breakpoint, Stage::ValueWrite, Some(0))
            }
            Call::WriteSystem(register)
                if register
                    == HvfSystemRegister::debug_watchpoint_control(0)
                        .expect("watchpoint control should exist") =>
            {
                let stage = if *watchpoint_control_writes == 0 {
                    Stage::DisableControl
                } else {
                    Stage::FinalControl
                };
                *watchpoint_control_writes += 1;
                (Family::Watchpoint, stage, Some(0))
            }
            Call::WriteSystem(register)
                if register
                    == HvfSystemRegister::debug_watchpoint_value(0)
                        .expect("watchpoint value should exist") =>
            {
                (Family::Watchpoint, Stage::ValueWrite, Some(0))
            }
            Call::WriteSystem(HvfSystemRegister::SMCR_EL1) => {
                (Family::Sme, Stage::SystemWrite, Some(0))
            }
            Call::WriteSystem(HvfSystemRegister::SMPRI_EL1) => {
                (Family::Sme, Stage::SystemWrite, Some(1))
            }
            Call::WriteSystem(HvfSystemRegister::TPIDR2_EL0) => {
                (Family::Sme, Stage::SystemWrite, Some(2))
            }
            Call::GetPstate => (Family::Sme, Stage::ValueDefaultRead, Some(0)),
            Call::SetPstate => (Family::Sme, Stage::PstateWrite, None),
            Call::GetMaximumSvl => (Family::Sme, Stage::FeatureRead, Some(2)),
            Call::GetZ(index) => (
                Family::Sme,
                Stage::ConditionalDefaultRead,
                Some(index as u8),
            ),
            Call::SetZ(index) => (Family::Sme, Stage::ConditionalWrite, Some(index as u8)),
            Call::GetP(index) => (
                Family::Sme,
                Stage::ConditionalDefaultRead,
                Some((SME_Z_REGISTER_COUNT + index as usize) as u8),
            ),
            Call::SetP(index) => (
                Family::Sme,
                Stage::ConditionalWrite,
                Some((SME_Z_REGISTER_COUNT + index as usize) as u8),
            ),
            Call::GetZa => (Family::Sme, Stage::ConditionalDefaultRead, Some(48)),
            Call::SetZa => (Family::Sme, Stage::ConditionalWrite, Some(48)),
            Call::GetZt0 => (Family::Sme, Stage::ConditionalDefaultRead, Some(49)),
            Call::SetZt0 => (Family::Sme, Stage::ConditionalWrite, Some(49)),
            Call::WriteQ(register) => (
                Family::SimdFp,
                Stage::FinalWrite,
                Some(
                    u8::try_from(register.raw())
                        .expect("test Q-register identifier should fit in u8"),
                ),
            ),
            Call::WriteScalar(HvfRegister::FPCR) => (Family::SimdFp, Stage::FinalWrite, Some(32)),
            Call::WriteScalar(HvfRegister::FPSR) => (Family::SimdFp, Stage::FinalWrite, Some(33)),
            _ => panic!("unexpected reviewed optional-state test call: {call:?}"),
        }
    }

    fn debug_state(
        value: HvfArm64OptionalStateValue<u64>,
        control: HvfArm64OptionalStateValue<u64>,
    ) -> HvfArm64DebugRegisterRestoreState {
        let mut values = [HvfArm64OptionalStateValue::DestinationDefault; 16];
        let mut controls = [HvfArm64OptionalStateValue::DestinationDefault; 16];
        values[0] = value;
        controls[0] = control;
        HvfArm64DebugRegisterRestoreState::try_new(1, values, controls)
            .expect("one debug slot should be valid")
    }

    fn simd(value: u8) -> HvfArm64VcpuSimdFpState {
        HvfArm64VcpuSimdFpState::new([[value; 16]; 32], 0x1234, 0x5678)
    }

    fn sme_input(
        version: u8,
        maximum_svl_bytes: usize,
        streaming_sve_mode_enabled: bool,
        za_storage_enabled: bool,
    ) -> HvfArm64SmeRestoreStateInput {
        HvfArm64SmeRestoreStateInput::new(
            version,
            HvfArm64VcpuSveSmeIdentificationRegisterState::new(TEST_ZFR0, TEST_SMFR0),
            maximum_svl_bytes,
            HvfArm64OptionalStateValue::Explicit(HvfArm64VcpuSmePstate::new(
                streaming_sve_mode_enabled,
                za_storage_enabled,
            )),
            [HvfArm64OptionalStateValue::DestinationDefault; 3],
        )
    }

    fn assert_sme_build_error(
        input: HvfArm64SmeRestoreStateInput,
        simd_fp: &HvfArm64VcpuSimdFpState,
        expected: HvfArm64ReviewedOptionalStateBuildError,
    ) {
        assert_eq!(
            HvfArm64SmeRestoreState::try_new(input, simd_fp),
            Err(expected)
        );
    }

    fn no_sme_request() -> HvfArm64ReviewedOptionalStateRestore {
        HvfArm64ReviewedOptionalStateRestore::try_new(
            0,
            None,
            debug_state(
                HvfArm64OptionalStateValue::Explicit(0x1111),
                HvfArm64OptionalStateValue::Explicit(1),
            ),
            debug_state(
                HvfArm64OptionalStateValue::Explicit(0x2222),
                HvfArm64OptionalStateValue::Explicit(1),
            ),
            None,
            simd(0),
        )
        .expect("no-SME request should be valid")
    }

    fn sme2_default_request() -> HvfArm64ReviewedOptionalStateRestore {
        let simd_fp = simd(0);
        let sme = HvfArm64SmeRestoreState::try_new(
            HvfArm64SmeRestoreStateInput::new(
                1,
                HvfArm64VcpuSveSmeIdentificationRegisterState::new(TEST_ZFR0, TEST_SMFR0),
                TEST_MAX_SVL,
                HvfArm64OptionalStateValue::Explicit(HvfArm64VcpuSmePstate::new(true, true)),
                [HvfArm64OptionalStateValue::DestinationDefault; 3],
            )
            .with_streaming_registers(
                vec![HvfArm64OptionalStateValue::DestinationDefault; 32],
                vec![HvfArm64OptionalStateValue::DestinationDefault; 16],
            )
            .with_za_register(
                HvfArm64OptionalStateValue::DestinationDefault,
                Some(HvfArm64OptionalStateValue::DestinationDefault),
            ),
            &simd_fp,
        )
        .expect("SME2 default request should be valid");
        HvfArm64ReviewedOptionalStateRestore::try_new(
            0,
            Some(1),
            debug_state(
                HvfArm64OptionalStateValue::DestinationDefault,
                HvfArm64OptionalStateValue::DestinationDefault,
            ),
            debug_state(
                HvfArm64OptionalStateValue::DestinationDefault,
                HvfArm64OptionalStateValue::DestinationDefault,
            ),
            Some(sme),
            simd_fp,
        )
        .expect("complete SME2 request should be valid")
    }

    fn assert_prewrite_rejection(
        request: &HvfArm64ReviewedOptionalStateRestore,
        access: &mut RecordingAccess,
        family: HvfArm64ReviewedOptionalStateRestoreFamily,
        rejection: HvfArm64ReviewedOptionalStateRestoreRejection,
    ) {
        let error = restore_arm64_reviewed_optional_state_with(access, request)
            .expect_err("destination mismatch should fail");
        assert_eq!(error.family(), family);
        assert_eq!(
            error.stage(),
            HvfArm64ReviewedOptionalStateRestoreStage::Validation
        );
        assert_eq!(error.completed_writes(), 0);
        assert_eq!(
            error
                .source()
                .and_then(|source| {
                    source.downcast_ref::<HvfArm64ReviewedOptionalStateRestoreRejection>()
                })
                .copied(),
            Some(rejection)
        );
        assert!(!access.calls.iter().copied().any(Call::is_write));
    }

    #[test]
    fn restores_debug_values_only_after_disabling_every_control() {
        let request = no_sme_request();
        let mut access = RecordingAccess::no_sme();

        restore_arm64_reviewed_optional_state_with(&mut access, &request)
            .expect("debug restore should succeed");

        let debug_writes: Vec<_> = access
            .calls
            .iter()
            .copied()
            .filter(|call| matches!(call, Call::WriteSystem(_)))
            .collect();
        assert_eq!(
            debug_writes,
            vec![
                Call::WriteSystem(HvfSystemRegister::debug_breakpoint_control(0).unwrap()),
                Call::WriteSystem(HvfSystemRegister::debug_breakpoint_value(0).unwrap()),
                Call::WriteSystem(HvfSystemRegister::debug_breakpoint_control(0).unwrap()),
                Call::WriteSystem(HvfSystemRegister::debug_watchpoint_control(0).unwrap()),
                Call::WriteSystem(HvfSystemRegister::debug_watchpoint_value(0).unwrap()),
                Call::WriteSystem(HvfSystemRegister::debug_watchpoint_control(0).unwrap()),
            ]
        );
        assert_eq!(
            access.system_writes,
            vec![
                (HvfSystemRegister::debug_breakpoint_control(0).unwrap(), 0,),
                (
                    HvfSystemRegister::debug_breakpoint_value(0).unwrap(),
                    0x1111,
                ),
                (HvfSystemRegister::debug_breakpoint_control(0).unwrap(), 1,),
                (HvfSystemRegister::debug_watchpoint_control(0).unwrap(), 0,),
                (
                    HvfSystemRegister::debug_watchpoint_value(0).unwrap(),
                    0x2222,
                ),
                (HvfSystemRegister::debug_watchpoint_control(0).unwrap(), 1,),
            ]
        );
    }

    #[test]
    fn restores_authoritative_simd_state_when_sme_is_absent() {
        let request = no_sme_request();
        let mut access = RecordingAccess::no_sme();

        restore_arm64_reviewed_optional_state_with(&mut access, &request)
            .expect("non-SME restore should succeed");

        let simd_writes: Vec<_> = access
            .calls
            .iter()
            .copied()
            .filter(|call| matches!(call, Call::WriteQ(_) | Call::WriteScalar(_)))
            .collect();
        assert_eq!(simd_writes.len(), 34);
        assert_eq!(
            simd_writes.first(),
            Some(&Call::WriteQ(
                HvfSimdFpRegister::q(0).expect("Q0 should exist")
            ))
        );
        assert_eq!(
            simd_writes.last(),
            Some(&Call::WriteScalar(HvfRegister::FPSR))
        );
    }

    #[test]
    fn rejects_enabled_fresh_debug_control_before_every_write() {
        let request = no_sme_request();
        let mut access = RecordingAccess::no_sme();
        access
            .system_values
            .insert(HvfSystemRegister::debug_breakpoint_control(0).unwrap(), 1);

        let error = restore_arm64_reviewed_optional_state_with(&mut access, &request)
            .expect_err("enabled fresh control should fail");

        assert_eq!(
            error.stage(),
            HvfArm64ReviewedOptionalStateRestoreStage::Validation
        );
        assert_eq!(error.completed_writes(), 0);
        assert!(!access.calls.iter().copied().any(Call::is_write));
    }

    #[test]
    fn rejects_every_destination_compatibility_mismatch_before_writes() {
        let no_sme = no_sme_request();
        let mut wrong_dfr = RecordingAccess::no_sme();
        wrong_dfr
            .system_values
            .insert(HvfSystemRegister::ID_AA64DFR0_EL1, 1);
        assert_prewrite_rejection(
            &no_sme,
            &mut wrong_dfr,
            HvfArm64ReviewedOptionalStateRestoreFamily::Destination,
            HvfArm64ReviewedOptionalStateRestoreRejection::DebugIdentification,
        );

        let mut wrong_sme_feature = RecordingAccess::no_sme();
        wrong_sme_feature
            .system_values
            .insert(HvfSystemRegister::ID_AA64PFR1_EL1, 0);
        assert_prewrite_rejection(
            &no_sme,
            &mut wrong_sme_feature,
            HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
            HvfArm64ReviewedOptionalStateRestoreRejection::SmeFeature,
        );

        let sme2 = sme2_default_request();
        let mut wrong_identification = RecordingAccess::sme2();
        wrong_identification
            .system_values
            .insert(HvfSystemRegister::ID_AA64ZFR0_EL1, TEST_ZFR0 ^ 1);
        assert_prewrite_rejection(
            &sme2,
            &mut wrong_identification,
            HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
            HvfArm64ReviewedOptionalStateRestoreRejection::SmeIdentification,
        );

        let mut wrong_maximum_svl = RecordingAccess::sme2();
        wrong_maximum_svl.maximum_svl = TEST_MAX_SVL * 2;
        assert_prewrite_rejection(
            &sme2,
            &mut wrong_maximum_svl,
            HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
            HvfArm64ReviewedOptionalStateRestoreRejection::SmeMaximumSvl,
        );

        let mut active_pstate = RecordingAccess::sme2();
        active_pstate.pstate = (true, false);
        assert_prewrite_rejection(
            &sme2,
            &mut active_pstate,
            HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
            HvfArm64ReviewedOptionalStateRestoreRejection::FreshSmePstate,
        );
    }

    #[test]
    fn restores_sme_contents_before_authoritative_simd_state() {
        let request = sme2_default_request();
        let mut access = RecordingAccess::sme2();

        restore_arm64_reviewed_optional_state_with(&mut access, &request)
            .expect("SME2 restore should succeed");

        let last_zt0 = access
            .calls
            .iter()
            .rposition(|call| *call == Call::SetZt0)
            .unwrap();
        let first_q = access
            .calls
            .iter()
            .position(|call| matches!(call, Call::WriteQ(_)))
            .unwrap();
        let last_scalar = access
            .calls
            .iter()
            .rposition(|call| matches!(call, Call::WriteScalar(_)))
            .unwrap();
        assert!(last_zt0 < first_q);
        assert!(first_q < last_scalar);
        assert_eq!(
            access.calls[last_scalar],
            Call::WriteScalar(HvfRegister::FPSR)
        );
    }

    #[test]
    fn every_backend_operation_failure_reports_the_exact_completed_write_prefix() {
        let request = sme2_default_request();
        let mut baseline = RecordingAccess::sme2();
        restore_arm64_reviewed_optional_state_with(&mut baseline, &request)
            .expect("baseline restore should succeed");
        let calls = baseline.calls;
        let mut breakpoint_control_writes = 0;
        let mut watchpoint_control_writes = 0;

        for failure_index in 0..calls.len() {
            let (expected_family, expected_stage, expected_index) = expected_failure_location(
                calls[failure_index],
                &mut breakpoint_control_writes,
                &mut watchpoint_control_writes,
            );
            let mut access = RecordingAccess::sme2();
            access.fail_at = Some(failure_index);
            let error = restore_arm64_reviewed_optional_state_with(&mut access, &request)
                .expect_err("injected operation should fail");
            let expected_completed = calls[..failure_index]
                .iter()
                .copied()
                .filter(|call| call.is_write())
                .count();
            assert_eq!(
                error.completed_writes(),
                expected_completed,
                "wrong prefix for call {failure_index}: {:?}",
                calls[failure_index]
            );
            assert_eq!(
                (error.family(), error.stage(), error.index()),
                (expected_family, expected_stage, expected_index),
                "wrong location for call {failure_index}: {:?}",
                calls[failure_index]
            );
            assert_eq!(
                error
                    .source()
                    .and_then(|source| source.downcast_ref::<BackendError>()),
                Some(&BackendError::InvalidState(SENSITIVE_BACKEND_MESSAGE))
            );
            assert!(!format!("{error:?}").contains(SENSITIVE_BACKEND_MESSAGE));
            assert!(!error.to_string().contains(SENSITIVE_BACKEND_MESSAGE));
        }
    }

    #[test]
    fn rejects_nonzero_documented_z_default_after_pstate_with_partial_prefix() {
        let request = sme2_default_request();
        let mut access = RecordingAccess::sme2();
        access.nonzero_z_default = true;

        let error = restore_arm64_reviewed_optional_state_with(&mut access, &request)
            .expect_err("nonzero transition default should fail");

        assert_eq!(
            error.stage(),
            HvfArm64ReviewedOptionalStateRestoreStage::ConditionalDefaultRead
        );
        assert!(matches!(
            error.source().unwrap().to_string().as_str(),
            "reviewed optional arm64 destination rejected: SME transition default"
        ));
        assert!(error.completed_writes() > 0);
        assert!(
            !access
                .calls
                .iter()
                .any(|call| matches!(call, Call::SetZ(_)))
        );
    }

    #[test]
    fn rejects_nonzero_documented_p_default_after_z_prefix() {
        let request = sme2_default_request();
        let mut access = RecordingAccess::sme2();
        access.nonzero_p_default = true;

        let error = restore_arm64_reviewed_optional_state_with(&mut access, &request)
            .expect_err("nonzero predicate transition default should fail");

        assert_eq!(
            (
                error.stage(),
                error.index(),
                error
                    .source()
                    .and_then(|source| {
                        source.downcast_ref::<HvfArm64ReviewedOptionalStateRestoreRejection>()
                    })
                    .copied(),
            ),
            (
                HvfArm64ReviewedOptionalStateRestoreStage::ConditionalDefaultRead,
                Some(32),
                Some(HvfArm64ReviewedOptionalStateRestoreRejection::SmeTransitionDefault),
            )
        );
        assert!(error.completed_writes() > 0);
        assert!(
            !access
                .calls
                .iter()
                .any(|call| matches!(call, Call::SetP(_)))
        );
    }

    #[test]
    fn checked_sme_shape_rejects_wrong_width_and_q_z_alias() {
        let simd_fp = simd(0);
        let wrong_width =
            vec![
                HvfArm64OptionalStateValue::Explicit(vec![0; TEST_MAX_SVL - 1].into_boxed_slice());
                32
            ];
        assert_eq!(
            HvfArm64SmeRestoreState::try_new(
                HvfArm64SmeRestoreStateInput::new(
                    1,
                    HvfArm64VcpuSveSmeIdentificationRegisterState::new(TEST_ZFR0, TEST_SMFR0),
                    TEST_MAX_SVL,
                    HvfArm64OptionalStateValue::Explicit(HvfArm64VcpuSmePstate::new(true, false)),
                    [HvfArm64OptionalStateValue::DestinationDefault; 3],
                )
                .with_streaming_registers(
                    wrong_width,
                    vec![HvfArm64OptionalStateValue::DestinationDefault; 16],
                ),
                &simd_fp,
            ),
            Err(HvfArm64ReviewedOptionalStateBuildError::SmeRegisterWidth)
        );

        let alias_mismatch =
            vec![
                HvfArm64OptionalStateValue::Explicit(vec![1; TEST_MAX_SVL].into_boxed_slice());
                32
            ];
        assert_eq!(
            HvfArm64SmeRestoreState::try_new(
                HvfArm64SmeRestoreStateInput::new(
                    1,
                    HvfArm64VcpuSveSmeIdentificationRegisterState::new(TEST_ZFR0, TEST_SMFR0),
                    TEST_MAX_SVL,
                    HvfArm64OptionalStateValue::Explicit(HvfArm64VcpuSmePstate::new(true, false)),
                    [HvfArm64OptionalStateValue::DestinationDefault; 3],
                )
                .with_streaming_registers(
                    alias_mismatch,
                    vec![HvfArm64OptionalStateValue::DestinationDefault; 16],
                ),
                &simd_fp,
            ),
            Err(HvfArm64ReviewedOptionalStateBuildError::SmeSimdAlias)
        );
    }

    #[test]
    fn checked_restore_shape_rejects_every_static_inventory_dependency() {
        let simd_fp = simd(0);

        for count in [0, 17] {
            assert_eq!(
                HvfArm64DebugRegisterRestoreState::try_new(
                    count,
                    [HvfArm64OptionalStateValue::DestinationDefault; 16],
                    [HvfArm64OptionalStateValue::DestinationDefault; 16],
                ),
                Err(HvfArm64ReviewedOptionalStateBuildError::DebugRegisterCount)
            );
        }
        let mut values = [HvfArm64OptionalStateValue::DestinationDefault; 16];
        values[1] = HvfArm64OptionalStateValue::Explicit(1);
        assert_eq!(
            HvfArm64DebugRegisterRestoreState::try_new(
                1,
                values,
                [HvfArm64OptionalStateValue::DestinationDefault; 16],
            ),
            Err(HvfArm64ReviewedOptionalStateBuildError::DebugRegisterInventory)
        );

        assert_sme_build_error(
            sme_input(3, TEST_MAX_SVL, false, false),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeVersion,
        );
        assert_sme_build_error(
            sme_input(0, 0, false, false),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeMaximumSvl,
        );
        assert_sme_build_error(
            sme_input(0, TEST_MAX_SVL + 1, false, false),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeMaximumSvl,
        );
        assert_sme_build_error(
            sme_input(0, 8, true, false).with_streaming_registers(
                vec![HvfArm64OptionalStateValue::DestinationDefault; 32],
                vec![HvfArm64OptionalStateValue::DestinationDefault; 16],
            ),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeMaximumSvl,
        );
        assert_sme_build_error(
            sme_input(0, TEST_MAX_SVL, true, false),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory,
        );
        assert_sme_build_error(
            sme_input(0, TEST_MAX_SVL, true, false).with_streaming_registers(
                vec![HvfArm64OptionalStateValue::DestinationDefault; 31],
                vec![HvfArm64OptionalStateValue::DestinationDefault; 16],
            ),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory,
        );
        assert_sme_build_error(
            sme_input(0, TEST_MAX_SVL, true, false).with_streaming_registers(
                vec![HvfArm64OptionalStateValue::DestinationDefault; 32],
                vec![HvfArm64OptionalStateValue::Explicit(vec![0].into_boxed_slice()); 16],
            ),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeRegisterWidth,
        );
        assert_sme_build_error(
            sme_input(0, TEST_MAX_SVL, false, false).with_streaming_registers(
                vec![HvfArm64OptionalStateValue::DestinationDefault; 32],
                vec![HvfArm64OptionalStateValue::DestinationDefault; 16],
            ),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory,
        );
        assert_sme_build_error(
            sme_input(0, TEST_MAX_SVL, false, true),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory,
        );
        assert_sme_build_error(
            sme_input(0, TEST_MAX_SVL, false, true).with_za_register(
                HvfArm64OptionalStateValue::Explicit(
                    vec![0; TEST_MAX_SVL * TEST_MAX_SVL - 1].into_boxed_slice(),
                ),
                None,
            ),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeRegisterWidth,
        );
        assert_sme_build_error(
            sme_input(0, TEST_MAX_SVL, false, true).with_za_register(
                HvfArm64OptionalStateValue::DestinationDefault,
                Some(HvfArm64OptionalStateValue::DestinationDefault),
            ),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeZt0Dependency,
        );
        assert_sme_build_error(
            sme_input(1, TEST_MAX_SVL, false, true)
                .with_za_register(HvfArm64OptionalStateValue::DestinationDefault, None),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeZt0Dependency,
        );
        assert_sme_build_error(
            sme_input(1, usize::MAX & !7, false, true).with_za_register(
                HvfArm64OptionalStateValue::DestinationDefault,
                Some(HvfArm64OptionalStateValue::DestinationDefault),
            ),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeRegisterSizeOverflow,
        );
        assert_sme_build_error(
            sme_input(0, TEST_MAX_SVL, false, false)
                .with_za_register(HvfArm64OptionalStateValue::DestinationDefault, None),
            &simd_fp,
            HvfArm64ReviewedOptionalStateBuildError::SmeConditionalInventory,
        );

        let sme =
            HvfArm64SmeRestoreState::try_new(sme_input(0, TEST_MAX_SVL, false, false), &simd_fp)
                .expect("inactive SME state should be valid");
        assert_eq!(
            HvfArm64ReviewedOptionalStateRestore::try_new(
                0,
                Some(1),
                debug_state(
                    HvfArm64OptionalStateValue::DestinationDefault,
                    HvfArm64OptionalStateValue::DestinationDefault,
                ),
                debug_state(
                    HvfArm64OptionalStateValue::DestinationDefault,
                    HvfArm64OptionalStateValue::DestinationDefault,
                ),
                Some(sme),
                simd_fp,
            ),
            Err(HvfArm64ReviewedOptionalStateBuildError::SmeVersion)
        );
    }

    #[test]
    fn sparse_sme_restore_reads_only_destination_default_fields() {
        let simd_fp = simd(0);
        let z_registers = (0..32)
            .map(|index| {
                if index % 2 == 0 {
                    HvfArm64OptionalStateValue::Explicit(vec![0; TEST_MAX_SVL].into_boxed_slice())
                } else {
                    HvfArm64OptionalStateValue::DestinationDefault
                }
            })
            .collect();
        let p_registers = (0..16)
            .map(|index| {
                if index % 2 == 0 {
                    HvfArm64OptionalStateValue::Explicit(vec![index as u8; 2].into_boxed_slice())
                } else {
                    HvfArm64OptionalStateValue::DestinationDefault
                }
            })
            .collect();
        let input = HvfArm64SmeRestoreStateInput::new(
            1,
            HvfArm64VcpuSveSmeIdentificationRegisterState::new(TEST_ZFR0, TEST_SMFR0),
            TEST_MAX_SVL,
            HvfArm64OptionalStateValue::Explicit(HvfArm64VcpuSmePstate::new(true, true)),
            [
                HvfArm64OptionalStateValue::Explicit(0xaaaa),
                HvfArm64OptionalStateValue::DestinationDefault,
                HvfArm64OptionalStateValue::Explicit(0xbbbb),
            ],
        )
        .with_streaming_registers(z_registers, p_registers)
        .with_za_register(
            HvfArm64OptionalStateValue::Explicit(
                vec![0x7c; TEST_MAX_SVL * TEST_MAX_SVL].into_boxed_slice(),
            ),
            Some(HvfArm64OptionalStateValue::DestinationDefault),
        );
        let sme = HvfArm64SmeRestoreState::try_new(input, &simd_fp)
            .expect("mixed sparse SME state should be valid");
        let request = HvfArm64ReviewedOptionalStateRestore::try_new(
            0,
            Some(1),
            debug_state(
                HvfArm64OptionalStateValue::Explicit(1),
                HvfArm64OptionalStateValue::Explicit(0),
            ),
            debug_state(
                HvfArm64OptionalStateValue::Explicit(2),
                HvfArm64OptionalStateValue::Explicit(0),
            ),
            Some(sme),
            simd_fp,
        )
        .expect("mixed sparse request should be valid");
        let mut access = RecordingAccess::sme2();

        restore_arm64_reviewed_optional_state_with(&mut access, &request)
            .expect("mixed sparse restore should succeed");

        assert_eq!(
            access
                .calls
                .iter()
                .filter(|call| matches!(call, Call::GetZ(_)))
                .count(),
            16
        );
        assert_eq!(
            access
                .calls
                .iter()
                .filter(|call| matches!(call, Call::GetP(_)))
                .count(),
            8
        );
        assert!(!access.calls.contains(&Call::GetZa));
        assert_eq!(
            access
                .calls
                .iter()
                .filter(|call| **call == Call::GetZt0)
                .count(),
            1
        );
        assert_eq!(
            access
                .calls
                .iter()
                .filter(|call| {
                    matches!(
                        call,
                        Call::ReadSystem(HvfSystemRegister::SMCR_EL1)
                            | Call::ReadSystem(HvfSystemRegister::SMPRI_EL1)
                            | Call::ReadSystem(HvfSystemRegister::TPIDR2_EL0)
                    )
                })
                .count(),
            1
        );
        assert!(!access.calls.iter().any(|call| {
            matches!(
                call,
                Call::ReadSystem(register)
                    if *register
                        == HvfSystemRegister::debug_breakpoint_value(0)
                            .expect("breakpoint value should exist")
                        || *register
                            == HvfSystemRegister::debug_watchpoint_value(0)
                                .expect("watchpoint value should exist")
            )
        }));
        assert_eq!(
            access
                .system_writes
                .iter()
                .copied()
                .filter(|(register, _)| {
                    matches!(
                        *register,
                        HvfSystemRegister::SMCR_EL1
                            | HvfSystemRegister::SMPRI_EL1
                            | HvfSystemRegister::TPIDR2_EL0
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (HvfSystemRegister::SMCR_EL1, 0xaaaa),
                (HvfSystemRegister::SMPRI_EL1, 0x2222),
                (HvfSystemRegister::TPIDR2_EL0, 0xbbbb),
            ]
        );
        assert_eq!(access.pstate_write, Some((true, true)));
        assert_eq!(access.z_writes.len(), 32);
        assert!(access.z_writes.iter().all(|(_, bytes)| {
            bytes.len() == TEST_MAX_SVL && bytes.iter().all(|byte| *byte == 0)
        }));
        assert_eq!(access.p_writes.len(), 16);
        for (register, bytes) in &access.p_writes {
            let expected = if register % 2 == 0 {
                u8::try_from(*register).expect("test predicate register should fit in u8")
            } else {
                0
            };
            assert_eq!(bytes.as_slice(), [expected; 2]);
        }
        assert_eq!(
            access.za_write,
            Some(vec![0x7c; TEST_MAX_SVL * TEST_MAX_SVL])
        );
        assert_eq!(access.zt0_write, Some([0xa5; 64]));
        assert_eq!(access.simd_writes.len(), 32);
        assert!(
            access
                .simd_writes
                .iter()
                .all(|(_, value)| *value == [0; 16])
        );
        assert_eq!(
            access.scalar_writes,
            vec![(HvfRegister::FPCR, 0x1234), (HvfRegister::FPSR, 0x5678)]
        );
    }

    #[test]
    fn checked_sme_shape_accepts_streaming_only_and_za_only_modes() {
        let simd_fp = simd(0);
        let streaming_only = sme_input(0, TEST_MAX_SVL, true, false).with_streaming_registers(
            vec![HvfArm64OptionalStateValue::DestinationDefault; 32],
            vec![HvfArm64OptionalStateValue::DestinationDefault; 16],
        );
        let za_only = sme_input(0, TEST_MAX_SVL, false, true)
            .with_za_register(HvfArm64OptionalStateValue::DestinationDefault, None);

        assert!(HvfArm64SmeRestoreState::try_new(streaming_only, &simd_fp).is_ok());
        assert!(HvfArm64SmeRestoreState::try_new(za_only, &simd_fp).is_ok());
    }

    #[test]
    fn state_debug_redacts_debug_sme_and_simd_values() {
        let request = sme2_default_request();
        let debug = format!("{request:?}");

        assert!(!debug.contains("1020304050607080"));
        assert!(!debug.contains("8877665544332211"));
        assert!(!debug.contains("1234"));
        assert!(!debug.contains("5678"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn restore_error_accessors_are_send_sync_and_value_free() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<super::HvfArm64ReviewedOptionalStateRestoreError>();
        assert_send_sync::<HvfArm64ReviewedOptionalStateRestore>();

        let error = super::HvfArm64ReviewedOptionalStateRestoreError::rejection(
            super::HvfArm64ReviewedOptionalStateRestoreFamily::Sme,
            HvfArm64ReviewedOptionalStateRestoreStage::Validation,
            Some(7),
            11,
            HvfArm64ReviewedOptionalStateRestoreRejection::SmeFeature,
        );
        assert_eq!(error.index(), Some(7));
        assert_eq!(error.completed_writes(), 11);
        assert!(!error.to_string().contains("0x"));
        assert!(!format!("{error:?}").contains(SENSITIVE_BACKEND_MESSAGE));
    }
}
