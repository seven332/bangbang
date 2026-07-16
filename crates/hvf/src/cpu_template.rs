//! Firecracker-compatible arm64 custom CPU-template application for HVF.

use std::fmt;

use bangbang_runtime::BackendError;
use bangbang_runtime::cpu::{
    ArmIdRegister, ArmRegister32, ArmRegister64, ArmRegister128, ArmRegisterModifier,
    CustomCpuTemplate,
};

use crate::runner::{HvfVcpuRunner, HvfVcpuRunnerError};
use crate::vcpu::{HvfRegister, HvfSimdFpRegister, HvfSystemRegister};

const CPU_TEMPLATE_VALUE_REDACTED: &str = "<redacted>";
const CPU_TEMPLATE_U32_TRANSPORT_WIDTH_MESSAGE: &str =
    "arm64 CPU-template U32 register returned bits outside its architectural width";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HvfArm64CpuTemplateRegister64 {
    General(HvfRegister),
    System(HvfSystemRegister),
}

impl fmt::Debug for HvfArm64CpuTemplateRegister64 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_TEMPLATE_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HvfArm64CpuTemplateRegister {
    U32(HvfRegister),
    U64(HvfArm64CpuTemplateRegister64),
    U128(HvfSimdFpRegister),
}

impl HvfArm64CpuTemplateRegister {
    #[cfg(test)]
    pub(crate) const fn from_system_register(register: HvfSystemRegister) -> Self {
        Self::U64(HvfArm64CpuTemplateRegister64::System(register))
    }
}

impl fmt::Debug for HvfArm64CpuTemplateRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_TEMPLATE_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HvfArm64CpuTemplateValue {
    U32(u32),
    U64(u64),
    U128(u128),
}

impl fmt::Debug for HvfArm64CpuTemplateValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_TEMPLATE_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MappedModifier {
    U32 {
        register: HvfRegister,
        filter: u32,
        value: u32,
    },
    U64 {
        register: HvfArm64CpuTemplateRegister64,
        filter: u64,
        value: u64,
    },
    U128 {
        register: HvfSimdFpRegister,
        filter: u128,
        value: u128,
    },
}

impl MappedModifier {
    const fn register(self) -> HvfArm64CpuTemplateRegister {
        match self {
            Self::U32 { register, .. } => HvfArm64CpuTemplateRegister::U32(register),
            Self::U64 { register, .. } => HvfArm64CpuTemplateRegister::U64(register),
            Self::U128 { register, .. } => HvfArm64CpuTemplateRegister::U128(register),
        }
    }

    const fn apply(self, baseline: HvfArm64CpuTemplateValue) -> Option<HvfArm64CpuTemplateTarget> {
        match (self, baseline) {
            (
                Self::U32 {
                    register,
                    filter,
                    value,
                },
                HvfArm64CpuTemplateValue::U32(baseline),
            ) => Some(HvfArm64CpuTemplateTarget::U32 {
                register,
                value: (baseline & !filter) | value,
            }),
            (
                Self::U64 {
                    register,
                    filter,
                    value,
                },
                HvfArm64CpuTemplateValue::U64(baseline),
            ) => Some(HvfArm64CpuTemplateTarget::U64 {
                register,
                value: (baseline & !filter) | value,
            }),
            (
                Self::U128 {
                    register,
                    filter,
                    value,
                },
                HvfArm64CpuTemplateValue::U128(baseline),
            ) => Some(HvfArm64CpuTemplateTarget::U128 {
                register,
                value: (baseline & !filter) | value,
            }),
            _ => None,
        }
    }

    const fn accepts_baseline(self, baseline: HvfArm64CpuTemplateValue) -> bool {
        matches!(
            (self, baseline),
            (Self::U32 { .. }, HvfArm64CpuTemplateValue::U32(_))
                | (Self::U64 { .. }, HvfArm64CpuTemplateValue::U64(_))
                | (Self::U128 { .. }, HvfArm64CpuTemplateValue::U128(_))
        )
    }
}

impl fmt::Debug for MappedModifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MappedModifier")
            .field("width", &self.register_width())
            .field("register", &CPU_TEMPLATE_VALUE_REDACTED)
            .field("filter", &CPU_TEMPLATE_VALUE_REDACTED)
            .field("value", &CPU_TEMPLATE_VALUE_REDACTED)
            .finish()
    }
}

impl MappedModifier {
    const fn register_width(self) -> &'static str {
        match self {
            Self::U32 { .. } => "U32",
            Self::U64 { .. } => "U64",
            Self::U128 { .. } => "U128",
        }
    }
}

/// Fully mapped custom template prepared before an HVF VM is created.
pub(crate) struct PreparedHvfArm64CpuTemplate {
    modifiers: Vec<MappedModifier>,
}

impl PreparedHvfArm64CpuTemplate {
    pub(crate) fn from_runtime(
        template: &CustomCpuTemplate,
    ) -> Result<Self, HvfArm64CpuTemplateError> {
        let mut modifiers = Vec::with_capacity(template.modifiers().len());
        for modifier in template.modifiers().iter().copied() {
            let modifier = match modifier {
                ArmRegisterModifier::U32 {
                    register,
                    filter,
                    value,
                } => MappedModifier::U32 {
                    register: match register {
                        ArmRegister32::Fpcr => HvfRegister::FPCR,
                        ArmRegister32::Fpsr => HvfRegister::FPSR,
                    },
                    filter,
                    value,
                },
                ArmRegisterModifier::U64 {
                    register,
                    filter,
                    value,
                } => MappedModifier::U64 {
                    register: map_u64_register(register)?,
                    filter,
                    value,
                },
                ArmRegisterModifier::U128 {
                    register: ArmRegister128::Q(register),
                    filter,
                    value,
                } => MappedModifier::U128 {
                    register: HvfSimdFpRegister::q(register.index())
                        .ok_or(HvfArm64CpuTemplateError::InvalidRuntimeRegister)?,
                    filter,
                    value,
                },
            };
            modifiers.push(modifier);
        }
        Ok(Self { modifiers })
    }
}

impl fmt::Debug for PreparedHvfArm64CpuTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedHvfArm64CpuTemplate")
            .field("modifier_count", &self.modifiers.len())
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HvfArm64CpuTemplateTarget {
    U32 {
        register: HvfRegister,
        value: u32,
    },
    U64 {
        register: HvfArm64CpuTemplateRegister64,
        value: u64,
    },
    U128 {
        register: HvfSimdFpRegister,
        value: u128,
    },
}

impl HvfArm64CpuTemplateTarget {
    #[cfg(test)]
    pub(crate) const fn new(register: HvfArm64CpuTemplateRegister, value: u64) -> Self {
        match register {
            HvfArm64CpuTemplateRegister::U64(register) => Self::U64 { register, value },
            HvfArm64CpuTemplateRegister::U32(_) | HvfArm64CpuTemplateRegister::U128(_) => {
                panic!("U64 CPU-template target constructor received another width")
            }
        }
    }

    pub(crate) const fn register(self) -> HvfArm64CpuTemplateRegister {
        match self {
            Self::U32 { register, .. } => HvfArm64CpuTemplateRegister::U32(register),
            Self::U64 { register, .. } => HvfArm64CpuTemplateRegister::U64(register),
            Self::U128 { register, .. } => HvfArm64CpuTemplateRegister::U128(register),
        }
    }

    const fn value(self) -> HvfArm64CpuTemplateValue {
        match self {
            Self::U32 { value, .. } => HvfArm64CpuTemplateValue::U32(value),
            Self::U64 { value, .. } => HvfArm64CpuTemplateValue::U64(value),
            Self::U128 { value, .. } => HvfArm64CpuTemplateValue::U128(value),
        }
    }
}

impl fmt::Debug for HvfArm64CpuTemplateTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfArm64CpuTemplateTarget")
            .field("register", &CPU_TEMPLATE_VALUE_REDACTED)
            .field("value", &CPU_TEMPLATE_VALUE_REDACTED)
            .finish()
    }
}

