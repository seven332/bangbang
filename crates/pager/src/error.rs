use std::fmt;
use std::io;

/// Stable, value-redacted pager protocol failure.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PagerError {
    /// The operating system could not provide a random session identity.
    Randomness,
    /// A local protocol limit, region, or transport configuration was invalid.
    InvalidConfiguration,
    /// The peer supplied malformed, unknown, or non-canonical bytes.
    InvalidFrame,
    /// The peer supplied a mismatched session, request, region, or generation.
    InvalidPeerState,
    /// The requested local or remote transition was not legal in this phase.
    InvalidLifecycle,
    /// A negotiated or global protocol bound was exceeded.
    LimitExceeded,
    /// The peer closed after transferring only part of a frame.
    UnexpectedEof,
    /// The complete frame operation exceeded its absolute deadline.
    Timeout,
    /// The peer disconnected between complete frames.
    Disconnected,
    /// A local socket operation failed.
    Io(io::ErrorKind),
    /// An earlier stream failure made subsequent framing unknowable.
    Poisoned,
}

impl fmt::Debug for PagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Randomness => "PagerError::Randomness",
            Self::InvalidConfiguration => "PagerError::InvalidConfiguration",
            Self::InvalidFrame => "PagerError::InvalidFrame",
            Self::InvalidPeerState => "PagerError::InvalidPeerState",
            Self::InvalidLifecycle => "PagerError::InvalidLifecycle",
            Self::LimitExceeded => "PagerError::LimitExceeded",
            Self::UnexpectedEof => "PagerError::UnexpectedEof",
            Self::Timeout => "PagerError::Timeout",
            Self::Disconnected => "PagerError::Disconnected",
            Self::Io(_) => "PagerError::Io(<redacted>)",
            Self::Poisoned => "PagerError::Poisoned",
        })
    }
}

impl fmt::Display for PagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Randomness => "snapshot pager session identity unavailable",
            Self::InvalidConfiguration => "invalid snapshot pager configuration",
            Self::InvalidFrame => "invalid snapshot pager peer frame",
            Self::InvalidPeerState => "mismatched snapshot pager peer state",
            Self::InvalidLifecycle => "invalid snapshot pager lifecycle transition",
            Self::LimitExceeded => "snapshot pager protocol limit exceeded",
            Self::UnexpectedEof => "snapshot pager peer truncated a frame",
            Self::Timeout => "snapshot pager operation timed out",
            Self::Disconnected => "snapshot pager peer disconnected",
            Self::Io(_) => "snapshot pager transport operation failed",
            Self::Poisoned => "snapshot pager transport is terminal",
        })
    }
}

impl std::error::Error for PagerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_text_and_debug_are_value_redacted() {
        let error = PagerError::Io(io::ErrorKind::PermissionDenied);
        assert_eq!(
            error.to_string(),
            "snapshot pager transport operation failed"
        );
        assert_eq!(format!("{error:?}"), "PagerError::Io(<redacted>)");
        assert!(!format!("{error:?}").contains("PermissionDenied"));
    }
}
