//! Vhost-user framing over the shared deadline-bounded Unix stream transport.

use std::fmt;
use std::io;
use std::os::fd::BorrowedFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use bangbang_unix_stream::{UnixStreamTransport, UnixStreamTransportError};

use crate::error::VhostUserError;
use crate::message::{HEADER_BYTES, Header, Request, frame};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransportError {
    Invalid,
    Timeout,
    Disconnected,
    Io(io::ErrorKind),
}

impl From<UnixStreamTransportError> for TransportError {
    fn from(error: UnixStreamTransportError) -> Self {
        match error {
            UnixStreamTransportError::Invalid => Self::Invalid,
            UnixStreamTransportError::Timeout => Self::Timeout,
            UnixStreamTransportError::Disconnected | UnixStreamTransportError::UnexpectedEof => {
                Self::Disconnected
            }
            UnixStreamTransportError::Io(kind) => Self::Io(kind),
        }
    }
}

impl From<TransportError> for VhostUserError {
    fn from(error: TransportError) -> Self {
        match error {
            TransportError::Invalid => Self::InvalidMessage,
            TransportError::Timeout => Self::Timeout,
            TransportError::Disconnected => Self::Disconnected,
            TransportError::Io(kind) => Self::Io(kind),
        }
    }
}

pub(crate) struct Transport {
    inner: UnixStreamTransport,
}

impl fmt::Debug for Transport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Transport")
            .field("stream", &"<redacted>")
            .field("timeout", &"<configured>")
            .finish()
    }
}

impl Transport {
    pub(crate) fn new(stream: UnixStream, timeout: Duration) -> Result<Self, VhostUserError> {
        let inner = UnixStreamTransport::new(stream, timeout)
            .map_err(TransportError::from)
            .map_err(VhostUserError::from)?;
        Ok(Self { inner })
    }

    pub(crate) fn send(
        &self,
        request: Request,
        body: &[u8],
        descriptors: &[BorrowedFd<'_>],
        need_reply: bool,
    ) -> Result<(), TransportError> {
        let encoded = frame(request, body, need_reply).map_err(|_| TransportError::Invalid)?;
        self.inner.send(&encoded, descriptors).map_err(Into::into)
    }

    pub(crate) fn request_reply(
        &self,
        request: Request,
        body: &[u8],
        descriptors: &[BorrowedFd<'_>],
        need_reply: bool,
    ) -> Result<Vec<u8>, TransportError> {
        let deadline = self.inner.deadline().map_err(TransportError::from)?;
        let encoded = frame(request, body, need_reply).map_err(|_| TransportError::Invalid)?;
        self.inner
            .send_until(&encoded, descriptors, deadline)
            .map_err(TransportError::from)?;
        self.receive_reply(request, deadline)
    }

    pub(crate) fn shutdown(&self) {
        self.inner.shutdown();
    }

    fn receive_reply(
        &self,
        expected_request: Request,
        deadline: bangbang_unix_stream::UnixStreamDeadline,
    ) -> Result<Vec<u8>, TransportError> {
        let header = self
            .inner
            .receive_exact_until(HEADER_BYTES, 0, deadline)
            .map_err(TransportError::from)?;
        let header = Header::decode(header.bytes()).map_err(|_| TransportError::Invalid)?;
        if !header.is_reply || header.need_reply || header.request != expected_request {
            return Err(TransportError::Invalid);
        }
        let body = self
            .inner
            .receive_exact_until(header.body_size, 0, deadline)
            .map_err(TransportError::from)?;
        Ok(body.into_parts().0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnected_socket_is_a_typed_failure() {
        let (sender, receiver) = UnixStream::pair().expect("stream pair should open");
        let transport = Transport::new(sender, Duration::from_millis(100))
            .expect("transport should initialize");
        drop(receiver);
        assert!(matches!(
            transport.send(Request::SetOwner, &[], &[], false),
            Err(TransportError::Disconnected) | Err(TransportError::Io(io::ErrorKind::BrokenPipe))
        ));
    }
}
