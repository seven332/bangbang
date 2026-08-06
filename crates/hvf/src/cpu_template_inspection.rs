//! CPU-only effective arm64 CPU-template inspection.

use std::fmt;

use bangbang_runtime::VmBackend;
use bangbang_runtime::cpu::{
    ARM64_CPU_TEMPLATE_REGISTER_COUNT, ArmCpuTemplateRegister, ArmRegisterAvailability,
    CustomCpuTemplate, arm64_cpu_template_register_descriptors,
};
use bangbang_runtime::machine::{MachineConfig, MachineConfigCpuTemplate};

use crate::backend::HvfBackend;
use crate::cpu_template::{
    HvfArm64CpuTemplateRegister, HvfArm64CpuTemplateValue, PreparedHvfArm64CpuTemplate,
    map_arm64_cpu_template_register,
};
use crate::topology::HvfVcpuTopology;

const VALUE_REDACTED: &str = "<redacted>";

/// Availability and exact-width value captured for one closed runtime target.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64CpuTemplateProfileStatus {
    /// The target was available and had this topology-common value.
    Available(HvfArm64CpuTemplateValue),
    /// The target's documented public macOS availability tier was absent.
    Unavailable,
}

impl fmt::Debug for HvfArm64CpuTemplateProfileStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available(_) => formatter
                .debug_tuple("Available")
                .field(&VALUE_REDACTED)
                .finish(),
            Self::Unavailable => formatter.write_str("Unavailable"),
        }
    }
}

/// One typed entry in an effective topology-common CPU-template profile.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64CpuTemplateProfileEntry {
    register: ArmCpuTemplateRegister,
    status: HvfArm64CpuTemplateProfileStatus,
}

impl HvfArm64CpuTemplateProfileEntry {
    /// Return the backend-neutral typed target.
    pub const fn register(self) -> ArmCpuTemplateRegister {
        self.register
    }

    /// Return the availability and exact-width effective value.
    pub const fn status(self) -> HvfArm64CpuTemplateProfileStatus {
        self.status
    }
}

impl fmt::Debug for HvfArm64CpuTemplateProfileEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfArm64CpuTemplateProfileEntry")
            .field("register", &VALUE_REDACTED)
            .field("status", &self.status)
            .finish()
    }
}

/// Exact closed effective arm64 CPU-template profile.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfArm64CpuTemplateProfile {
    entries: Vec<HvfArm64CpuTemplateProfileEntry>,
}

impl HvfArm64CpuTemplateProfile {
    /// Return all entries in runtime descriptor order.
    pub fn entries(&self) -> &[HvfArm64CpuTemplateProfileEntry] {
        &self.entries
    }
}

impl fmt::Debug for HvfArm64CpuTemplateProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HvfArm64CpuTemplateProfile")
            .field("entry_count", &self.entries.len())
            .field("values", &VALUE_REDACTED)
            .finish()
    }
}

/// Stable stage for one value-redacted effective inspection failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64CpuTemplateInspectionStage {
    /// The binary target cannot execute Hypervisor.framework.
    Unsupported,
    /// Static selection, native mapping, VM, or topology preparation failed.
    Prepare,
    /// Production CPU-template application/readback failed.
    Apply,
    /// The complete topology-common profile could not be captured.
    Capture,
    /// Topology or VM teardown did not complete cleanly.
    Teardown,
}

/// One bounded effective-inspection failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64CpuTemplateInspectionError {
    stage: HvfArm64CpuTemplateInspectionStage,
}

impl HvfArm64CpuTemplateInspectionError {
    const fn new(stage: HvfArm64CpuTemplateInspectionStage) -> Self {
        Self { stage }
    }

    /// Return the stable failure stage.
    pub const fn stage(self) -> HvfArm64CpuTemplateInspectionStage {
        self.stage
    }
}

impl fmt::Display for HvfArm64CpuTemplateInspectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self.stage {
            HvfArm64CpuTemplateInspectionStage::Unsupported => {
                "effective arm64 CPU inspection is unsupported"
            }
            HvfArm64CpuTemplateInspectionStage::Prepare => {
                "effective arm64 CPU inspection preparation failed"
            }
            HvfArm64CpuTemplateInspectionStage::Apply => {
                "effective arm64 CPU-template application failed"
            }
            HvfArm64CpuTemplateInspectionStage::Capture => {
                "effective arm64 CPU profile capture failed"
            }
            HvfArm64CpuTemplateInspectionStage::Teardown => {
                "effective arm64 CPU inspection teardown failed"
            }
        })
    }
}

impl std::error::Error for HvfArm64CpuTemplateInspectionError {}

#[derive(Clone, Copy)]
struct CapturePlanEntry {
    register: ArmCpuTemplateRegister,
    mapped: Option<HvfArm64CpuTemplateRegister>,
}