fn map_u64_register(
    register: ArmRegister64,
) -> Result<HvfArm64CpuTemplateRegister64, HvfArm64CpuTemplateError> {
    Ok(match register {
        ArmRegister64::X(register) => HvfArm64CpuTemplateRegister64::General(
            HvfRegister::general_purpose(register.index())
                .ok_or(HvfArm64CpuTemplateError::InvalidRuntimeRegister)?,
        ),
        ArmRegister64::Pc => HvfArm64CpuTemplateRegister64::General(HvfRegister::PC),
        ArmRegister64::Pstate => HvfArm64CpuTemplateRegister64::General(HvfRegister::CPSR),
        ArmRegister64::SpEl0 => HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::SP_EL0),
        ArmRegister64::SpEl1 => HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::SP_EL1),
        ArmRegister64::ElrEl1 => HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::ELR_EL1),
        ArmRegister64::SpsrEl1 => {
            HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::SPSR_EL1)
        }
        ArmRegister64::Id(register) => HvfArm64CpuTemplateRegister64::System(match register {
            ArmIdRegister::Pfr0 => HvfSystemRegister::ID_AA64PFR0_EL1,
            ArmIdRegister::Isar0 => HvfSystemRegister::ID_AA64ISAR0_EL1,
            ArmIdRegister::Isar1 => HvfSystemRegister::ID_AA64ISAR1_EL1,
            ArmIdRegister::Mmfr2 => HvfSystemRegister::ID_AA64MMFR2_EL1,
        }),
    })
}

/// Failure while one vCPU owner thread reads or applies a custom CPU template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64CpuTemplateVcpuError {
    BaselineRead {
        completed_reads: usize,
        source: BackendError,
    },
    RegisterWrite {
        completed_modifiers: usize,
        source: BackendError,
    },
    RegisterReadback {
        completed_modifiers: usize,
        source: BackendError,
    },
    RegisterReadbackMismatch {
        completed_modifiers: usize,
    },
}

impl HvfArm64CpuTemplateVcpuError {
    /// Return the number of requested register reads completed before failure.
    pub const fn completed_reads(&self) -> usize {
        match self {
            Self::BaselineRead {
                completed_reads, ..
            } => *completed_reads,
            Self::RegisterWrite { .. }
            | Self::RegisterReadback { .. }
            | Self::RegisterReadbackMismatch { .. } => 0,
        }
    }

    /// Return the number of complete write-and-readback operations before failure.
    pub const fn completed_modifiers(&self) -> usize {
        match self {
            Self::RegisterWrite {
                completed_modifiers,
                ..
            }
            | Self::RegisterReadback {
                completed_modifiers,
                ..
            }
            | Self::RegisterReadbackMismatch {
                completed_modifiers,
            } => *completed_modifiers,
            Self::BaselineRead { .. } => 0,
        }
    }
}

impl fmt::Display for HvfArm64CpuTemplateVcpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BaselineRead {
                completed_reads,
                source,
            } => write!(
                f,
                "arm64 CPU-template baseline read failed after {completed_reads} successful reads: {source}"
            ),
            Self::RegisterWrite {
                completed_modifiers,
                source,
            } => write!(
                f,
                "arm64 CPU-template register write failed after {completed_modifiers} verified modifiers: {source}"
            ),
            Self::RegisterReadback {
                completed_modifiers,
                source,
            } => write!(
                f,
                "arm64 CPU-template register readback failed after {completed_modifiers} verified modifiers: {source}"
            ),
            Self::RegisterReadbackMismatch {
                completed_modifiers,
            } => write!(
                f,
                "arm64 CPU-template register readback differed from the requested value after {completed_modifiers} verified modifiers"
            ),
        }
    }
}

impl std::error::Error for HvfArm64CpuTemplateVcpuError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BaselineRead { source, .. }
            | Self::RegisterWrite { source, .. }
            | Self::RegisterReadback { source, .. } => Some(source),
            Self::RegisterReadbackMismatch { .. } => None,
        }
    }
}

/// Failure while coordinating one custom CPU template across an HVF topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64CpuTemplateError {
    InvalidRuntimeRegister,
    InvalidTopology {
        member_count: usize,
        mpidr_count: usize,
    },
    BaselineRead {
        member_index: usize,
        mpidr: u64,
        completed_members: usize,
        source: Box<HvfVcpuRunnerError>,
    },
    BaselineLength {
        member_index: usize,
        mpidr: u64,
        completed_members: usize,
        expected: usize,
        actual: usize,
    },
    BaselineMismatch {
        member_index: usize,
        mpidr: u64,
        completed_members: usize,
        completed_modifiers: usize,
    },
    BaselineWidth {
        member_index: usize,
        mpidr: u64,
        completed_members: usize,
        completed_modifiers: usize,
    },
    Apply {
        member_index: usize,
        mpidr: u64,
        completed_members: usize,
        source: Box<HvfVcpuRunnerError>,
    },
}

impl fmt::Display for HvfArm64CpuTemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRuntimeRegister => f.write_str(
                "arm64 CPU-template runtime register was outside its validated finite profile",
            ),
            Self::InvalidTopology {
                member_count,
                mpidr_count,
            } => write!(
                f,
                "arm64 CPU-template topology contained {member_count} member(s) and {mpidr_count} MPIDR value(s)"
            ),
            Self::BaselineRead {
                member_index,
                mpidr,
                completed_members,
                source,
            } => write!(
                f,
                "failed to read arm64 CPU-template baseline for vCPU {member_index} (MPIDR 0x{mpidr:x}) after {completed_members} completed member(s): {source}"
            ),
            Self::BaselineLength {
                member_index,
                mpidr,
                completed_members,
                expected,
                actual,
            } => write!(
                f,
                "arm64 CPU-template baseline for vCPU {member_index} (MPIDR 0x{mpidr:x}) contained {actual} entries after {completed_members} completed member(s), expected {expected}"
            ),
            Self::BaselineMismatch {
                member_index,
                mpidr,
                completed_members,
                completed_modifiers,
            } => write!(
                f,
                "arm64 CPU-template baseline differs after {completed_modifiers} matched modifier(s) for vCPU {member_index} (MPIDR 0x{mpidr:x}); all {completed_members} member(s) were read"
            ),
            Self::BaselineWidth {
                member_index,
                mpidr,
                completed_members,
                completed_modifiers,
            } => write!(
                f,
                "arm64 CPU-template baseline width differs from its mapped register after {completed_modifiers} matched modifier(s) for vCPU {member_index} (MPIDR 0x{mpidr:x}); all {completed_members} member(s) were read"
            ),
            Self::Apply {
                member_index,
                mpidr,
                completed_members,
                source,
            } => write!(
                f,
                "failed to apply arm64 CPU template to vCPU {member_index} (MPIDR 0x{mpidr:x}) after {completed_members} completed member(s): {source}"
            ),
        }
    }
}

impl std::error::Error for HvfArm64CpuTemplateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BaselineRead { source, .. } | Self::Apply { source, .. } => Some(source.as_ref()),
            Self::InvalidRuntimeRegister
            | Self::InvalidTopology { .. }
            | Self::BaselineLength { .. }
            | Self::BaselineMismatch { .. }
            | Self::BaselineWidth { .. } => None,
        }
    }
}

trait CpuTemplateMember {
    fn read_cpu_template_baseline(
        &self,
        registers: &[HvfArm64CpuTemplateRegister],
    ) -> Result<Vec<HvfArm64CpuTemplateValue>, HvfVcpuRunnerError>;

    fn apply_cpu_template_targets(
        &self,
        targets: &[HvfArm64CpuTemplateTarget],
    ) -> Result<(), HvfVcpuRunnerError>;
}

impl CpuTemplateMember for HvfVcpuRunner<'_> {
    fn read_cpu_template_baseline(
        &self,
        registers: &[HvfArm64CpuTemplateRegister],
    ) -> Result<Vec<HvfArm64CpuTemplateValue>, HvfVcpuRunnerError> {
        HvfVcpuRunner::read_arm64_cpu_template_baseline(self, registers)
    }

    fn apply_cpu_template_targets(
        &self,
        targets: &[HvfArm64CpuTemplateTarget],
    ) -> Result<(), HvfVcpuRunnerError> {
        HvfVcpuRunner::apply_arm64_cpu_template_targets(self, targets)
    }
}

