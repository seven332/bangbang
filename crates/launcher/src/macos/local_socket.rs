//! Exact no-follow launcher-side connection to one local Unix socket.

use std::ffi::CStr;
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use bangbang_session::ObjectIdentity;

#[cfg(feature = "elevated-bootstrap-probe")]
use super::vhost_user_broker::{
    NonblockingAnchoredConnect, PendingAnchoredConnect, anchored_socket_child_is_absent,
    begin_anchored_exact_nonblocking, finish_anchored_exact_nonblocking,
    validate_anchored_socket_child,
};
use super::vhost_user_broker::{ScopedConnectError, connect_anchored_exact};

/// Redacted outcome of one anchored local-socket connection.
pub(crate) type LocalSocketConnectError = ScopedConnectError;

/// One connected stream and its launcher-validated pathname identity.
pub(crate) struct ConnectedLocalSocket {
    stream: UnixStream,
    source_identity: ObjectIdentity,
}

/// Result of beginning one exact nonblocking local-socket connection.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) enum LocalSocketConnectStart {
    Connected(ConnectedLocalSocket),
    Pending(PendingLocalSocket),
}

/// In-progress exact local-socket connection retained by the event loop.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) struct PendingLocalSocket(PendingAnchoredConnect);

#[cfg(feature = "elevated-bootstrap-probe")]
impl std::fmt::Debug for PendingLocalSocket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PendingLocalSocket(<redacted>)")
    }
}

#[cfg(feature = "elevated-bootstrap-probe")]
impl PendingLocalSocket {
    pub(crate) fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }

    pub(crate) const fn source_identity(&self) -> ObjectIdentity {
        self.0.source_identity()
    }
}

impl std::fmt::Debug for ConnectedLocalSocket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectedLocalSocket")
            .field("stream", &"<owned>")
            .field("source_identity", &"<redacted>")
            .finish()
    }
}

impl ConnectedLocalSocket {
    pub(crate) const fn source_identity(&self) -> ObjectIdentity {
        self.source_identity
    }

    pub(crate) fn into_stream(self) -> UnixStream {
        self.stream
    }
}

/// Connects one exact child relative to a retained no-follow directory anchor.
pub(crate) fn connect_anchored(
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    name: &CStr,
    timeout: Duration,
) -> Result<ConnectedLocalSocket, LocalSocketConnectError> {
    connect_anchored_exact(anchor_descriptor, anchor_identity, name, timeout).map(
        |(stream, source_identity)| ConnectedLocalSocket {
            stream,
            source_identity,
        },
    )
}

/// Starts one exact nonblocking connection relative to the retained anchor.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) fn begin_connect_anchored(
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    name: &CStr,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
) -> Result<LocalSocketConnectStart, LocalSocketConnectError> {
    map_nonblocking(begin_anchored_exact_nonblocking(
        anchor_descriptor,
        anchor_identity,
        name,
        expected_uid,
        expected_gid,
        expected_mode,
    )?)
}

/// Completes one writable nonblocking anchored connection.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) fn finish_connect_anchored(
    pending: PendingLocalSocket,
) -> Result<LocalSocketConnectStart, LocalSocketConnectError> {
    map_nonblocking(finish_anchored_exact_nonblocking(pending.0)?)
}

/// Revalidates the exact socket child against its original identity.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) fn validate_anchored_child(
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    name: &CStr,
    expected_uid: u32,
    expected_gid: u32,
    expected_mode: u32,
    expected_identity: ObjectIdentity,
) -> Result<(), LocalSocketConnectError> {
    validate_anchored_socket_child(
        anchor_descriptor,
        anchor_identity,
        name,
        expected_uid,
        expected_gid,
        expected_mode,
        expected_identity,
    )
}

/// Returns whether the fixed child is absent beneath the retained exact anchor.
#[cfg(feature = "elevated-bootstrap-probe")]
pub(crate) fn anchored_child_is_absent(
    anchor_descriptor: RawFd,
    anchor_identity: ObjectIdentity,
    name: &CStr,
) -> Result<bool, LocalSocketConnectError> {
    anchored_socket_child_is_absent(anchor_descriptor, anchor_identity, name)
}

#[cfg(feature = "elevated-bootstrap-probe")]
fn map_nonblocking(
    connection: NonblockingAnchoredConnect,
) -> Result<LocalSocketConnectStart, LocalSocketConnectError> {
    Ok(match connection {
        NonblockingAnchoredConnect::Connected(stream, source_identity) => {
            LocalSocketConnectStart::Connected(ConnectedLocalSocket {
                stream,
                source_identity,
            })
        }
        NonblockingAnchoredConnect::Pending(pending) => {
            LocalSocketConnectStart::Pending(PendingLocalSocket(pending))
        }
    })
}
