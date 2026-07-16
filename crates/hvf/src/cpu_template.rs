//! Firecracker-compatible arm64 custom CPU-template application for HVF.

use std::fmt;

use bangbang_runtime::BackendError;
use bangbang_runtime::cpu::{ArmIdRegister, CustomCpuTemplate};

use crate::runner::{HvfVcpuRunner, HvfVcpuRunnerError};
use crate::vcpu::HvfSystemRegister;

const CPU_TEMPLATE_VALUE_REDACTED: &str = "<redacted>";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvfArm64CpuTemplateRegister(HvfSystemRegister);

impl HvfArm64CpuTemplateRegister {
    pub(crate) const fn system_register(self) -> HvfSystemRegister {
        self.0
    }

    #[cfg(test)]
    pub(crate) const fn from_system_register(register: HvfSystemRegister) -> Self {
        Self(register)
    }
}

impl fmt::Debug for HvfArm64CpuTemplateRegister {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(CPU_TEMPLATE_VALUE_REDACTED)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct MappedModifier {
    register: HvfArm64CpuTemplateRegister,
    filter: u64,
    value: u64,
}

impl MappedModifier {
    const fn apply(self, baseline: u64) -> HvfArm64CpuTemplateTarget {
        HvfArm64CpuTemplateTarget {
            register: self.register,
            value: (baseline & !self.filter) | self.value,
        }
    }
}

impl fmt::Debug for MappedModifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MappedModifier")
            .field("register", &CPU_TEMPLATE_VALUE_REDACTED)
            .field("filter", &CPU_TEMPLATE_VALUE_REDACTED)
            .field("value", &CPU_TEMPLATE_VALUE_REDACTED)
            .finish()
    }
}

/// Fully mapped custom template prepared before an HVF VM is created.
pub(crate) struct PreparedHvfArm64CpuTemplate {
    modifiers: Vec<MappedModifier>,
}

impl PreparedHvfArm64CpuTemplate {
    pub(crate) fn from_runtime(template: &CustomCpuTemplate) -> Self {
        let modifiers = template
            .modifiers()
            .iter()
            .copied()
            .map(|modifier| MappedModifier {
                register: HvfArm64CpuTemplateRegister(match modifier.register() {
                    ArmIdRegister::Pfr0 => HvfSystemRegister::ID_AA64PFR0_EL1,
                    ArmIdRegister::Isar0 => HvfSystemRegister::ID_AA64ISAR0_EL1,
                    ArmIdRegister::Isar1 => HvfSystemRegister::ID_AA64ISAR1_EL1,
                    ArmIdRegister::Mmfr2 => HvfSystemRegister::ID_AA64MMFR2_EL1,
                }),
                filter: modifier.filter(),
                value: modifier.value(),
            })
            .collect();
        Self { modifiers }
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
pub(crate) struct HvfArm64CpuTemplateTarget {
    register: HvfArm64CpuTemplateRegister,
    value: u64,
}

impl HvfArm64CpuTemplateTarget {
    #[cfg(test)]
    pub(crate) const fn new(register: HvfArm64CpuTemplateRegister, value: u64) -> Self {
        Self { register, value }
    }

    pub(crate) const fn register(self) -> HvfArm64CpuTemplateRegister {
        self.register
    }

    pub(crate) const fn value(self) -> u64 {
        self.value
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
            Self::InvalidTopology { .. }
            | Self::BaselineLength { .. }
            | Self::BaselineMismatch { .. } => None,
        }
    }
}

trait CpuTemplateMember {
    fn read_cpu_template_baseline(
        &self,
        registers: &[HvfArm64CpuTemplateRegister],
    ) -> Result<Vec<u64>, HvfVcpuRunnerError>;