pub(crate) fn apply_arm64_cpu_template(
    runners: &[HvfVcpuRunner<'_>],
    mpidrs: &[u64],
    template: &PreparedHvfArm64CpuTemplate,
) -> Result<(), HvfArm64CpuTemplateError> {
    apply_custom_cpu_template_with(runners, mpidrs, template)
}

fn apply_custom_cpu_template_with<M: CpuTemplateMember>(
    members: &[M],
    mpidrs: &[u64],
    template: &PreparedHvfArm64CpuTemplate,
) -> Result<(), HvfArm64CpuTemplateError> {
    if members.is_empty() || members.len() != mpidrs.len() {
        return Err(HvfArm64CpuTemplateError::InvalidTopology {
            member_count: members.len(),
            mpidr_count: mpidrs.len(),
        });
    }

    let modifiers = &template.modifiers;
    if modifiers.is_empty() {
        return Ok(());
    }

    let registers = modifiers
        .iter()
        .copied()
        .map(MappedModifier::register)
        .collect::<Vec<_>>();
    let mut baselines = Vec::with_capacity(members.len());
    for (member_index, (member, &mpidr)) in members.iter().zip(mpidrs).enumerate() {
        let baseline = member
            .read_cpu_template_baseline(&registers)
            .map_err(|source| HvfArm64CpuTemplateError::BaselineRead {
                member_index,
                mpidr,
                completed_members: baselines.len(),
                source: Box::new(source),
            })?;
        if baseline.len() != modifiers.len() {
            return Err(HvfArm64CpuTemplateError::BaselineLength {
                member_index,
                mpidr,
                completed_members: baselines.len() + 1,
                expected: modifiers.len(),
                actual: baseline.len(),
            });
        }
        baselines.push(baseline);
    }

    let Some(common_baseline) = baselines.first() else {
        return Ok(());
    };
    let Some(&common_mpidr) = mpidrs.first() else {
        return Err(HvfArm64CpuTemplateError::InvalidTopology {
            member_count: members.len(),
            mpidr_count: mpidrs.len(),
        });
    };
    for (member_index, (baseline, &mpidr)) in baselines.iter().zip(mpidrs).enumerate() {
        if let Some(completed_modifiers) = modifiers
            .iter()
            .copied()
            .zip(baseline.iter().copied())
            .position(|(modifier, value)| !modifier.accepts_baseline(value))
        {
            return Err(HvfArm64CpuTemplateError::BaselineWidth {
                member_index,
                mpidr,
                completed_members: baselines.len(),
                completed_modifiers,
            });
        }
    }
    for (member_index, (baseline, &mpidr)) in baselines.iter().zip(mpidrs).enumerate().skip(1) {
        if let Some(completed_modifiers) = baseline
            .iter()
            .zip(common_baseline)
            .position(|(actual, expected)| actual != expected)
        {
            return Err(HvfArm64CpuTemplateError::BaselineMismatch {
                member_index,
                mpidr,
                completed_members: baselines.len(),
                completed_modifiers,
            });
        }
    }

    let mut targets = Vec::with_capacity(modifiers.len());
    for (modifier, baseline) in modifiers
        .iter()
        .copied()
        .zip(common_baseline.iter().copied())
    {
        let Some(target) = modifier.apply(baseline) else {
            return Err(HvfArm64CpuTemplateError::BaselineWidth {
                member_index: 0,
                mpidr: common_mpidr,
                completed_members: baselines.len(),
                completed_modifiers: targets.len(),
            });
        };
        targets.push(target);
    }
    for (member_index, (member, &mpidr)) in members.iter().zip(mpidrs).enumerate() {
        member
            .apply_cpu_template_targets(&targets)
            .map_err(|source| HvfArm64CpuTemplateError::Apply {
                member_index,
                mpidr,
                completed_members: member_index,
                source: Box::new(source),
            })?;
    }

    Ok(())
}

pub(crate) trait HvfArm64CpuTemplateAccess {
    fn read_general_register(&mut self, register: HvfRegister) -> Result<u64, BackendError>;

    fn write_general_register(
        &mut self,
        register: HvfRegister,
        value: u64,
    ) -> Result<(), BackendError>;

    fn read_simd_fp_register(
        &mut self,
        register: HvfSimdFpRegister,
    ) -> Result<[u8; 16], BackendError>;

    fn write_simd_fp_register(
        &mut self,
        register: HvfSimdFpRegister,
        value: [u8; 16],
    ) -> Result<(), BackendError>;

    fn read_system_register(&mut self, register: HvfSystemRegister) -> Result<u64, BackendError>;

    fn write_system_register(
        &mut self,
        register: HvfSystemRegister,
        value: u64,
    ) -> Result<(), BackendError>;
}

pub(crate) fn read_cpu_template_baseline_with<A: HvfArm64CpuTemplateAccess + ?Sized>(
    registers: &[HvfArm64CpuTemplateRegister],
    access: &mut A,
) -> Result<Vec<HvfArm64CpuTemplateValue>, HvfArm64CpuTemplateVcpuError> {
    let mut baseline = Vec::with_capacity(registers.len());
    for register in registers.iter().copied() {
        let value = read_cpu_template_register(access, register).map_err(|source| {
            HvfArm64CpuTemplateVcpuError::BaselineRead {
                completed_reads: baseline.len(),
                source,
            }
        })?;
        baseline.push(value);
    }
    Ok(baseline)
}

pub(crate) fn apply_cpu_template_targets_with<A: HvfArm64CpuTemplateAccess + ?Sized>(
    targets: &[HvfArm64CpuTemplateTarget],
    access: &mut A,
) -> Result<(), HvfArm64CpuTemplateVcpuError> {
    for (completed_modifiers, target) in targets.iter().copied().enumerate() {
        write_cpu_template_target(access, target).map_err(|source| {
            HvfArm64CpuTemplateVcpuError::RegisterWrite {
                completed_modifiers,
                source,
            }
        })?;
        let actual = read_cpu_template_register(access, target.register()).map_err(|source| {
            HvfArm64CpuTemplateVcpuError::RegisterReadback {
                completed_modifiers,
                source,
            }
        })?;
        if actual != target.value() {
            return Err(HvfArm64CpuTemplateVcpuError::RegisterReadbackMismatch {
                completed_modifiers,
            });
        }
    }
    Ok(())
}

fn read_cpu_template_register<A: HvfArm64CpuTemplateAccess + ?Sized>(
    access: &mut A,
    register: HvfArm64CpuTemplateRegister,
) -> Result<HvfArm64CpuTemplateValue, BackendError> {
    match register {
        HvfArm64CpuTemplateRegister::U32(register) => access
            .read_general_register(register)
            .and_then(|value| {
                u32::try_from(value).map_err(|_| {
                    BackendError::InvalidState(CPU_TEMPLATE_U32_TRANSPORT_WIDTH_MESSAGE)
                })
            })
            .map(HvfArm64CpuTemplateValue::U32),
        HvfArm64CpuTemplateRegister::U64(HvfArm64CpuTemplateRegister64::General(register)) => {
            access
                .read_general_register(register)
                .map(HvfArm64CpuTemplateValue::U64)
        }
        HvfArm64CpuTemplateRegister::U64(HvfArm64CpuTemplateRegister64::System(register)) => access
            .read_system_register(register)
            .map(HvfArm64CpuTemplateValue::U64),
        HvfArm64CpuTemplateRegister::U128(register) => access
            .read_simd_fp_register(register)
            .map(u128::from_le_bytes)
            .map(HvfArm64CpuTemplateValue::U128),
    }
}

fn write_cpu_template_target<A: HvfArm64CpuTemplateAccess + ?Sized>(
    access: &mut A,
    target: HvfArm64CpuTemplateTarget,
) -> Result<(), BackendError> {
    match target {
        HvfArm64CpuTemplateTarget::U32 { register, value } => {
            access.write_general_register(register, u64::from(value))
        }
        HvfArm64CpuTemplateTarget::U64 {
            register: HvfArm64CpuTemplateRegister64::General(register),
            value,
        } => access.write_general_register(register, value),
        HvfArm64CpuTemplateTarget::U64 {
            register: HvfArm64CpuTemplateRegister64::System(register),
            value,
        } => access.write_system_register(register, value),
        HvfArm64CpuTemplateTarget::U128 { register, value } => {
            access.write_simd_fp_register(register, value.to_le_bytes())
        }
    }
}

#[cfg(test)]
mod supplementary_tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use bangbang_runtime::cpu::{
        CpuConfigArmRegisterModifier, CpuConfigArmRegisterWidth, CpuConfigInput,
        KVM_REG_ARM64_ID_AA64ISAR1_EL1, KVM_REG_ARM64_ID_AA64MMFR2_EL1,
        KVM_REG_ARM64_ID_AA64PFR0_EL1,
    };

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum MemberEvent {
        Read {
            member: usize,
            registers: Vec<HvfArm64CpuTemplateRegister>,
        },
        Apply {
            member: usize,
            targets: Vec<HvfArm64CpuTemplateTarget>,
        },
    }

    struct RecordingMember {
        index: usize,
        baseline: Vec<HvfArm64CpuTemplateValue>,
        events: Rc<RefCell<Vec<MemberEvent>>>,
        read_error: bool,
        apply_error: bool,
    }

    impl CpuTemplateMember for RecordingMember {
        fn read_cpu_template_baseline(
            &self,
            registers: &[HvfArm64CpuTemplateRegister],
        ) -> Result<Vec<HvfArm64CpuTemplateValue>, HvfVcpuRunnerError> {
            self.events.borrow_mut().push(MemberEvent::Read {
                member: self.index,
                registers: registers.to_vec(),
            });
            if self.read_error {
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "injected CPU-template baseline failure",
                )))
            } else {
                Ok(self.baseline.clone())
            }
        }

        fn apply_cpu_template_targets(
            &self,
            targets: &[HvfArm64CpuTemplateTarget],
        ) -> Result<(), HvfVcpuRunnerError> {
            self.events.borrow_mut().push(MemberEvent::Apply {
                member: self.index,
                targets: targets.to_vec(),
            });
            if self.apply_error {
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "injected CPU-template apply failure",
                )))
            } else {
                Ok(())
            }
        }
    }

    fn custom_template(
        modifiers: Vec<CpuConfigArmRegisterModifier>,
    ) -> PreparedHvfArm64CpuTemplate {
        let template = CpuConfigInput::new(Vec::new(), modifiers, Vec::new())
            .into_custom_template()
            .expect("test CPU-template input should validate")
            .expect("nonempty modifiers should produce a template");
        PreparedHvfArm64CpuTemplate::from_runtime(&template)
            .expect("validated test CPU template should map to HVF")
    }

    #[test]
    fn reads_only_requested_registers_on_every_member_before_applying_common_targets() {
        let template = custom_template(vec![
            CpuConfigArmRegisterModifier::new(
                KVM_REG_ARM64_ID_AA64ISAR1_EL1,
                CpuConfigArmRegisterWidth::U64,
                0x00ff,
                0x0005,
            ),
            CpuConfigArmRegisterModifier::new(
                KVM_REG_ARM64_ID_AA64PFR0_EL1,
                CpuConfigArmRegisterWidth::U64,
                0xf000,
                0x2000,
            ),
        ]);
        let events = Rc::new(RefCell::new(Vec::new()));
        let members = [
            RecordingMember {
                index: 0,
                baseline: vec![
                    HvfArm64CpuTemplateValue::U64(0xf0f0),
                    HvfArm64CpuTemplateValue::U64(0xaaaa),
                ],
                events: Rc::clone(&events),
                read_error: false,
                apply_error: false,
            },
            RecordingMember {
                index: 1,
                baseline: vec![
                    HvfArm64CpuTemplateValue::U64(0xf0f0),
                    HvfArm64CpuTemplateValue::U64(0xaaaa),
                ],
                events: Rc::clone(&events),
                read_error: false,
                apply_error: false,
            },
        ];

        apply_custom_cpu_template_with(&members, &[0, 1], &template)
            .expect("matching baselines should apply");

        assert_eq!(
            *events.borrow(),
            [
                MemberEvent::Read {
                    member: 0,
                    registers: vec![
                        HvfArm64CpuTemplateRegister::from_system_register(
                            HvfSystemRegister::ID_AA64ISAR1_EL1,
                        ),
                        HvfArm64CpuTemplateRegister::from_system_register(
                            HvfSystemRegister::ID_AA64PFR0_EL1,
                        ),
                    ],
                },
                MemberEvent::Read {
                    member: 1,
                    registers: vec![
                        HvfArm64CpuTemplateRegister::from_system_register(
                            HvfSystemRegister::ID_AA64ISAR1_EL1,
                        ),
                        HvfArm64CpuTemplateRegister::from_system_register(
                            HvfSystemRegister::ID_AA64PFR0_EL1,
                        ),
                    ],
                },
                MemberEvent::Apply {
                    member: 0,
                    targets: vec![
                        HvfArm64CpuTemplateTarget::new(
                            HvfArm64CpuTemplateRegister::from_system_register(
                                HvfSystemRegister::ID_AA64ISAR1_EL1,
                            ),
                            0xf005,
                        ),
                        HvfArm64CpuTemplateTarget::new(
                            HvfArm64CpuTemplateRegister::from_system_register(
                                HvfSystemRegister::ID_AA64PFR0_EL1,
                            ),
                            0x2aaa,
                        ),
                    ],
                },
                MemberEvent::Apply {
                    member: 1,
                    targets: vec![
                        HvfArm64CpuTemplateTarget::new(
                            HvfArm64CpuTemplateRegister::from_system_register(
                                HvfSystemRegister::ID_AA64ISAR1_EL1,
                            ),
                            0xf005,
                        ),
                        HvfArm64CpuTemplateTarget::new(
                            HvfArm64CpuTemplateRegister::from_system_register(
                                HvfSystemRegister::ID_AA64PFR0_EL1,
                            ),
                            0x2aaa,
                        ),
                    ],
                },
            ]
        );
    }

    #[test]
    fn baseline_mismatch_returns_before_every_write() {
        let template = custom_template(vec![CpuConfigArmRegisterModifier::new(
            KVM_REG_ARM64_ID_AA64MMFR2_EL1,
            CpuConfigArmRegisterWidth::U64,
            0xff,
            0x02,
        )]);
        let events = Rc::new(RefCell::new(Vec::new()));
        let members = [
            RecordingMember {
                index: 0,
                baseline: vec![HvfArm64CpuTemplateValue::U64(0x10)],
                events: Rc::clone(&events),
                read_error: false,
                apply_error: false,
            },
            RecordingMember {
                index: 1,
                baseline: vec![HvfArm64CpuTemplateValue::U64(0x11)],
                events: Rc::clone(&events),
                read_error: false,
                apply_error: false,
            },
        ];

        assert_eq!(
            apply_custom_cpu_template_with(&members, &[0, 1], &template),
            Err(HvfArm64CpuTemplateError::BaselineMismatch {
                member_index: 1,
                mpidr: 1,
                completed_members: 2,
                completed_modifiers: 0,
            })
        );
        assert!(
            events
                .borrow()
                .iter()
                .all(|event| matches!(event, MemberEvent::Read { .. }))
        );
    }

    #[derive(Default)]
    struct RegisterAccess {
        values: Vec<(HvfSystemRegister, u64)>,
        write_calls: usize,
        read_calls: usize,
        fail_write: Option<usize>,
        fail_read: Option<usize>,
        ignore_write: Option<usize>,
    }

    impl HvfArm64CpuTemplateAccess for RegisterAccess {
        fn read_general_register(&mut self, _register: HvfRegister) -> Result<u64, BackendError> {
            Err(BackendError::InvalidState(
                "unexpected general register read",
            ))
        }

        fn write_general_register(
            &mut self,
            _register: HvfRegister,
            _value: u64,
        ) -> Result<(), BackendError> {
            Err(BackendError::InvalidState(
                "unexpected general register write",
            ))
        }

        fn read_simd_fp_register(
            &mut self,
            _register: HvfSimdFpRegister,
        ) -> Result<[u8; 16], BackendError> {
            Err(BackendError::InvalidState("unexpected SIMD register read"))
        }

        fn write_simd_fp_register(
            &mut self,
            _register: HvfSimdFpRegister,
            _value: [u8; 16],
        ) -> Result<(), BackendError> {
            Err(BackendError::InvalidState("unexpected SIMD register write"))
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            let call = self.read_calls;
            self.read_calls += 1;
            if self.fail_read == Some(call) {
                return Err(BackendError::InvalidState("injected readback failure"));
            }
            Ok(self
                .values
                .iter()
                .find_map(|(candidate, value)| (*candidate == register).then_some(*value))
                .unwrap_or_default())
        }

        fn write_system_register(
            &mut self,
            register: HvfSystemRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            let call = self.write_calls;
            self.write_calls += 1;
            if self.fail_write == Some(call) {
                return Err(BackendError::InvalidState("injected write failure"));
            }
            if self.ignore_write != Some(call) {
                if let Some((_, current)) = self
                    .values
                    .iter_mut()
                    .find(|(candidate, _)| *candidate == register)
                {
                    *current = value;
                } else {
                    self.values.push((register, value));
                }
            }
            Ok(())
        }
    }

    fn targets() -> [HvfArm64CpuTemplateTarget; 3] {
        [
            HvfArm64CpuTemplateTarget::new(
                HvfArm64CpuTemplateRegister::from_system_register(
                    HvfSystemRegister::ID_AA64PFR0_EL1,
                ),
                0x11,
            ),
            HvfArm64CpuTemplateTarget::new(
                HvfArm64CpuTemplateRegister::from_system_register(
                    HvfSystemRegister::ID_AA64ISAR1_EL1,
                ),
                0x22,
            ),
            HvfArm64CpuTemplateTarget::new(
                HvfArm64CpuTemplateRegister::from_system_register(
                    HvfSystemRegister::ID_AA64MMFR2_EL1,
                ),
                0x33,
            ),
        ]
    }

    fn apply_targets(access: &mut RegisterAccess) -> Result<(), HvfArm64CpuTemplateVcpuError> {
        apply_cpu_template_targets_with(&targets(), access)
    }

    #[test]
    fn every_target_position_reports_write_readback_and_mismatch_failures() {
        for failed_index in 0..targets().len() {
            let mut write_failure = RegisterAccess {
                fail_write: Some(failed_index),
                ..RegisterAccess::default()
            };
            assert!(matches!(
                apply_targets(&mut write_failure),
                Err(HvfArm64CpuTemplateVcpuError::RegisterWrite {
                    completed_modifiers,
                    ..
                }) if completed_modifiers == failed_index
            ));

            let mut read_failure = RegisterAccess {
                fail_read: Some(failed_index),
                ..RegisterAccess::default()
            };
            assert!(matches!(
                apply_targets(&mut read_failure),
                Err(HvfArm64CpuTemplateVcpuError::RegisterReadback {
                    completed_modifiers,
                    ..
                }) if completed_modifiers == failed_index
            ));

            let mut mismatch = RegisterAccess {
                ignore_write: Some(failed_index),
                ..RegisterAccess::default()
            };
            assert_eq!(
                apply_targets(&mut mismatch),
                Err(HvfArm64CpuTemplateVcpuError::RegisterReadbackMismatch {
                    completed_modifiers: failed_index,
                })
            );
        }
    }

    #[test]
    fn target_and_modifier_debug_output_redacts_registers_masks_and_values() {
        let modifier = MappedModifier::U64 {
            register: HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::ID_AA64PFR0_EL1),
            filter: 0xdead_beef,
            value: 0x1234,
        };
        let target = modifier.apply(HvfArm64CpuTemplateValue::U64(0xfeed_face));

        for debug in [format!("{modifier:?}"), format!("{target:?}")] {
            assert!(debug.contains(CPU_TEMPLATE_VALUE_REDACTED));
            for secret in ["deadbeef", "1234", "feedface"] {
                assert!(!debug.contains(secret));
            }
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum TypedAccessEvent {
        ReadGeneral(HvfRegister),
        WriteGeneral(HvfRegister, u64),
        ReadSimd(HvfSimdFpRegister),
        WriteSimd(HvfSimdFpRegister, [u8; 16]),
        ReadSystem(HvfSystemRegister),
        WriteSystem(HvfSystemRegister, u64),
    }

    #[derive(Default)]
    struct TypedAccess {
        general: Vec<(HvfRegister, u64)>,
        simd: Vec<(HvfSimdFpRegister, [u8; 16])>,
        system: Vec<(HvfSystemRegister, u64)>,
        events: Vec<TypedAccessEvent>,
        write_calls: usize,
        read_calls: usize,
        fail_write: Option<usize>,
        fail_read: Option<usize>,
        ignore_write: Option<usize>,
    }

    impl TypedAccess {
        fn begin_read(&mut self) -> Result<(), BackendError> {
            let call = self.read_calls;
            self.read_calls += 1;
            if self.fail_read == Some(call) {
                Err(BackendError::InvalidState(
                    "injected typed CPU-template read failure",
                ))
            } else {
                Ok(())
            }
        }

        fn begin_write(&mut self) -> Result<bool, BackendError> {
            let call = self.write_calls;
            self.write_calls += 1;
            if self.fail_write == Some(call) {
                Err(BackendError::InvalidState(
                    "injected typed CPU-template write failure",
                ))
            } else {
                Ok(self.ignore_write != Some(call))
            }
        }
    }

    impl HvfArm64CpuTemplateAccess for TypedAccess {
        fn read_general_register(&mut self, register: HvfRegister) -> Result<u64, BackendError> {
            self.events.push(TypedAccessEvent::ReadGeneral(register));
            self.begin_read()?;
            self.general
                .iter()
                .rev()
                .find_map(|(candidate, value)| (*candidate == register).then_some(*value))
                .ok_or(BackendError::InvalidState(
                    "typed general register is unset",
                ))
        }

        fn write_general_register(
            &mut self,
            register: HvfRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            self.events
                .push(TypedAccessEvent::WriteGeneral(register, value));
            if self.begin_write()? {
                self.general.push((register, value));
            }
            Ok(())
        }

        fn read_simd_fp_register(
            &mut self,
            register: HvfSimdFpRegister,
        ) -> Result<[u8; 16], BackendError> {
            self.events.push(TypedAccessEvent::ReadSimd(register));
            self.begin_read()?;
            self.simd
                .iter()
                .rev()
                .find_map(|(candidate, value)| (*candidate == register).then_some(*value))
                .ok_or(BackendError::InvalidState("typed SIMD register is unset"))
        }

        fn write_simd_fp_register(
            &mut self,
            register: HvfSimdFpRegister,
            value: [u8; 16],
        ) -> Result<(), BackendError> {
            self.events
                .push(TypedAccessEvent::WriteSimd(register, value));
            if self.begin_write()? {
                self.simd.push((register, value));
            }
            Ok(())
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.events.push(TypedAccessEvent::ReadSystem(register));
            self.begin_read()?;
            self.system
                .iter()
                .rev()
                .find_map(|(candidate, value)| (*candidate == register).then_some(*value))
                .ok_or(BackendError::InvalidState("typed system register is unset"))
        }

        fn write_system_register(
            &mut self,
            register: HvfSystemRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            self.events
                .push(TypedAccessEvent::WriteSystem(register, value));
            if self.begin_write()? {
                self.system.push((register, value));
            }
            Ok(())
        }
    }

    #[test]
    fn mixed_width_access_is_ordered_exact_and_little_endian() {
        let q31 = HvfSimdFpRegister::q(31).expect("Q31 should map");
        let q_value = 0xf0e1_d2c3_b4a5_9687_7869_5a4b_3c2d_1e0f_u128;
        let targets = [
            HvfArm64CpuTemplateTarget::U32 {
                register: HvfRegister::FPCR,
                value: 0x8000_0001,
            },
            HvfArm64CpuTemplateTarget::U64 {
                register: HvfArm64CpuTemplateRegister64::General(
                    HvfRegister::general_purpose(4).expect("X4 should map"),
                ),
                value: 0x8000_0000_0000_0001,
            },
            HvfArm64CpuTemplateTarget::U64 {
                register: HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::SP_EL0),
                value: 0x1234_5678_9abc_def0,
            },
            HvfArm64CpuTemplateTarget::U128 {
                register: q31,
                value: q_value,
            },
        ];
        let mut access = TypedAccess::default();

        apply_cpu_template_targets_with(&targets, &mut access)
            .expect("mixed-width exact readback should succeed");

        assert_eq!(
            access.events,
            [
                TypedAccessEvent::WriteGeneral(HvfRegister::FPCR, 0x8000_0001),
                TypedAccessEvent::ReadGeneral(HvfRegister::FPCR),
                TypedAccessEvent::WriteGeneral(
                    HvfRegister::general_purpose(4).expect("X4 should map"),
                    0x8000_0000_0000_0001,
                ),
                TypedAccessEvent::ReadGeneral(
                    HvfRegister::general_purpose(4).expect("X4 should map"),
                ),
                TypedAccessEvent::WriteSystem(HvfSystemRegister::SP_EL0, 0x1234_5678_9abc_def0,),
                TypedAccessEvent::ReadSystem(HvfSystemRegister::SP_EL0),
                TypedAccessEvent::WriteSimd(q31, q_value.to_le_bytes()),
                TypedAccessEvent::ReadSimd(q31),
            ]
        );
    }

    #[test]
    fn every_mixed_width_target_position_reports_failures_and_retries() {
        let x4 = HvfRegister::general_purpose(4).expect("X4 should map");
        let q31 = HvfSimdFpRegister::q(31).expect("Q31 should map");
        let q_value = 0xf0e1_d2c3_b4a5_9687_7869_5a4b_3c2d_1e0f_u128;
        let targets = [
            HvfArm64CpuTemplateTarget::U32 {
                register: HvfRegister::FPCR,
                value: 1,
            },
            HvfArm64CpuTemplateTarget::U64 {
                register: HvfArm64CpuTemplateRegister64::General(x4),
                value: 2,
            },
            HvfArm64CpuTemplateTarget::U64 {
                register: HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::SP_EL0),
                value: 3,
            },
            HvfArm64CpuTemplateTarget::U128 {
                register: q31,
                value: q_value,
            },
        ];
        let initialized_access = || TypedAccess {
            general: vec![(HvfRegister::FPCR, 0), (x4, 0)],
            simd: vec![(q31, [0; 16])],
            system: vec![(HvfSystemRegister::SP_EL0, 0)],
            ..TypedAccess::default()
        };

        for failed_index in 0..targets.len() {
            let mut write_failure = TypedAccess {
                fail_write: Some(failed_index),
                ..initialized_access()
            };
            assert!(matches!(
                apply_cpu_template_targets_with(&targets, &mut write_failure),
                Err(HvfArm64CpuTemplateVcpuError::RegisterWrite {
                    completed_modifiers,
                    ..
                }) if completed_modifiers == failed_index
            ));
            write_failure.fail_write = None;
            apply_cpu_template_targets_with(&targets, &mut write_failure)
                .expect("a complete retry after a typed write failure should succeed");

            let mut read_failure = TypedAccess {
                fail_read: Some(failed_index),
                ..initialized_access()
            };
            assert!(matches!(
                apply_cpu_template_targets_with(&targets, &mut read_failure),
                Err(HvfArm64CpuTemplateVcpuError::RegisterReadback {
                    completed_modifiers,
                    ..
                }) if completed_modifiers == failed_index
            ));
            read_failure.fail_read = None;
            apply_cpu_template_targets_with(&targets, &mut read_failure)
                .expect("a complete retry after a typed readback failure should succeed");

            let mut mismatch = TypedAccess {
                ignore_write: Some(failed_index),
                ..initialized_access()
            };
            assert_eq!(
                apply_cpu_template_targets_with(&targets, &mut mismatch),
                Err(HvfArm64CpuTemplateVcpuError::RegisterReadbackMismatch {
                    completed_modifiers: failed_index,
                })
            );
            mismatch.ignore_write = None;
            apply_cpu_template_targets_with(&targets, &mut mismatch)
                .expect("a complete retry after a typed mismatch should succeed");
        }
    }

    #[test]
    fn mixed_width_baseline_preserves_values_and_rejects_u32_transport_upper_bits() {
        let q0 = HvfSimdFpRegister::q(0).expect("Q0 should map");
        let q_value = 0x8070_6050_4030_2010_0f1e_2d3c_4b5a_6978_u128;
        let registers = [
            HvfArm64CpuTemplateRegister::U32(HvfRegister::FPSR),
            HvfArm64CpuTemplateRegister::U64(HvfArm64CpuTemplateRegister64::General(
                HvfRegister::PC,
            )),
            HvfArm64CpuTemplateRegister::U64(HvfArm64CpuTemplateRegister64::System(
                HvfSystemRegister::SPSR_EL1,
            )),
            HvfArm64CpuTemplateRegister::U128(q0),
        ];
        let mut access = TypedAccess {
            general: vec![
                (HvfRegister::FPSR, u32::MAX.into()),
                (HvfRegister::PC, 0x55aa),
            ],
            simd: vec![(q0, q_value.to_le_bytes())],
            system: vec![(HvfSystemRegister::SPSR_EL1, 0xaa55)],
            ..TypedAccess::default()
        };

        assert_eq!(
            read_cpu_template_baseline_with(&registers, &mut access),
            Ok(vec![
                HvfArm64CpuTemplateValue::U32(u32::MAX),
                HvfArm64CpuTemplateValue::U64(0x55aa),
                HvfArm64CpuTemplateValue::U64(0xaa55),
                HvfArm64CpuTemplateValue::U128(q_value),
            ])
        );

        let mut invalid = TypedAccess {
            general: vec![(HvfRegister::FPCR, 1_u64 << 32)],
            ..TypedAccess::default()
        };
        let error = read_cpu_template_baseline_with(
            &[HvfArm64CpuTemplateRegister::U32(HvfRegister::FPCR)],
            &mut invalid,
        )
        .expect_err("U32 transport upper bits must fail closed");
        assert_eq!(error.completed_reads(), 0);
        assert_eq!(
            error.to_string(),
            format!(
                "arm64 CPU-template baseline read failed after 0 successful reads: {}",
                BackendError::InvalidState(CPU_TEMPLATE_U32_TRANSPORT_WIDTH_MESSAGE)
            )
        );
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use bangbang_runtime::cpu::{
        CpuConfigArmRegisterModifier, CpuConfigArmRegisterWidth, CpuConfigInput,
        KVM_REG_ARM64_CORE_ELR_EL1, KVM_REG_ARM64_CORE_FPCR, KVM_REG_ARM64_CORE_FPSR,
        KVM_REG_ARM64_CORE_PC, KVM_REG_ARM64_CORE_PSTATE, KVM_REG_ARM64_CORE_SP_EL0,
        KVM_REG_ARM64_CORE_SP_EL1, KVM_REG_ARM64_CORE_SPSR_EL1, KVM_REG_ARM64_ID_AA64ISAR0_EL1,
        KVM_REG_ARM64_ID_AA64ISAR1_EL1, KVM_REG_ARM64_ID_AA64MMFR2_EL1,
        KVM_REG_ARM64_ID_AA64PFR0_EL1, kvm_reg_arm64_core_q, kvm_reg_arm64_core_x,
    };

    use super::*;

    const PFR0_FILTER: u64 = 0x000f_000f_0000_0000;
    const ISAR0_FILTER: u64 = 0xf0ff_0fff_0000_f000;
    const ISAR0_VALUE: u64 = 0x1000;
    const ISAR1_FILTER: u64 = 0x00ff_f000_00ff_f00f;
    const ISAR1_VALUE: u64 = 0x0010_0001;
    const MMFR2_FILTER: u64 = 0x0000_000f_0000_0000;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        Read {
            member: usize,
            registers: Vec<HvfArm64CpuTemplateRegister>,
        },
        Apply {
            member: usize,
            targets: Vec<HvfArm64CpuTemplateTarget>,
        },
    }

    struct FakeMember {
        index: usize,
        baseline: Vec<HvfArm64CpuTemplateValue>,
        fail_read: bool,
        fail_apply: bool,
        events: Rc<RefCell<Vec<Event>>>,
    }

    impl CpuTemplateMember for FakeMember {
        fn read_cpu_template_baseline(
            &self,
            registers: &[HvfArm64CpuTemplateRegister],
        ) -> Result<Vec<HvfArm64CpuTemplateValue>, HvfVcpuRunnerError> {
            self.events.borrow_mut().push(Event::Read {
                member: self.index,
                registers: registers.to_vec(),
            });
            if self.fail_read {
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "test baseline read failure",
                )))
            } else {
                Ok(self.baseline.clone())
            }
        }

        fn apply_cpu_template_targets(
            &self,
            targets: &[HvfArm64CpuTemplateTarget],
        ) -> Result<(), HvfVcpuRunnerError> {
            self.events.borrow_mut().push(Event::Apply {
                member: self.index,
                targets: targets.to_vec(),
            });
            if self.fail_apply {
                Err(HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "test target apply failure",
                )))
            } else {
                Ok(())
            }
        }
    }

    fn modifier(id: u64, filter: u64, value: u64) -> CpuConfigArmRegisterModifier {
        CpuConfigArmRegisterModifier::new(
            id,
            CpuConfigArmRegisterWidth::U64,
            u128::from(filter),
            u128::from(value),
        )
    }

    fn typed_modifier(
        id: u64,
        width: CpuConfigArmRegisterWidth,
        filter: u128,
        value: u128,
    ) -> CpuConfigArmRegisterModifier {
        CpuConfigArmRegisterModifier::new(id, width, filter, value)
    }

    fn prepare(modifiers: Vec<CpuConfigArmRegisterModifier>) -> PreparedHvfArm64CpuTemplate {
        let template = CpuConfigInput::new(Vec::new(), modifiers, Vec::new())
            .into_custom_template()
            .expect("test template should validate")
            .expect("test template should be nonempty");
        PreparedHvfArm64CpuTemplate::from_runtime(&template)
            .expect("validated test CPU template should map to HVF")
    }

    fn canonical_template() -> PreparedHvfArm64CpuTemplate {
        prepare(vec![
            modifier(KVM_REG_ARM64_ID_AA64PFR0_EL1, PFR0_FILTER, 0),
            modifier(KVM_REG_ARM64_ID_AA64ISAR0_EL1, ISAR0_FILTER, ISAR0_VALUE),
            modifier(KVM_REG_ARM64_ID_AA64ISAR1_EL1, ISAR1_FILTER, ISAR1_VALUE),
            modifier(KVM_REG_ARM64_ID_AA64MMFR2_EL1, MMFR2_FILTER, 0),
        ])
    }

    #[test]
    fn maps_every_reviewed_core_identity_to_its_exact_hvf_operation() {
        for index in [0_u8].into_iter().chain(4..=30) {
            let prepared = prepare(vec![typed_modifier(
                kvm_reg_arm64_core_x(index).expect("reviewed X index should map"),
                CpuConfigArmRegisterWidth::U64,
                u64::MAX.into(),
                1,
            )]);
            assert_eq!(
                prepared.modifiers,
                [MappedModifier::U64 {
                    register: HvfArm64CpuTemplateRegister64::General(
                        HvfRegister::general_purpose(index).expect("reviewed X index should map"),
                    ),
                    filter: u64::MAX,
                    value: 1,
                }]
            );
        }

        for (id, register) in [
            (
                KVM_REG_ARM64_CORE_PC,
                HvfArm64CpuTemplateRegister64::General(HvfRegister::PC),
            ),
            (
                KVM_REG_ARM64_CORE_PSTATE,
                HvfArm64CpuTemplateRegister64::General(HvfRegister::CPSR),
            ),
            (
                KVM_REG_ARM64_CORE_SP_EL0,
                HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::SP_EL0),
            ),
            (
                KVM_REG_ARM64_CORE_SP_EL1,
                HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::SP_EL1),
            ),
            (
                KVM_REG_ARM64_CORE_ELR_EL1,
                HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::ELR_EL1),
            ),
            (
                KVM_REG_ARM64_CORE_SPSR_EL1,
                HvfArm64CpuTemplateRegister64::System(HvfSystemRegister::SPSR_EL1),
            ),
        ] {
            let prepared = prepare(vec![typed_modifier(
                id,
                CpuConfigArmRegisterWidth::U64,
                u64::MAX.into(),
                1,
            )]);
            assert_eq!(
                prepared.modifiers,
                [MappedModifier::U64 {
                    register,
                    filter: u64::MAX,
                    value: 1,
                }]
            );
        }

        for (id, register) in [
            (KVM_REG_ARM64_CORE_FPCR, HvfRegister::FPCR),
            (KVM_REG_ARM64_CORE_FPSR, HvfRegister::FPSR),
        ] {
            let prepared = prepare(vec![typed_modifier(
                id,
                CpuConfigArmRegisterWidth::U32,
                u32::MAX.into(),
                1,
            )]);
            assert_eq!(
                prepared.modifiers,
                [MappedModifier::U32 {
                    register,
                    filter: u32::MAX,
                    value: 1,
                }]
            );
        }

        for index in 0..=31 {
            let prepared = prepare(vec![typed_modifier(
                kvm_reg_arm64_core_q(index).expect("reviewed Q index should map"),
                CpuConfigArmRegisterWidth::U128,
                u128::MAX,
                1 << 127,
            )]);
            assert_eq!(
                prepared.modifiers,
                [MappedModifier::U128 {
                    register: HvfSimdFpRegister::q(index).expect("reviewed Q index should map"),
                    filter: u128::MAX,
                    value: 1 << 127,
                }]
            );
        }
    }

    fn system_register(register: HvfSystemRegister) -> HvfArm64CpuTemplateRegister {
        HvfArm64CpuTemplateRegister::from_system_register(register)
    }

    fn system_target(register: HvfSystemRegister, value: u64) -> HvfArm64CpuTemplateTarget {
        HvfArm64CpuTemplateTarget::new(system_register(register), value)
    }

    #[test]
    fn computes_all_four_targets_once_and_reads_every_member_before_writing() {
        let template = canonical_template();
        let baseline = [
            0x1234_5678_9abc_def0,
            0xfedc_ba98_7654_3210,
            0x0123_4567_89ab_cdef,
            0x0f0f_f0f0_55aa_aa55,
        ];
        let events = Rc::new(RefCell::new(Vec::new()));
        let members = [0, 1].map(|index| FakeMember {
            index,
            baseline: baseline
                .into_iter()
                .map(HvfArm64CpuTemplateValue::U64)
                .collect(),
            fail_read: false,
            fail_apply: false,
            events: Rc::clone(&events),
        });

        apply_custom_cpu_template_with(&members, &[0, 1], &template)
            .expect("matching topology should accept the template");

        let expected_registers = vec![
            system_register(HvfSystemRegister::ID_AA64PFR0_EL1),
            system_register(HvfSystemRegister::ID_AA64ISAR0_EL1),
            system_register(HvfSystemRegister::ID_AA64ISAR1_EL1),
            system_register(HvfSystemRegister::ID_AA64MMFR2_EL1),
        ];
        let expected_targets = vec![
            system_target(
                HvfSystemRegister::ID_AA64PFR0_EL1,
                baseline[0] & !PFR0_FILTER,
            ),
            system_target(
                HvfSystemRegister::ID_AA64ISAR0_EL1,
                (baseline[1] & !ISAR0_FILTER) | ISAR0_VALUE,
            ),
            system_target(
                HvfSystemRegister::ID_AA64ISAR1_EL1,
                (baseline[2] & !ISAR1_FILTER) | ISAR1_VALUE,
            ),
            system_target(
                HvfSystemRegister::ID_AA64MMFR2_EL1,
                baseline[3] & !MMFR2_FILTER,
            ),
        ];
        assert_eq!(
            events.borrow().as_slice(),
            [
                Event::Read {
                    member: 0,
                    registers: expected_registers.clone(),
                },
                Event::Read {
                    member: 1,
                    registers: expected_registers,
                },
                Event::Apply {
                    member: 0,
                    targets: expected_targets.clone(),
                },
                Event::Apply {
                    member: 1,
                    targets: expected_targets,
                },
            ]
        );
    }

    #[test]
    fn mixed_width_topology_reads_every_member_before_ordered_targets() {
        let x4 = HvfRegister::general_purpose(4).expect("X4 should map");
        let q31 = HvfSimdFpRegister::q(31).expect("Q31 should map");
        let template = prepare(vec![
            typed_modifier(
                KVM_REG_ARM64_CORE_FPCR,
                CpuConfigArmRegisterWidth::U32,
                u32::MAX.into(),
                0x8000_0001,
            ),
            typed_modifier(
                kvm_reg_arm64_core_x(4).expect("X4 should have a KVM identity"),
                CpuConfigArmRegisterWidth::U64,
                u64::MAX.into(),
                0x8000_0000_0000_0001,
            ),
            typed_modifier(
                kvm_reg_arm64_core_q(31).expect("Q31 should have a KVM identity"),
                CpuConfigArmRegisterWidth::U128,
                u128::MAX,
                (1 << 127) | 1,
            ),
        ]);
        let baseline = vec![
            HvfArm64CpuTemplateValue::U32(0),
            HvfArm64CpuTemplateValue::U64(0),
            HvfArm64CpuTemplateValue::U128(0),
        ];
        let events = Rc::new(RefCell::new(Vec::new()));
        let members = [0, 1].map(|index| FakeMember {
            index,
            baseline: baseline.clone(),
            fail_read: false,
            fail_apply: false,
            events: Rc::clone(&events),
        });

        apply_custom_cpu_template_with(&members, &[4, 5], &template)
            .expect("matching mixed-width baselines should apply");

        let registers = vec![
            HvfArm64CpuTemplateRegister::U32(HvfRegister::FPCR),
            HvfArm64CpuTemplateRegister::U64(HvfArm64CpuTemplateRegister64::General(x4)),
            HvfArm64CpuTemplateRegister::U128(q31),
        ];
        let targets = vec![
            HvfArm64CpuTemplateTarget::U32 {
                register: HvfRegister::FPCR,
                value: 0x8000_0001,
            },
            HvfArm64CpuTemplateTarget::U64 {
                register: HvfArm64CpuTemplateRegister64::General(x4),
                value: 0x8000_0000_0000_0001,
            },
            HvfArm64CpuTemplateTarget::U128 {
                register: q31,
                value: (1 << 127) | 1,
            },
        ];
        assert_eq!(
            events.borrow().as_slice(),
            [
                Event::Read {
                    member: 0,
                    registers: registers.clone(),
                },
                Event::Read {
                    member: 1,
                    registers,
                },
                Event::Apply {
                    member: 0,
                    targets: targets.clone(),
                },
                Event::Apply { member: 1, targets },
            ]
        );
    }

    #[test]
    fn reads_only_requested_registers() {
        let template = prepare(vec![modifier(
            KVM_REG_ARM64_ID_AA64ISAR1_EL1,
            ISAR1_FILTER,
            ISAR1_VALUE,
        )]);
        let events = Rc::new(RefCell::new(Vec::new()));
        let members = [FakeMember {
            index: 0,
            baseline: vec![HvfArm64CpuTemplateValue::U64(ISAR1_VALUE)],
            fail_read: false,
            fail_apply: false,
            events: Rc::clone(&events),
        }];

        apply_custom_cpu_template_with(&members, &[0], &template)
            .expect("one requested register should apply");

        assert_eq!(
            events.borrow()[0],
            Event::Read {
                member: 0,
                registers: vec![system_register(HvfSystemRegister::ID_AA64ISAR1_EL1)],
            }
        );
    }

    #[test]
    fn baseline_mismatch_finishes_all_reads_and_performs_no_writes() {
        let template = prepare(vec![modifier(
            KVM_REG_ARM64_ID_AA64PFR0_EL1,
            PFR0_FILTER,
            0,
        )]);
        let events = Rc::new(RefCell::new(Vec::new()));
        let members = [
            FakeMember {
                index: 0,
                baseline: vec![HvfArm64CpuTemplateValue::U64(1)],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
            FakeMember {
                index: 1,
                baseline: vec![HvfArm64CpuTemplateValue::U64(2)],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
            FakeMember {
                index: 2,
                baseline: vec![HvfArm64CpuTemplateValue::U64(1)],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
        ];

        let error = apply_custom_cpu_template_with(&members, &[0, 1, 2], &template)
            .expect_err("different baselines must fail");

        assert_eq!(
            error,
            HvfArm64CpuTemplateError::BaselineMismatch {
                member_index: 1,
                mpidr: 1,
                completed_members: 3,
                completed_modifiers: 0,
            }
        );
        assert!(
            events
                .borrow()
                .iter()
                .all(|event| matches!(event, Event::Read { .. }))
        );
        assert_eq!(events.borrow().len(), 3);
    }

    #[test]
    fn baseline_width_mismatch_fails_before_every_write() {
        let template = prepare(vec![modifier(
            KVM_REG_ARM64_ID_AA64PFR0_EL1,
            PFR0_FILTER,
            0,
        )]);
        let events = Rc::new(RefCell::new(Vec::new()));
        let members = [
            FakeMember {
                index: 0,
                baseline: vec![HvfArm64CpuTemplateValue::U64(1)],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
            FakeMember {
                index: 1,
                baseline: vec![HvfArm64CpuTemplateValue::U32(1)],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
        ];

        assert_eq!(
            apply_custom_cpu_template_with(&members, &[7, 8], &template),
            Err(HvfArm64CpuTemplateError::BaselineWidth {
                member_index: 1,
                mpidr: 8,
                completed_members: 2,
                completed_modifiers: 0,
            })
        );
        assert_eq!(events.borrow().len(), 2);
        assert!(
            events
                .borrow()
                .iter()
                .all(|event| matches!(event, Event::Read { .. }))
        );
    }

    #[test]
    fn apply_failure_reports_completed_members_after_full_preflight() {
        let template = prepare(vec![modifier(
            KVM_REG_ARM64_ID_AA64PFR0_EL1,
            PFR0_FILTER,
            0,
        )]);
        let events = Rc::new(RefCell::new(Vec::new()));
        let members = [
            FakeMember {
                index: 0,
                baseline: vec![HvfArm64CpuTemplateValue::U64(1)],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
            FakeMember {
                index: 1,
                baseline: vec![HvfArm64CpuTemplateValue::U64(1)],
                fail_read: false,
                fail_apply: true,
                events: Rc::clone(&events),
            },
            FakeMember {
                index: 2,
                baseline: vec![HvfArm64CpuTemplateValue::U64(1)],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
        ];

        let error = apply_custom_cpu_template_with(&members, &[4, 5, 6], &template)
            .expect_err("member apply failure must stop publication");

        assert!(matches!(
            error,
            HvfArm64CpuTemplateError::Apply {
                member_index: 1,
                mpidr: 5,
                completed_members: 1,
                ..
            }
        ));
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| matches!(event, Event::Read { .. }))
                .count(),
            3
        );
        assert_eq!(
            events
                .borrow()
                .iter()
                .filter(|event| matches!(event, Event::Apply { .. }))
                .count(),
            2
        );
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RegisterEvent {
        Write(HvfSystemRegister, u64),
        Read(HvfSystemRegister),
    }

    #[derive(Default)]
    struct FakeVcpu {
        events: Vec<RegisterEvent>,
        values: Vec<(HvfSystemRegister, u64)>,
        read_override: Option<u64>,
    }

    impl HvfArm64CpuTemplateAccess for FakeVcpu {
        fn read_general_register(&mut self, _register: HvfRegister) -> Result<u64, BackendError> {
            Err(BackendError::InvalidState(
                "unexpected general register read",
            ))
        }

        fn write_general_register(
            &mut self,
            _register: HvfRegister,
            _value: u64,
        ) -> Result<(), BackendError> {
            Err(BackendError::InvalidState(
                "unexpected general register write",
            ))
        }

        fn read_simd_fp_register(
            &mut self,
            _register: HvfSimdFpRegister,
        ) -> Result<[u8; 16], BackendError> {
            Err(BackendError::InvalidState("unexpected SIMD register read"))
        }

        fn write_simd_fp_register(
            &mut self,
            _register: HvfSimdFpRegister,
            _value: [u8; 16],
        ) -> Result<(), BackendError> {
            Err(BackendError::InvalidState("unexpected SIMD register write"))
        }

        fn read_system_register(
            &mut self,
            register: HvfSystemRegister,
        ) -> Result<u64, BackendError> {
            self.events.push(RegisterEvent::Read(register));
            if let Some(value) = self.read_override {
                return Ok(value);
            }
            self.values
                .iter()
                .rev()
                .find_map(|(candidate, value)| (*candidate == register).then_some(*value))
                .ok_or(BackendError::InvalidState("test register is unset"))
        }

        fn write_system_register(
            &mut self,
            register: HvfSystemRegister,
            value: u64,
        ) -> Result<(), BackendError> {
            self.events.push(RegisterEvent::Write(register, value));
            self.values.push((register, value));
            Ok(())
        }
    }

    #[test]
    fn owner_thread_apply_writes_then_immediately_reads_each_target() {
        let targets = [
            system_target(HvfSystemRegister::ID_AA64PFR0_EL1, 0x1111),
            system_target(HvfSystemRegister::ID_AA64ISAR0_EL1, 0x2222),
        ];
        let mut vcpu = FakeVcpu::default();

        apply_cpu_template_targets_with(&targets, &mut vcpu)
            .expect("exact readback should succeed");

        assert_eq!(
            vcpu.events,
            [
                RegisterEvent::Write(HvfSystemRegister::ID_AA64PFR0_EL1, 0x1111),
                RegisterEvent::Read(HvfSystemRegister::ID_AA64PFR0_EL1),
                RegisterEvent::Write(HvfSystemRegister::ID_AA64ISAR0_EL1, 0x2222),
                RegisterEvent::Read(HvfSystemRegister::ID_AA64ISAR0_EL1),
            ]
        );
    }

    #[test]
    fn owner_thread_readback_mismatch_reports_completed_modifier_count() {
        let target = system_target(HvfSystemRegister::ID_AA64PFR0_EL1, 0x1111);
        let mut vcpu = FakeVcpu {
            read_override: Some(0x2222),
            ..FakeVcpu::default()
        };

        let error = apply_cpu_template_targets_with(&[target], &mut vcpu)
            .expect_err("non-exact readback must fail");

        assert_eq!(
            error,
            HvfArm64CpuTemplateVcpuError::RegisterReadbackMismatch {
                completed_modifiers: 0,
            }
        );
    }

    #[test]
    fn debug_output_redacts_registers_masks_targets_and_readbacks() {
        let template = canonical_template();
        let target = system_target(HvfSystemRegister::ID_AA64PFR0_EL1, 0xfeed_face_dead_beef);
        let prepared_debug = format!("{template:?}");
        let target_debug = format!("{target:?}");
        let error_debug = format!(
            "{:?}",
            HvfArm64CpuTemplateVcpuError::RegisterReadbackMismatch {
                completed_modifiers: 0,
            }
        );

        assert!(!prepared_debug.contains("c020"));
        assert!(!prepared_debug.contains("000f000f"));
        assert!(!target_debug.contains("feed"));
        assert!(!target_debug.contains("c020"));
        assert!(!error_debug.contains("feed"));
        assert!(!error_debug.contains("c020"));
    }
}
