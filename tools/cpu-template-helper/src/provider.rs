//! Production Hypervisor.framework effective-profile adapter.

use bangbang_hvf::{
    HvfArm64CpuTemplateInspectionStage, HvfArm64CpuTemplateProfileStatus, HvfArm64CpuTemplateValue,
    inspect_effective_arm64_cpu_template,
};
use bangbang_runtime::cpu::arm64_cpu_template_register_descriptors;

use crate::profile::{
    ArmCpuTemplateValue, EffectiveCpuTemplateProfile, EffectiveCpuTemplateProfileEntry,
    EffectiveCpuTemplateProvider, EffectiveProfileProviderError,
};
use crate::projection::PreparedCpuTemplateInspection;

/// Real one-shot HVF provider used by the public helper executable.
#[derive(Debug, Default)]
pub struct HvfEffectiveCpuTemplateProvider;

impl HvfEffectiveCpuTemplateProvider {
    /// Construct a stateless production provider.
    pub const fn new() -> Self {
        Self
    }
}

impl EffectiveCpuTemplateProvider for HvfEffectiveCpuTemplateProvider {
    fn inspect(
        &mut self,
        request: &PreparedCpuTemplateInspection,
    ) -> Result<EffectiveCpuTemplateProfile, EffectiveProfileProviderError> {
        let profile = inspect_effective_arm64_cpu_template(
            request.machine_config(),
            request.custom_template(),
        )
        .map_err(|error| match error.stage() {
            HvfArm64CpuTemplateInspectionStage::Unsupported => {
                EffectiveProfileProviderError::Unsupported
            }
            HvfArm64CpuTemplateInspectionStage::Prepare => EffectiveProfileProviderError::Prepare,
            HvfArm64CpuTemplateInspectionStage::Apply => EffectiveProfileProviderError::Apply,
            HvfArm64CpuTemplateInspectionStage::Capture => EffectiveProfileProviderError::Capture,
            HvfArm64CpuTemplateInspectionStage::Teardown => EffectiveProfileProviderError::Teardown,
        })?;

        if profile.entries().len() != bangbang_runtime::cpu::ARM64_CPU_TEMPLATE_REGISTER_COUNT {
            return Err(EffectiveProfileProviderError::Capture);
        }
        let entries = profile
            .entries()
            .iter()
            .copied()
            .zip(arm64_cpu_template_register_descriptors())
            .map(|(entry, descriptor)| {
                if entry.register() != descriptor.register() {
                    return Err(EffectiveProfileProviderError::Capture);
                }
                Ok(match entry.status() {
                    HvfArm64CpuTemplateProfileStatus::Available(value) => {
                        EffectiveCpuTemplateProfileEntry::available(
                            descriptor.identity(),
                            helper_value(value),
                        )
                    }
                    HvfArm64CpuTemplateProfileStatus::Unavailable => {
                        EffectiveCpuTemplateProfileEntry::unavailable(descriptor.identity())
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        EffectiveCpuTemplateProfile::try_new(entries)
            .map_err(|_| EffectiveProfileProviderError::Capture)
    }
}

const fn helper_value(value: HvfArm64CpuTemplateValue) -> ArmCpuTemplateValue {
    match value {
        HvfArm64CpuTemplateValue::U32(value) => ArmCpuTemplateValue::U32(value),
        HvfArm64CpuTemplateValue::U64(value) => ArmCpuTemplateValue::U64(value),
        HvfArm64CpuTemplateValue::U128(value) => ArmCpuTemplateValue::U128(value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_width_values_map_without_widening() {
        assert_eq!(
            helper_value(HvfArm64CpuTemplateValue::U32(u32::MAX)),
            ArmCpuTemplateValue::U32(u32::MAX)
        );
        assert_eq!(
            helper_value(HvfArm64CpuTemplateValue::U64(u64::MAX)),
            ArmCpuTemplateValue::U64(u64::MAX)
        );
        assert_eq!(
            helper_value(HvfArm64CpuTemplateValue::U128(u128::MAX)),
            ArmCpuTemplateValue::U128(u128::MAX)
        );
    }
}
