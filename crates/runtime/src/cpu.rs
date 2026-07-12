//! Backend-neutral CPU configuration model.

use std::fmt;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuConfigInput {
    category: Option<CpuConfigTemplateCategory>,
}

impl CpuConfigInput {
    pub const fn new(category: Option<CpuConfigTemplateCategory>) -> Self {
        Self { category }
    }

    pub const fn noop() -> Self {
        Self::new(None)
    }

    pub const fn with_category(category: CpuConfigTemplateCategory) -> Self {
        Self::new(Some(category))
    }

    pub const fn category(self) -> Option<CpuConfigTemplateCategory> {
        self.category
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuConfigTemplateCategory {
    KvmCapabilities,
    VcpuFeatures,
    ArmRegisterModifiers,
    Mixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuConfigError {
    UnsupportedOnHvf { category: CpuConfigTemplateCategory },
}

impl CpuConfigError {
    pub const fn unsupported_on_hvf(category: CpuConfigTemplateCategory) -> Self {
        Self::UnsupportedOnHvf { category }
    }

    pub const fn category(self) -> CpuConfigTemplateCategory {
        match self {
            Self::UnsupportedOnHvf { category } => category,
        }
    }
}

impl fmt::Display for CpuConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.category() {
            CpuConfigTemplateCategory::KvmCapabilities => f.write_str(
                "cpu-config kvm_capabilities are KVM-specific and are not supported on arm64 HVF",
            ),
            CpuConfigTemplateCategory::VcpuFeatures => f.write_str(
                "cpu-config vcpu_features are KVM vCPU-init-specific and are not supported on arm64 HVF",
            ),
            CpuConfigTemplateCategory::ArmRegisterModifiers => f.write_str(
                "cpu-config reg_modifiers have no safe Firecracker-equivalent feature configuration on arm64 HVF",
            ),
            CpuConfigTemplateCategory::Mixed => f.write_str(
                "mixed cpu-config categories are KVM-specific and are not supported on arm64 HVF",
            ),
        }
    }
}

impl std::error::Error for CpuConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_has_no_category() {
        assert_eq!(CpuConfigInput::noop().category(), None);
    }

    #[test]
    fn input_retains_only_category() {
        for category in [
            CpuConfigTemplateCategory::KvmCapabilities,
            CpuConfigTemplateCategory::VcpuFeatures,
            CpuConfigTemplateCategory::ArmRegisterModifiers,
            CpuConfigTemplateCategory::Mixed,
        ] {
            let input = CpuConfigInput::with_category(category);

            assert_eq!(input.category(), Some(category));
            assert_eq!(
                format!("{input:?}"),
                format!("CpuConfigInput {{ category: Some({category:?}) }}")
            );
        }
    }

    #[test]
    fn errors_are_category_specific_and_value_redacted() {
        for (category, expected) in [
            (
                CpuConfigTemplateCategory::KvmCapabilities,
                "cpu-config kvm_capabilities are KVM-specific and are not supported on arm64 HVF",
            ),
            (
                CpuConfigTemplateCategory::VcpuFeatures,
                "cpu-config vcpu_features are KVM vCPU-init-specific and are not supported on arm64 HVF",
            ),
            (
                CpuConfigTemplateCategory::ArmRegisterModifiers,
                "cpu-config reg_modifiers have no safe Firecracker-equivalent feature configuration on arm64 HVF",
            ),
            (
                CpuConfigTemplateCategory::Mixed,
                "mixed cpu-config categories are KVM-specific and are not supported on arm64 HVF",
            ),
        ] {
            let error = CpuConfigError::unsupported_on_hvf(category);

            assert_eq!(error.category(), category);
            assert_eq!(error.to_string(), expected);
            assert_eq!(
                format!("{error:?}"),
                format!("UnsupportedOnHvf {{ category: {category:?} }}")
            );
        }
    }
}
