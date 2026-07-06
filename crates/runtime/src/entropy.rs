use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntropyConfigInput {
    rate_limiter_configured: bool,
}

impl EntropyConfigInput {
    pub const fn new() -> Self {
        Self {
            rate_limiter_configured: false,
        }
    }

    pub const fn with_rate_limiter_configured(mut self) -> Self {
        self.rate_limiter_configured = true;
        self
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
    }
}

impl Default for EntropyConfigInput {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntropyConfigError {
    UnsupportedRateLimiter,
}

impl fmt::Display for EntropyConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedRateLimiter => f.write_str("entropy rate_limiter is not supported"),
        }
    }
}

impl std::error::Error for EntropyConfigError {}
