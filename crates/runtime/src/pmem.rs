//! Backend-neutral pmem configuration model.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemConfigInput {
    id: String,
    path_on_host: String,
    root_device: bool,
    read_only: bool,
    rate_limiter_configured: bool,
}

impl PmemConfigInput {
    pub fn new(id: impl Into<String>, path_on_host: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            path_on_host: path_on_host.into(),
            root_device: false,
            read_only: false,
            rate_limiter_configured: false,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path_on_host(&self) -> &str {
        &self.path_on_host
    }

    pub const fn root_device(&self) -> bool {
        self.root_device
    }

    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
    }

    pub const fn with_root_device(mut self, root_device: bool) -> Self {
        self.root_device = root_device;
        self
    }

    pub const fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub const fn with_rate_limiter_configured(mut self) -> Self {
        self.rate_limiter_configured = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemConfig {
    id: String,
    path_on_host: String,
    root_device: bool,
    read_only: bool,
}

impl PmemConfig {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path_on_host(&self) -> &str {
        &self.path_on_host
    }

    pub const fn root_device(&self) -> bool {
        self.root_device
    }

    pub const fn read_only(&self) -> bool {
        self.read_only
    }
}

impl From<PmemConfigInput> for PmemConfig {
    fn from(input: PmemConfigInput) -> Self {
        Self {
            id: input.id,
            path_on_host: input.path_on_host,
            root_device: input.root_device,
            read_only: input.read_only,
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PmemConfigs {
    configs: Vec<PmemConfig>,
}

impl PmemConfigs {
    pub const fn new() -> Self {
        Self {
            configs: Vec::new(),
        }
    }

    pub fn as_slice(&self) -> &[PmemConfig] {
        &self.configs
    }

    pub fn upsert(&mut self, config: PmemConfig) {
        if let Some(existing) = self
            .configs
            .iter_mut()
            .find(|existing| existing.id == config.id)
        {
            *existing = config;
            return;
        }

        self.configs.push(config);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmemConfigError {
    UnsupportedRateLimiter,
}

impl fmt::Display for PmemConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRateLimiter => f.write_str("pmem rate_limiter is not supported"),
        }
    }
}

impl std::error::Error for PmemConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_defaults_to_firecracker_pmem_defaults() {
        let input = PmemConfigInput::new("pmem0", "/tmp/pmem.img");

        assert_eq!(input.id(), "pmem0");
        assert_eq!(input.path_on_host(), "/tmp/pmem.img");
        assert!(!input.root_device());
        assert!(!input.read_only());
        assert!(!input.rate_limiter_configured());
    }

    #[test]
    fn upsert_replaces_matching_id_without_mutating_others() {
        let mut configs = PmemConfigs::new();
        configs.upsert(PmemConfigInput::new("pmem0", "/tmp/old.img").into());
        configs.upsert(PmemConfigInput::new("pmem1", "/tmp/other.img").into());
        configs.upsert(
            PmemConfigInput::new("pmem0", "/tmp/new.img")
                .with_root_device(true)
                .with_read_only(true)
                .into(),
        );

        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].id(), "pmem0");
        assert_eq!(configs.as_slice()[0].path_on_host(), "/tmp/new.img");
        assert!(configs.as_slice()[0].root_device());
        assert!(configs.as_slice()[0].read_only());
        assert_eq!(configs.as_slice()[1].id(), "pmem1");
        assert_eq!(configs.as_slice()[1].path_on_host(), "/tmp/other.img");
    }
}
