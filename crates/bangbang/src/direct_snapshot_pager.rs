//! Bounded direct-process connection to an operator-selected snapshot pager.

use std::fmt;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use crate::direct_vhost_user::{self, DirectVhostUserConnectError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectSnapshotPagerConnectError {
    InvalidPath,
    Timeout,
    Refused,
    Io(io::ErrorKind),
}

impl fmt::Display for DirectSnapshotPagerConnectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath => formatter.write_str("snapshot pager socket path is invalid"),
            Self::Timeout => formatter.write_str("snapshot pager socket connection timed out"),
            Self::Refused => formatter.write_str("snapshot pager socket connection was refused"),
            Self::Io(_) => formatter.write_str("snapshot pager socket connection failed"),
        }
    }
}

impl std::error::Error for DirectSnapshotPagerConnectError {}

pub(crate) fn validate_path(path: &Path) -> Result<(), DirectSnapshotPagerConnectError> {
    direct_vhost_user::validate_path(path).map_err(DirectSnapshotPagerConnectError::from)
}

pub(crate) fn connect(
    path: &Path,
    timeout: Duration,
) -> Result<UnixStream, DirectSnapshotPagerConnectError> {
    let stream =
        direct_vhost_user::connect(path, timeout).map_err(DirectSnapshotPagerConnectError::from)?;
    stream
        .set_nonblocking(false)
        .map_err(|source| DirectSnapshotPagerConnectError::Io(source.kind()))?;
    Ok(stream)
}

impl From<DirectVhostUserConnectError> for DirectSnapshotPagerConnectError {
    fn from(source: DirectVhostUserConnectError) -> Self {
        match source {
            DirectVhostUserConnectError::InvalidPath => Self::InvalidPath,
            DirectVhostUserConnectError::Timeout => Self::Timeout,
            DirectVhostUserConnectError::Refused => Self::Refused,
            DirectVhostUserConnectError::Io(kind) => Self::Io(kind),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn pager_connector_rejects_invalid_paths_before_socket_creation() {
        assert_eq!(
            validate_path(Path::new("")),
            Err(DirectSnapshotPagerConnectError::InvalidPath)
        );
        assert_eq!(
            DirectSnapshotPagerConnectError::InvalidPath.to_string(),
            "snapshot pager socket path is invalid"
        );
    }

    #[test]
    fn pager_connector_errors_never_render_the_selected_path() {
        let path = "/private/operator-selected/pager.sock";
        for error in [
            DirectSnapshotPagerConnectError::InvalidPath,
            DirectSnapshotPagerConnectError::Timeout,
            DirectSnapshotPagerConnectError::Refused,
            DirectSnapshotPagerConnectError::Io(io::ErrorKind::PermissionDenied),
        ] {
            assert!(!error.to_string().contains(path));
            assert!(!format!("{error:?}").contains(path));
        }
    }
}