    fn apply_cpu_template_targets(
        &self,
        targets: &[HvfArm64CpuTemplateTarget],
    ) -> Result<(), HvfVcpuRunnerError>;
}

impl CpuTemplateMember for HvfVcpuRunner<'_> {
    fn read_cpu_template_baseline(
        &self,
        registers: &[HvfArm64CpuTemplateRegister],
    ) -> Result<Vec<u64>, HvfVcpuRunnerError> {
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
        .map(|modifier| modifier.register)
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

    let targets = modifiers
        .iter()
        .copied()
        .zip(common_baseline.iter().copied())
        .map(|(modifier, baseline)| modifier.apply(baseline))
        .collect::<Vec<_>>();
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

pub(crate) fn read_cpu_template_baseline_with(
    registers: &[HvfArm64CpuTemplateRegister],
    mut read: impl FnMut(HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<Vec<u64>, HvfArm64CpuTemplateVcpuError> {
    let mut baseline = Vec::with_capacity(registers.len());
    for register in registers {
        let value = read(register.system_register()).map_err(|source| {
            HvfArm64CpuTemplateVcpuError::BaselineRead {
                completed_reads: baseline.len(),
                source,
            }
        })?;
        baseline.push(value);
    }
    Ok(baseline)
}

pub(crate) fn apply_cpu_template_targets_with<V>(
    targets: &[HvfArm64CpuTemplateTarget],
    vcpu: &mut V,
    mut write: impl FnMut(&mut V, HvfSystemRegister, u64) -> Result<(), BackendError>,
    mut read: impl FnMut(&mut V, HvfSystemRegister) -> Result<u64, BackendError>,
) -> Result<(), HvfArm64CpuTemplateVcpuError> {
    for (completed_modifiers, target) in targets.iter().copied().enumerate() {
        let register = target.register().system_register();
        write(vcpu, register, target.value()).map_err(|source| {
            HvfArm64CpuTemplateVcpuError::RegisterWrite {
                completed_modifiers,
                source,
            }
        })?;
        let actual = read(vcpu, register).map_err(|source| {
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
            registers: Vec<HvfSystemRegister>,
        },
        Apply {
            member: usize,
            targets: Vec<(HvfSystemRegister, u64)>,
        },
    }

    struct RecordingMember {
        index: usize,
        baseline: Vec<u64>,
        events: Rc<RefCell<Vec<MemberEvent>>>,
        read_error: bool,
        apply_error: bool,
    }

    impl CpuTemplateMember for RecordingMember {
        fn read_cpu_template_baseline(
            &self,
            registers: &[HvfArm64CpuTemplateRegister],
        ) -> Result<Vec<u64>, HvfVcpuRunnerError> {
            self.events.borrow_mut().push(MemberEvent::Read {
                member: self.index,
                registers: registers
                    .iter()
                    .map(|register| register.system_register())
                    .collect(),
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
                targets: targets
                    .iter()
                    .map(|target| (target.register().system_register(), target.value()))
                    .collect(),
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
                baseline: vec![0xf0f0, 0xaaaa],
                events: Rc::clone(&events),
                read_error: false,
                apply_error: false,
            },
            RecordingMember {
                index: 1,
                baseline: vec![0xf0f0, 0xaaaa],
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
                        HvfSystemRegister::ID_AA64ISAR1_EL1,
                        HvfSystemRegister::ID_AA64PFR0_EL1,
                    ],
                },
                MemberEvent::Read {
                    member: 1,
                    registers: vec![
                        HvfSystemRegister::ID_AA64ISAR1_EL1,
                        HvfSystemRegister::ID_AA64PFR0_EL1,
                    ],
                },
                MemberEvent::Apply {
                    member: 0,
                    targets: vec![
                        (HvfSystemRegister::ID_AA64ISAR1_EL1, 0xf005),
                        (HvfSystemRegister::ID_AA64PFR0_EL1, 0x2aaa),
                    ],
                },
                MemberEvent::Apply {
                    member: 1,
                    targets: vec![
                        (HvfSystemRegister::ID_AA64ISAR1_EL1, 0xf005),
                        (HvfSystemRegister::ID_AA64PFR0_EL1, 0x2aaa),
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
                baseline: vec![0x10],
                events: Rc::clone(&events),
                read_error: false,
                apply_error: false,
            },
            RecordingMember {
                index: 1,
                baseline: vec![0x11],
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

    fn targets() -> [HvfArm64CpuTemplateTarget; 3] {
        [
            HvfArm64CpuTemplateTarget {
                register: HvfArm64CpuTemplateRegister(HvfSystemRegister::ID_AA64PFR0_EL1),
                value: 0x11,
            },
            HvfArm64CpuTemplateTarget {
                register: HvfArm64CpuTemplateRegister(HvfSystemRegister::ID_AA64ISAR1_EL1),
                value: 0x22,
            },
            HvfArm64CpuTemplateTarget {
                register: HvfArm64CpuTemplateRegister(HvfSystemRegister::ID_AA64MMFR2_EL1),
                value: 0x33,
            },
        ]
    }

    fn apply_targets(access: &mut RegisterAccess) -> Result<(), HvfArm64CpuTemplateVcpuError> {
        apply_cpu_template_targets_with(
            &targets(),
            access,
            |access, register, value| {
                let call = access.write_calls;
                access.write_calls += 1;
                if access.fail_write == Some(call) {
                    return Err(BackendError::InvalidState("injected write failure"));
                }
                if access.ignore_write != Some(call) {
                    if let Some((_, current)) = access
                        .values
                        .iter_mut()
                        .find(|(candidate, _)| *candidate == register)
                    {
                        *current = value;
                    } else {
                        access.values.push((register, value));
                    }
                }
                Ok(())
            },
            |access, register| {
                let call = access.read_calls;
                access.read_calls += 1;
                if access.fail_read == Some(call) {
                    return Err(BackendError::InvalidState("injected readback failure"));
                }
                Ok(access
                    .values
                    .iter()
                    .find_map(|(candidate, value)| (*candidate == register).then_some(*value))
                    .unwrap_or_default())
            },
        )
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
        let modifier = MappedModifier {
            register: HvfArm64CpuTemplateRegister(HvfSystemRegister::ID_AA64PFR0_EL1),
            filter: 0xdead_beef,
            value: 0x1234,
        };
        let target = modifier.apply(0xfeed_face);

        for debug in [format!("{modifier:?}"), format!("{target:?}")] {
            assert!(debug.contains(CPU_TEMPLATE_VALUE_REDACTED));
            for secret in ["deadbeef", "1234", "feedface"] {
                assert!(!debug.contains(secret));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use bangbang_runtime::cpu::{
        CpuConfigArmRegisterModifier, CpuConfigArmRegisterWidth, CpuConfigInput,
        KVM_REG_ARM64_ID_AA64ISAR0_EL1, KVM_REG_ARM64_ID_AA64ISAR1_EL1,
        KVM_REG_ARM64_ID_AA64MMFR2_EL1, KVM_REG_ARM64_ID_AA64PFR0_EL1,
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
            registers: Vec<HvfSystemRegister>,
        },
        Apply {
            member: usize,
            targets: Vec<(HvfSystemRegister, u64)>,
        },
    }

    struct FakeMember {
        index: usize,
        baseline: Vec<u64>,
        fail_read: bool,
        fail_apply: bool,
        events: Rc<RefCell<Vec<Event>>>,
    }

    impl CpuTemplateMember for FakeMember {
        fn read_cpu_template_baseline(
            &self,
            registers: &[HvfArm64CpuTemplateRegister],
        ) -> Result<Vec<u64>, HvfVcpuRunnerError> {
            self.events.borrow_mut().push(Event::Read {
                member: self.index,
                registers: registers
                    .iter()
                    .map(|register| register.system_register())
                    .collect(),
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
                targets: targets
                    .iter()
                    .map(|target| (target.register().system_register(), target.value()))
                    .collect(),
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

    fn prepare(modifiers: Vec<CpuConfigArmRegisterModifier>) -> PreparedHvfArm64CpuTemplate {
        let template = CpuConfigInput::new(Vec::new(), modifiers, Vec::new())
            .into_custom_template()
            .expect("test template should validate")
            .expect("test template should be nonempty");
        PreparedHvfArm64CpuTemplate::from_runtime(&template)
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
            baseline: baseline.to_vec(),
            fail_read: false,
            fail_apply: false,
            events: Rc::clone(&events),
        });

        apply_custom_cpu_template_with(&members, &[0, 1], &template)
            .expect("matching topology should accept the template");

        let expected_registers = vec![
            HvfSystemRegister::ID_AA64PFR0_EL1,
            HvfSystemRegister::ID_AA64ISAR0_EL1,
            HvfSystemRegister::ID_AA64ISAR1_EL1,
            HvfSystemRegister::ID_AA64MMFR2_EL1,
        ];
        let expected_targets = vec![
            (
                HvfSystemRegister::ID_AA64PFR0_EL1,
                baseline[0] & !PFR0_FILTER,
            ),
            (
                HvfSystemRegister::ID_AA64ISAR0_EL1,
                (baseline[1] & !ISAR0_FILTER) | ISAR0_VALUE,
            ),
            (
                HvfSystemRegister::ID_AA64ISAR1_EL1,
                (baseline[2] & !ISAR1_FILTER) | ISAR1_VALUE,
            ),
            (
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
    fn reads_only_requested_registers() {
        let template = prepare(vec![modifier(
            KVM_REG_ARM64_ID_AA64ISAR1_EL1,
            ISAR1_FILTER,
            ISAR1_VALUE,
        )]);
        let events = Rc::new(RefCell::new(Vec::new()));
        let members = [FakeMember {
            index: 0,
            baseline: vec![ISAR1_VALUE],
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
                registers: vec![HvfSystemRegister::ID_AA64ISAR1_EL1],
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
                baseline: vec![1],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
            FakeMember {
                index: 1,
                baseline: vec![2],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
            FakeMember {
                index: 2,
                baseline: vec![1],
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
                baseline: vec![1],
                fail_read: false,
                fail_apply: false,
                events: Rc::clone(&events),
            },
            FakeMember {
                index: 1,
                baseline: vec![1],
                fail_read: false,
                fail_apply: true,
                events: Rc::clone(&events),
            },
            FakeMember {
                index: 2,
                baseline: vec![1],
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
    }

    #[test]
    fn owner_thread_apply_writes_then_immediately_reads_each_target() {
        let targets = [
            HvfArm64CpuTemplateTarget {
                register: HvfArm64CpuTemplateRegister(HvfSystemRegister::ID_AA64PFR0_EL1),
                value: 0x1111,
            },
            HvfArm64CpuTemplateTarget {
                register: HvfArm64CpuTemplateRegister(HvfSystemRegister::ID_AA64ISAR0_EL1),
                value: 0x2222,
            },
        ];
        let mut vcpu = FakeVcpu::default();

        apply_cpu_template_targets_with(
            &targets,
            &mut vcpu,
            |vcpu, register, value| {
                vcpu.events.push(RegisterEvent::Write(register, value));
                vcpu.values.push((register, value));
                Ok(())
            },
            |vcpu, register| {
                vcpu.events.push(RegisterEvent::Read(register));
                vcpu.values
                    .iter()
                    .rev()
                    .find_map(|(candidate, value)| (*candidate == register).then_some(*value))
                    .ok_or(BackendError::InvalidState("test register is unset"))
            },
        )
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
        let target = HvfArm64CpuTemplateTarget {
            register: HvfArm64CpuTemplateRegister(HvfSystemRegister::ID_AA64PFR0_EL1),
            value: 0x1111,
        };

        let error = apply_cpu_template_targets_with(
            &[target],
            &mut (),
            |_, _, _| Ok(()),
            |_, _| Ok(0x2222),
        )
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
        let target = HvfArm64CpuTemplateTarget {
            register: HvfArm64CpuTemplateRegister(HvfSystemRegister::ID_AA64PFR0_EL1),
            value: 0xfeed_face_dead_beef,
        };
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
