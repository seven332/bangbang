//! Backend-neutral machine configuration model.

use std::fmt;

pub const DEFAULT_VCPU_COUNT: u8 = 1;
pub const DEFAULT_MEM_SIZE_MIB: u64 = 128;
pub const MAX_SUPPORTED_VCPUS: u8 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineConfigInput {
    vcpu_count: u8,
    mem_size_mib: u64,
    smt: bool,
    cpu_template: Option<MachineConfigCpuTemplate>,
    track_dirty_pages: bool,
    huge_pages: MachineConfigHugePages,
}

impl MachineConfigInput {
    pub const fn new(vcpu_count: u8, mem_size_mib: u64) -> Self {
        Self {
            vcpu_count,
            mem_size_mib,
            smt: false,
            cpu_template: None,
            track_dirty_pages: false,
            huge_pages: MachineConfigHugePages::None,
        }
    }

    pub const fn with_smt(mut self, smt: bool) -> Self {
        self.smt = smt;
        self
    }

    pub const fn with_cpu_template(mut self, cpu_template: MachineConfigCpuTemplate) -> Self {
        self.cpu_template = Some(cpu_template);
        self
    }

    pub const fn with_track_dirty_pages(mut self, track_dirty_pages: bool) -> Self {
        self.track_dirty_pages = track_dirty_pages;
        self
    }

    pub const fn with_huge_pages(mut self, huge_pages: MachineConfigHugePages) -> Self {
        self.huge_pages = huge_pages;
        self
    }

