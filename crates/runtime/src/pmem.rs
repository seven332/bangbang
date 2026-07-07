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

impl TryFrom<PmemConfigInput> for PmemConfig {
    type Error = PmemConfigError;

    fn try_from(input: PmemConfigInput) -> Result<Self, Self::Error> {
        validate_pmem_id(&input.id)?;

        if input.path_on_host.is_empty() {
            return Err(PmemConfigError::EmptyPathOnHost);
        }

        if input.rate_limiter_configured {
            return Err(PmemConfigError::UnsupportedRateLimiter);
        }

        Ok(Self {
            id: input.id,
            path_on_host: input.path_on_host,
            root_device: input.root_device,
            read_only: input.read_only,
        })
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
    EmptyPmemId,
    InvalidPmemId,
    EmptyPathOnHost,
    UnsupportedRateLimiter,
}

impl fmt::Display for PmemConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPmemId => f.write_str("pmem id must not be empty"),
            Self::InvalidPmemId => {
                f.write_str("pmem id must contain only alphanumeric characters or '_'")
            }
            Self::EmptyPathOnHost => f.write_str("pmem path_on_host must not be empty"),
            Self::UnsupportedRateLimiter => f.write_str("pmem rate_limiter is not supported"),
        }
    }
}

impl std::error::Error for PmemConfigError {}

fn validate_pmem_id(id: &str) -> Result<(), PmemConfigError> {
    if id.is_empty() {
        return Err(PmemConfigError::EmptyPmemId);
    }

    if !id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(PmemConfigError::InvalidPmemId);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pmem_config(input: PmemConfigInput) -> PmemConfig {
        input.try_into().expect("pmem input should validate")
    }

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
    fn config_accepts_firecracker_id_character_set() {
        let config = pmem_config(PmemConfigInput::new("pmem_\u{00e9}1", "/tmp/pmem.img"));

        assert_eq!(config.id(), "pmem_\u{00e9}1");
    }

    #[test]
    fn config_rejects_empty_pmem_id() {
        let err = PmemConfig::try_from(PmemConfigInput::new("", "/tmp/pmem.img"))
            .expect_err("empty pmem id should fail");

        assert_eq!(err, PmemConfigError::EmptyPmemId);
        assert_eq!(err.to_string(), "pmem id must not be empty");
    }

    #[test]
    fn config_rejects_invalid_pmem_id_without_echoing_it() {
        let invalid = "bad/id\nsecret";
        let err = PmemConfig::try_from(PmemConfigInput::new(invalid, "/tmp/pmem.img"))
            .expect_err("invalid pmem id should fail");

        assert_eq!(err, PmemConfigError::InvalidPmemId);
        assert_eq!(
            err.to_string(),
            "pmem id must contain only alphanumeric characters or '_'"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn config_rejects_empty_path_on_host() {
        let err = PmemConfig::try_from(PmemConfigInput::new("pmem0", ""))
            .expect_err("empty pmem path should fail");

        assert_eq!(err, PmemConfigError::EmptyPathOnHost);
        assert_eq!(err.to_string(), "pmem path_on_host must not be empty");
    }

    #[test]
    fn upsert_replaces_matching_id_without_mutating_others() {
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new("pmem0", "/tmp/old.img")));
        configs.upsert(pmem_config(PmemConfigInput::new("pmem1", "/tmp/other.img")));
        configs.upsert(pmem_config(
            PmemConfigInput::new("pmem0", "/tmp/new.img")
                .with_root_device(true)
                .with_read_only(true),
        ));

        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].id(), "pmem0");
        assert_eq!(configs.as_slice()[0].path_on_host(), "/tmp/new.img");
        assert!(configs.as_slice()[0].root_device());
        assert!(configs.as_slice()[0].read_only());
        assert_eq!(configs.as_slice()[1].id(), "pmem1");
        assert_eq!(configs.as_slice()[1].path_on_host(), "/tmp/other.img");
    }
}
