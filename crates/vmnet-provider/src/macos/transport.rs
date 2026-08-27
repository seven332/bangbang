use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use bangbang_unix_stream::{UnixStreamTransport, UnixStreamTransportError};

use crate::bootstrap::{BROKER_BOOTSTRAP_BYTES, BrokerBootstrap};
use crate::supervision::{OWNER_SUPERVISION_BYTES, OwnerSupervisionMessage};

pub(super) const RECORD_TIMEOUT: Duration = Duration::from_secs(6);
pub(super) const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecordError {
    Timeout,
    Disconnected,
    Invalid,
    Io(io::ErrorKind),
}

impl From<UnixStreamTransportError> for RecordError {
    fn from(error: UnixStreamTransportError) -> Self {
        match error {
            UnixStreamTransportError::Timeout => Self::Timeout,
            UnixStreamTransportError::Disconnected | UnixStreamTransportError::UnexpectedEof => {
                Self::Disconnected
            }
            UnixStreamTransportError::Invalid => Self::Invalid,
            UnixStreamTransportError::Io(kind) => Self::Io(kind),
        }
    }
}

pub(super) struct RecordTransport {
    inner: UnixStreamTransport,
    poll: UnixStream,
}

impl RecordTransport {
    pub(super) fn new(stream: UnixStream) -> Result<Self, RecordError> {
        let poll = stream
            .try_clone()
            .map_err(|error| RecordError::Io(error.kind()))?;
        let inner = UnixStreamTransport::new(stream, RECORD_TIMEOUT).map_err(RecordError::from)?;
        Ok(Self { inner, poll })
    }

    pub(super) fn poll_fd(&self) -> RawFd {
        self.poll.as_raw_fd()
    }

    pub(super) fn receive_broker_bootstrap(&self) -> Result<BrokerBootstrap, RecordError> {
        let received = self
            .inner
            .receive_exact(BROKER_BOOTSTRAP_BYTES, 0)
            .map_err(RecordError::from)?;
        if received.descriptor_count() != 0 {
            return Err(RecordError::Invalid);
        }
        BrokerBootstrap::decode(received.bytes()).map_err(|_| RecordError::Invalid)
    }

    pub(super) fn send_owner(&self, message: &OwnerSupervisionMessage) -> Result<(), RecordError> {
        self.inner
            .send(&message.encode(), &[])
            .map_err(RecordError::from)
    }

    pub(super) fn receive_owner(&self) -> Result<OwnerSupervisionMessage, RecordError> {
        let received = self
            .inner
            .receive_exact(OWNER_SUPERVISION_BYTES, 0)
            .map_err(RecordError::from)?;
        if received.descriptor_count() != 0 {
            return Err(RecordError::Invalid);
        }
        OwnerSupervisionMessage::decode(received.bytes()).map_err(|_| RecordError::Invalid)
    }

    pub(super) fn shutdown(&self) {
        self.inner.shutdown();
    }
}

impl std::fmt::Debug for RecordTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RecordTransport(<redacted>)")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct PollEvent {
    pub(super) readable: bool,
    pub(super) closed: bool,
}

pub(super) fn poll(
    descriptors: &[RawFd],
    timeout: Duration,
) -> Result<Vec<PollEvent>, RecordError> {
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(descriptors.len())
        .map_err(|_| RecordError::Invalid)?;
    entries.extend(descriptors.iter().copied().map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }));
    loop {
        // SAFETY: `entries` is an initialized writable pollfd array for the
        // bounded synchronous poll call.
        let result = unsafe {
            libc::poll(
                entries.as_mut_ptr(),
                libc::nfds_t::try_from(entries.len()).map_err(|_| RecordError::Invalid)?,
                duration_to_poll_timeout(timeout),
            )
        };
        if result >= 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(RecordError::Io(error.kind()));
        }
    }
    Ok(entries
        .iter()
        .map(|entry| PollEvent {
            readable: entry.revents & libc::POLLIN != 0,
            closed: entry.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0,
        })
        .collect())
}

fn duration_to_poll_timeout(duration: Duration) -> libc::c_int {
    let millis = duration
        .as_nanos()
        .saturating_add(999_999)
        .checked_div(1_000_000)
        .unwrap_or(u128::MAX)
        .min(i32::MAX as u128);
    i32::try_from(millis).unwrap_or(i32::MAX)
}