    pub fn validate(self) -> Result<MachineConfig, MachineConfigError> {
        MachineConfig::try_from(self)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MachineConfigPatchInput {
    vcpu_count: Option<u8>,
    mem_size_mib: Option<u64>,
    smt: Option<bool>,
    cpu_template: Option<MachineConfigCpuTemplate>,
    track_dirty_pages: Option<bool>,
    huge_pages: Option<MachineConfigHugePages>,
}

impl MachineConfigPatchInput {
    pub const fn new() -> Self {
        Self {
            vcpu_count: None,
            mem_size_mib: None,
            smt: None,
            cpu_template: None,
            track_dirty_pages: None,
            huge_pages: None,
        }
    }

    pub const fn with_vcpu_count(mut self, vcpu_count: u8) -> Self {
        self.vcpu_count = Some(vcpu_count);
        self
    }

    pub const fn with_mem_size_mib(mut self, mem_size_mib: u64) -> Self {
        self.mem_size_mib = Some(mem_size_mib);
        self
    }

    pub const fn with_smt(mut self, smt: bool) -> Self {
        self.smt = Some(smt);
        self
    }

    pub const fn with_cpu_template(mut self, cpu_template: MachineConfigCpuTemplate) -> Self {
        self.cpu_template = Some(cpu_template);
        self
    }

    pub const fn with_track_dirty_pages(mut self, track_dirty_pages: bool) -> Self {
        self.track_dirty_pages = Some(track_dirty_pages);
        self
    }

    pub const fn with_huge_pages(mut self, huge_pages: MachineConfigHugePages) -> Self {
        self.huge_pages = Some(huge_pages);
        self
    }

    pub const fn is_empty(self) -> bool {
        self.vcpu_count.is_none()
            && self.mem_size_mib.is_none()
            && self.smt.is_none()
            && self.cpu_template.is_none()
            && self.track_dirty_pages.is_none()
            && self.huge_pages.is_none()
    }

    pub fn apply_to(self, current: MachineConfig) -> Result<MachineConfig, MachineConfigError> {
        if self.is_empty() {
            return Err(MachineConfigError::EmptyPatch);
        }

        let mut input = MachineConfigInput::new(
            self.vcpu_count.unwrap_or(current.vcpu_count()),
            self.mem_size_mib.unwrap_or(current.mem_size_mib()),
        )
        .with_smt(self.smt.unwrap_or(current.smt()))
        .with_track_dirty_pages(
            self.track_dirty_pages
                .unwrap_or(current.track_dirty_pages()),
        )
        .with_huge_pages(self.huge_pages.unwrap_or(current.huge_pages()));

        if let Some(cpu_template) = self.cpu_template.or(current.cpu_template()) {
            input = input.with_cpu_template(cpu_template);
        }

        input.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MachineConfig {
    vcpu_count: u8,
    mem_size_mib: u64,
    smt: bool,
    cpu_template: Option<MachineConfigCpuTemplate>,
    track_dirty_pages: bool,
    huge_pages: MachineConfigHugePages,
}

impl MachineConfig {
    pub const fn vcpu_count(self) -> u8 {
        self.vcpu_count
    }

    pub const fn mem_size_mib(self) -> u64 {
        self.mem_size_mib
    }

    pub const fn smt(self) -> bool {
        self.smt
    }

    pub const fn cpu_template(self) -> Option<MachineConfigCpuTemplate> {
        self.cpu_template
    }

    pub const fn track_dirty_pages(self) -> bool {
        self.track_dirty_pages
    }

    pub const fn huge_pages(self) -> MachineConfigHugePages {
        self.huge_pages
    }
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            vcpu_count: DEFAULT_VCPU_COUNT,
            mem_size_mib: DEFAULT_MEM_SIZE_MIB,
            smt: false,
            cpu_template: None,
            track_dirty_pages: false,
            huge_pages: MachineConfigHugePages::None,
        }
    }
}

impl TryFrom<MachineConfigInput> for MachineConfig {
    type Error = MachineConfigError;

    fn try_from(input: MachineConfigInput) -> Result<Self, Self::Error> {
        if input.vcpu_count == 0 || input.vcpu_count > MAX_SUPPORTED_VCPUS {
            return Err(MachineConfigError::InvalidVcpuCount);
        }
        if input.mem_size_mib == 0 {
            return Err(MachineConfigError::InvalidMemorySize);
        }
        if input.smt {
            return Err(MachineConfigError::SmtNotSupported);
        }
        if let Some(cpu_template) = input.cpu_template
            && cpu_template != MachineConfigCpuTemplate::None
        {
            return Err(MachineConfigError::UnsupportedCpuTemplate { cpu_template });
        }
        if input.track_dirty_pages {
            return Err(MachineConfigError::DirtyPageTrackingNotSupported);
        }
        if input.huge_pages != MachineConfigHugePages::None {
            return Err(MachineConfigError::HugePagesNotSupported);
        }

        Ok(Self {
            vcpu_count: input.vcpu_count,
            mem_size_mib: input.mem_size_mib,
            smt: input.smt,
            cpu_template: None,
            track_dirty_pages: input.track_dirty_pages,
            huge_pages: input.huge_pages,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineConfigCpuTemplate {
    C3,
    T2,
    T2S,
    T2CL,
    T2A,
    V1N1,
    None,
}

impl fmt::Display for MachineConfigCpuTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::C3 => f.write_str("C3"),
            Self::T2 => f.write_str("T2"),
            Self::T2S => f.write_str("T2S"),
            Self::T2CL => f.write_str("T2CL"),
            Self::T2A => f.write_str("T2A"),
            Self::V1N1 => f.write_str("V1N1"),
            Self::None => f.write_str("None"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MachineConfigHugePages {
    #[default]
    None,
    TwoM,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineConfigError {
    EmptyPatch,
    InvalidVcpuCount,
    InvalidMemorySize,
    SmtNotSupported,
    UnsupportedCpuTemplate {
        cpu_template: MachineConfigCpuTemplate,
    },
    DirtyPageTrackingNotSupported,
    HugePagesNotSupported,
}

impl fmt::Display for MachineConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPatch => f.write_str("machine config patch must update at least one field"),
            Self::InvalidVcpuCount => {
                write!(f, "machine vcpu_count must be in 1..={MAX_SUPPORTED_VCPUS}")
            }
            Self::InvalidMemorySize => f.write_str("machine mem_size_mib must not be zero"),
            Self::SmtNotSupported => f.write_str("machine smt is not supported"),
            Self::UnsupportedCpuTemplate { cpu_template } => {
                write!(f, "machine cpu_template {cpu_template} is not supported")
            }
            Self::DirtyPageTrackingNotSupported => {
                f.write_str("machine track_dirty_pages is not supported")
            }
            Self::HugePagesNotSupported => f.write_str("machine huge_pages is not supported"),
        }
    }
}

impl std::error::Error for MachineConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_machine_config_matches_firecracker_shape() {
        let config = MachineConfig::default();

        assert_eq!(config.vcpu_count(), DEFAULT_VCPU_COUNT);
        assert_eq!(config.mem_size_mib(), DEFAULT_MEM_SIZE_MIB);
        assert!(!config.smt());
        assert_eq!(config.cpu_template(), None);
        assert!(!config.track_dirty_pages());
        assert_eq!(config.huge_pages(), MachineConfigHugePages::None);
    }

    #[test]
    fn validates_machine_config_input() {
        let config = MachineConfigInput::new(2, 256)
            .with_cpu_template(MachineConfigCpuTemplate::None)
            .with_huge_pages(MachineConfigHugePages::None)
            .validate()
            .expect("machine config should validate");

        assert_eq!(config.vcpu_count(), 2);
        assert_eq!(config.mem_size_mib(), 256);
        assert_eq!(config.cpu_template(), None);
        assert_eq!(config.huge_pages(), MachineConfigHugePages::None);
    }

    #[test]
    fn rejects_invalid_machine_config_input() {
        for (input, expected) in [
            (
                MachineConfigInput::new(0, 128),
                MachineConfigError::InvalidVcpuCount,
            ),
            (
                MachineConfigInput::new(MAX_SUPPORTED_VCPUS + 1, 128),
                MachineConfigError::InvalidVcpuCount,
            ),
            (
                MachineConfigInput::new(1, 0),
                MachineConfigError::InvalidMemorySize,
            ),
            (
                MachineConfigInput::new(1, 128).with_smt(true),
                MachineConfigError::SmtNotSupported,
            ),
            (
                MachineConfigInput::new(1, 128).with_cpu_template(MachineConfigCpuTemplate::V1N1),
                MachineConfigError::UnsupportedCpuTemplate {
                    cpu_template: MachineConfigCpuTemplate::V1N1,
                },
            ),
            (
                MachineConfigInput::new(1, 128).with_track_dirty_pages(true),
                MachineConfigError::DirtyPageTrackingNotSupported,
            ),
            (
                MachineConfigInput::new(1, 128).with_huge_pages(MachineConfigHugePages::TwoM),
                MachineConfigError::HugePagesNotSupported,
            ),
        ] {
            assert_eq!(
                input.validate().expect_err("input should be invalid"),
                expected
            );
        }
    }

    #[test]
    fn applies_machine_config_patch_to_current_config() {
        let current = MachineConfigInput::new(2, 256)
            .validate()
            .expect("current machine config should validate");
        let patched = MachineConfigPatchInput::new()
            .with_mem_size_mib(512)
            .with_cpu_template(MachineConfigCpuTemplate::None)
            .apply_to(current)
            .expect("patch should validate");

        assert_eq!(patched.vcpu_count(), 2);
        assert_eq!(patched.mem_size_mib(), 512);
        assert!(!patched.smt());
        assert_eq!(patched.cpu_template(), None);
        assert!(!patched.track_dirty_pages());
        assert_eq!(patched.huge_pages(), MachineConfigHugePages::None);
    }

    #[test]
    fn rejects_empty_machine_config_patch() {
        let err = MachineConfigPatchInput::new()
            .apply_to(MachineConfig::default())
            .expect_err("empty patch should fail");

        assert_eq!(err, MachineConfigError::EmptyPatch);
    }

    #[test]
    fn rejects_invalid_machine_config_patch() {
        for (patch, expected) in [
            (
                MachineConfigPatchInput::new().with_vcpu_count(0),
                MachineConfigError::InvalidVcpuCount,
            ),
            (
                MachineConfigPatchInput::new().with_mem_size_mib(0),
                MachineConfigError::InvalidMemorySize,
            ),
            (
                MachineConfigPatchInput::new().with_smt(true),
                MachineConfigError::SmtNotSupported,
            ),
            (
                MachineConfigPatchInput::new().with_cpu_template(MachineConfigCpuTemplate::T2A),
                MachineConfigError::UnsupportedCpuTemplate {
                    cpu_template: MachineConfigCpuTemplate::T2A,
                },
            ),
            (
                MachineConfigPatchInput::new().with_track_dirty_pages(true),
                MachineConfigError::DirtyPageTrackingNotSupported,
            ),
            (
                MachineConfigPatchInput::new().with_huge_pages(MachineConfigHugePages::TwoM),
                MachineConfigError::HugePagesNotSupported,
            ),
        ] {
            assert_eq!(
                patch
                    .apply_to(MachineConfig::default())
                    .expect_err("patch should be invalid"),
                expected
            );
        }
    }

    #[test]
    fn displays_machine_config_errors() {
        assert_eq!(
            MachineConfigError::EmptyPatch.to_string(),
            "machine config patch must update at least one field"
        );
        assert_eq!(
            MachineConfigError::InvalidVcpuCount.to_string(),
            "machine vcpu_count must be in 1..=32"
        );
        assert_eq!(
            MachineConfigError::InvalidMemorySize.to_string(),
            "machine mem_size_mib must not be zero"
        );
        assert_eq!(
            MachineConfigError::SmtNotSupported.to_string(),
            "machine smt is not supported"
        );
        assert_eq!(
            MachineConfigError::UnsupportedCpuTemplate {
                cpu_template: MachineConfigCpuTemplate::V1N1,
            }
            .to_string(),
            "machine cpu_template V1N1 is not supported"
        );
        assert_eq!(
            MachineConfigError::DirtyPageTrackingNotSupported.to_string(),
            "machine track_dirty_pages is not supported"
        );
        assert_eq!(
            MachineConfigError::HugePagesNotSupported.to_string(),
            "machine huge_pages is not supported"
        );
    }
}
