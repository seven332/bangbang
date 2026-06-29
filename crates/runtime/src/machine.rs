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
            cpu_template: match input.cpu_template {
                Some(MachineConfigCpuTemplate::None) | None => None,
            },
            track_dirty_pages: input.track_dirty_pages,
            huge_pages: input.huge_pages,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineConfigCpuTemplate {
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MachineConfigHugePages {
    #[default]
    None,
    TwoM,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineConfigError {
    InvalidVcpuCount,
    InvalidMemorySize,
    SmtNotSupported,
    DirtyPageTrackingNotSupported,
    HugePagesNotSupported,
}

impl fmt::Display for MachineConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidVcpuCount => {
                write!(f, "machine vcpu_count must be in 1..={MAX_SUPPORTED_VCPUS}")
            }
            Self::InvalidMemorySize => f.write_str("machine mem_size_mib must not be zero"),
            Self::SmtNotSupported => f.write_str("machine smt is not supported"),
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
    fn displays_machine_config_errors_without_user_values() {
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
            MachineConfigError::DirtyPageTrackingNotSupported.to_string(),
            "machine track_dirty_pages is not supported"
        );
        assert_eq!(
            MachineConfigError::HugePagesNotSupported.to_string(),
            "machine huge_pages is not supported"
        );
    }
}
