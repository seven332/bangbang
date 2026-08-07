//! Firecracker-shaped CPU-template dump and verification support.

pub mod cli;
pub mod document;
pub mod fingerprint;
pub mod host;
pub mod input;
pub mod profile;
pub mod projection;
pub mod provider;
pub mod publication;
pub mod strip;
pub mod strip_publication;

/// Maximum accepted config, template, or emitted template document size.
pub const CPU_TEMPLATE_DOCUMENT_MAX_BYTES: usize = 1024 * 1024;

/// Stable process-exit class reserved for the future public helper binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperExitClass {
    Success,
    OperationalFailure,
    InvocationFailure,
}

impl HelperExitClass {
    /// Return the stable process exit code.
    pub const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::OperationalFailure => 1,
            Self::InvocationFailure => 2,
        }
    }
}