/// Inspect one real idle HVF topology at the CPU-template application/readback
/// checkpoint.
///
/// The owner creates no guest memory, GIC, kernel, FDT, device, or run loop.
/// It returns only after all vCPU owner threads and the VM have been torn down.
pub fn inspect_effective_arm64_cpu_template(
    machine_config: MachineConfig,
    custom_template: Option<&CustomCpuTemplate>,
) -> Result<HvfArm64CpuTemplateProfile, HvfArm64CpuTemplateInspectionError> {
    if !HvfBackend::is_supported_target() {
        return Err(HvfArm64CpuTemplateInspectionError::new(
            HvfArm64CpuTemplateInspectionStage::Unsupported,
        ));
    }
    if !matches!(
        machine_config.cpu_template(),
        None | Some(MachineConfigCpuTemplate::None)
    ) {
        return Err(HvfArm64CpuTemplateInspectionError::new(
            HvfArm64CpuTemplateInspectionStage::Prepare,
        ));
    }

    let prepared_template = custom_template
        .map(PreparedHvfArm64CpuTemplate::from_runtime)
        .transpose()
        .map_err(|_| {
            HvfArm64CpuTemplateInspectionError::new(HvfArm64CpuTemplateInspectionStage::Prepare)
        })?;
    let capture_plan = prepare_capture_plan(
        crate::ffi::macos_15_system_registers_available(),
        crate::ffi::macos_15_2_system_registers_available(),
    )?;

    let mut backend = HvfBackend::new();
    backend.create_vm().map_err(|_| {
        HvfArm64CpuTemplateInspectionError::new(HvfArm64CpuTemplateInspectionStage::Prepare)
    })?;

    let operation = inspect_created_vm(
        machine_config.vcpu_count(),
        prepared_template.as_ref(),
        &capture_plan,
    );
    let teardown = backend.destroy_vm();
    prefer_teardown(operation, teardown.is_err())
}

fn inspect_created_vm(
    vcpu_count: u8,
    prepared_template: Option<&PreparedHvfArm64CpuTemplate>,
    capture_plan: &[CapturePlanEntry],
) -> Result<HvfArm64CpuTemplateProfile, HvfArm64CpuTemplateInspectionError> {
    let topology = HvfVcpuTopology::create(vcpu_count, None, None).map_err(|_| {
        HvfArm64CpuTemplateInspectionError::new(HvfArm64CpuTemplateInspectionStage::Prepare)
    })?;

    let operation = (|| {
        if let Some(template) = prepared_template {
            topology
                .apply_arm64_cpu_template_with_state(template)
                .map_err(|_| {
                    HvfArm64CpuTemplateInspectionError::new(
                        HvfArm64CpuTemplateInspectionStage::Apply,
                    )
                })?;
        }
        capture_profile(&topology, capture_plan)
    })();
    let shutdown = topology.shutdown();
    drop(topology);
    prefer_teardown(operation, shutdown.is_err())
}

fn prefer_teardown<T>(
    operation: Result<T, HvfArm64CpuTemplateInspectionError>,
    teardown_failed: bool,
) -> Result<T, HvfArm64CpuTemplateInspectionError> {
    if teardown_failed {
        Err(HvfArm64CpuTemplateInspectionError::new(
            HvfArm64CpuTemplateInspectionStage::Teardown,
        ))
    } else {
        operation
    }
}

fn prepare_capture_plan(
    macos_15_available: bool,
    macos_15_2_available: bool,
) -> Result<Vec<CapturePlanEntry>, HvfArm64CpuTemplateInspectionError> {
    arm64_cpu_template_register_descriptors()
        .map(|descriptor| {
            let available = match descriptor.availability() {
                ArmRegisterAvailability::Baseline => true,
                ArmRegisterAvailability::MacOs15_0 => macos_15_available,
                ArmRegisterAvailability::MacOs15_2 => macos_15_2_available,
            };
            let mapped = available
                .then(|| map_arm64_cpu_template_register(descriptor.register()))
                .transpose()
                .map_err(|_| {
                    HvfArm64CpuTemplateInspectionError::new(
                        HvfArm64CpuTemplateInspectionStage::Prepare,
                    )
                })?;
            Ok(CapturePlanEntry {
                register: descriptor.register(),
                mapped,
            })
        })
        .collect()
}

