//! Exact no-follow launcher-side connection to one local Unix socket.

use std::ffi::CStr;
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use bangbang_session::ObjectIdentity;

use super::vhost_user_broker::{ScopedConnectError, connect_anchored_exact};

/// Redacted outcome of one anchored local-socket connection.
pub(crate) type LocalSocketConnectError = ScopedConnectError;

/// One connected stream and its launcher-validated pathname identity.
pub(crate) struct ConnectedLocalSocket {
    stream: UnixStream,
    source_identity: ObjectIdentity,
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
