use std::fmt;
use std::io;

/// A redacted vhost-user frontend failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VhostUserError {
    /// Client construction or a method parameter was invalid.
    InvalidConfiguration,
    /// The requested operation was not legal in the current protocol state.
    InvalidState,
    /// A feature was unknown, unavailable, or outside the reviewed subset.
    UnsupportedFeature,
    /// The peer sent a malformed or mismatched protocol message.
    InvalidMessage,
    /// The backend returned a nonzero acknowledgement or config failure.
    BackendFailure,
    /// The complete operation exceeded its absolute deadline.
    Timeout,
    /// The backend disconnected before the operation completed.
    Disconnected,
    /// A local descriptor or socket operation failed.
    Io(io::ErrorKind),
    /// An earlier stream failure made subsequent framing unknowable.
    Poisoned,
}

impl fmt::Display for VhostUserError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidConfiguration => "invalid vhost-user frontend configuration",
            Self::InvalidState => "invalid vhost-user frontend state",
            Self::UnsupportedFeature => "unsupported vhost-user feature",
            Self::InvalidMessage => "invalid vhost-user peer message",
            Self::BackendFailure => "vhost-user backend rejected an operation",
            Self::Timeout => "vhost-user operation timed out",
            Self::Disconnected => "vhost-user backend disconnected",
            Self::Io(_) => "vhost-user transport operation failed",
            Self::Poisoned => "vhost-user frontend is terminal",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for VhostUserError {}

/// A redacted queue-notifier failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VhostUserNotifierError {
    /// A descriptor operation failed.
    Io(io::ErrorKind),
    /// A pipe transferred a non-integral eight-byte notification.
    InvalidNotification,
}

impl fmt::Display for VhostUserNotifierError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => formatter.write_str("vhost-user notifier operation failed"),
            Self::InvalidNotification => formatter.write_str("invalid vhost-user notifier payload"),
        }
    }
}

impl std::error::Error for VhostUserNotifierError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_text_is_fixed_and_value_redacted() {
        let frontend = VhostUserError::Io(io::ErrorKind::PermissionDenied);
        assert_eq!(
            frontend.to_string(),
            "vhost-user transport operation failed"
        );
        assert!(!frontend.to_string().contains("PermissionDenied"));

        let notifier = VhostUserNotifierError::Io(io::ErrorKind::BrokenPipe);
        assert_eq!(notifier.to_string(), "vhost-user notifier operation failed");
        assert!(!notifier.to_string().contains("BrokenPipe"));
    }
}