fn capture_profile(
    topology: &HvfVcpuTopology<'_>,
    capture_plan: &[CapturePlanEntry],
) -> Result<HvfArm64CpuTemplateProfile, HvfArm64CpuTemplateInspectionError> {
    if capture_plan.len() != ARM64_CPU_TEMPLATE_REGISTER_COUNT {
        return Err(HvfArm64CpuTemplateInspectionError::new(
            HvfArm64CpuTemplateInspectionStage::Capture,
        ));
    }
    let mapped = capture_plan
        .iter()
        .filter_map(|entry| entry.mapped)
        .collect::<Vec<_>>();
    let values = topology
        .capture_common_arm64_cpu_template_values(&mapped)
        .map_err(|_| {
            HvfArm64CpuTemplateInspectionError::new(HvfArm64CpuTemplateInspectionStage::Capture)
        })?;
    let mut values = values.into_iter();
    let mut entries = Vec::with_capacity(capture_plan.len());
    for entry in capture_plan {
        let status = if entry.mapped.is_some() {
            HvfArm64CpuTemplateProfileStatus::Available(values.next().ok_or_else(|| {
                HvfArm64CpuTemplateInspectionError::new(HvfArm64CpuTemplateInspectionStage::Capture)
            })?)
        } else {
            HvfArm64CpuTemplateProfileStatus::Unavailable
        };
        entries.push(HvfArm64CpuTemplateProfileEntry {
            register: entry.register,
            status,
        });
    }
    if values.next().is_some() {
        return Err(HvfArm64CpuTemplateInspectionError::new(
            HvfArm64CpuTemplateInspectionStage::Capture,
        ));
    }
    Ok(HvfArm64CpuTemplateProfile { entries })
}

#[cfg(test)]
mod tests {
    use bangbang_runtime::cpu::{
        ArmRegisterAvailability, CpuConfigArmRegisterWidth, arm64_cpu_template_register_descriptors,
    };

    use super::*;
    use crate::cpu_template::{cpu_template_register_tag, cpu_template_register_width};

    #[test]
    fn capture_plan_uses_the_exact_runtime_census_and_native_widths() {
        let plan = prepare_capture_plan(true, true).expect("complete plan should map");
        assert_eq!(plan.len(), ARM64_CPU_TEMPLATE_REGISTER_COUNT);
        for (entry, descriptor) in plan
            .iter()
            .copied()
            .zip(arm64_cpu_template_register_descriptors())
        {
            assert_eq!(entry.register, descriptor.register());
            let mapped = entry.mapped.expect("all tiers should be available");
            assert!(cpu_template_register_tag(mapped).is_some());
            let expected_width = match descriptor.width() {
                CpuConfigArmRegisterWidth::U32 => {
                    crate::cpu_template::HvfArm64CpuTemplateValueWidth::U32
                }
                CpuConfigArmRegisterWidth::U64 => {
                    crate::cpu_template::HvfArm64CpuTemplateValueWidth::U64
                }
                CpuConfigArmRegisterWidth::U128 => {
                    crate::cpu_template::HvfArm64CpuTemplateValueWidth::U128
                }
            };
            assert_eq!(cpu_template_register_width(mapped), expected_width);
        }
    }

    #[test]
    fn capture_plan_marks_only_documented_optional_tiers_unavailable() {
        let plan = prepare_capture_plan(false, false).expect("baseline plan should map");
        let descriptors = arm64_cpu_template_register_descriptors().collect::<Vec<_>>();
        let unavailable = plan
            .iter()
            .zip(&descriptors)
            .filter(|(entry, descriptor)| {
                assert_eq!(
                    entry.mapped.is_none(),
                    descriptor.availability() != ArmRegisterAvailability::Baseline
                );
                entry.mapped.is_none()
            })
            .count();
        assert_eq!(unavailable, 3);

        let macos_15 = prepare_capture_plan(true, false).expect("macOS 15 plan should map");
        assert_eq!(
            macos_15
                .iter()
                .filter(|entry| entry.mapped.is_none())
                .count(),
            2
        );
    }

    #[test]
    fn inspection_errors_and_profiles_redact_targets_and_values() {
        let error =
            HvfArm64CpuTemplateInspectionError::new(HvfArm64CpuTemplateInspectionStage::Capture);
        assert_eq!(error.stage(), HvfArm64CpuTemplateInspectionStage::Capture);
        assert!(!error.to_string().contains("0x"));

        let profile = HvfArm64CpuTemplateProfile {
            entries: vec![HvfArm64CpuTemplateProfileEntry {
                register: arm64_cpu_template_register_descriptors()
                    .next()
                    .expect("descriptor census must be nonempty")
                    .register(),
                status: HvfArm64CpuTemplateProfileStatus::Available(HvfArm64CpuTemplateValue::U32(
                    0xfeed_beef,
                )),
            }],
        };
        let debug = format!("{profile:?}");
        assert!(debug.contains("entry_count"));
        assert!(!debug.contains("feed"));
    }

    #[test]
    fn teardown_failure_overrides_success_and_prior_operation_failure() {
        assert_eq!(prefer_teardown(Ok(7_u8), false), Ok(7));
        let capture =
            HvfArm64CpuTemplateInspectionError::new(HvfArm64CpuTemplateInspectionStage::Capture);
        assert_eq!(prefer_teardown::<u8>(Err(capture), false), Err(capture));
        assert_eq!(
            prefer_teardown(Ok(7_u8), true)
                .expect_err("teardown must override success")
                .stage(),
            HvfArm64CpuTemplateInspectionStage::Teardown
        );
        assert_eq!(
            prefer_teardown::<u8>(Err(capture), true)
                .expect_err("teardown must override an earlier failure")
                .stage(),
            HvfArm64CpuTemplateInspectionStage::Teardown
        );
    }
}
