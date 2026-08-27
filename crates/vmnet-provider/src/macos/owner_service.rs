use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use bangbang_session::vmnet_provider::{
    DataOwnerEvent, DataOwnerState, MAX_PROVIDER_TIMEOUT, ProviderCleanup, ProviderStatus,
    ProviderTerminalCode, VmnetDataOwner, VmnetProviderTransport,
};

use crate::broker::BrokerError;
use crate::owner::{DroppedOwner, OwnerBackend, OwnerError, PrivilegedOwner};
use crate::supervision::{OwnerBootstrap, OwnerSupervisionMessage};

use super::backend::{SystemCredentialOps, SystemOwnerBackend};
use super::transport::{POLL_INTERVAL, PollEvent, RecordError, RecordTransport, poll};

pub(super) fn run(supervision: UnixStream, data: UnixStream) -> Result<(), BrokerError> {
    let signals = OwnerSignals::install()?;
    let supervision = RecordTransport::new(supervision).map_err(map_record_error)?;
    let bootstrap = match supervision.receive_owner().map_err(map_record_error)? {
        OwnerSupervisionMessage::Bootstrap(bootstrap) => bootstrap,
        OwnerSupervisionMessage::Ready { .. }
        | OwnerSupervisionMessage::Failed { .. }
        | OwnerSupervisionMessage::Stop { .. }
        | OwnerSupervisionMessage::Final { .. } => return Err(BrokerError::Protocol),
    };
    let scope = bootstrap.scope();
    let privileged = match PrivilegedOwner::<SystemOwnerBackend>::start(bootstrap.policy()) {
        Ok(owner) => owner,
        Err(OwnerError::Start(failure)) => {
            supervision
                .send_owner(&OwnerSupervisionMessage::Failed {
                    scope,
                    status: failure.status(),
                    cleanup: failure.cleanup(),
                })
                .map_err(map_record_error)?;
            return Ok(());
        }
        Err(OwnerError::Credential { .. } | OwnerError::Backend(_)) => {
            return Err(BrokerError::Protocol);
        }
    };
    let mut credentials = SystemCredentialOps;
    let mut owner = match privileged.drop_credentials(&mut credentials, bootstrap.target()) {
        Ok(owner) => owner,
        Err(OwnerError::Credential { cleanup }) => {
            supervision
                .send_owner(&OwnerSupervisionMessage::Failed {
                    scope,
                    status: if cleanup == ProviderCleanup::Complete {
                        ProviderStatus::BackendFailure
                    } else {
                        ProviderStatus::CleanupUncertain
                    },
                    cleanup,
                })
                .map_err(map_record_error)?;
            return Ok(());
        }
        Err(OwnerError::Start(_) | OwnerError::Backend(_)) => {
            return Err(BrokerError::Protocol);
        }
    };

    supervision
        .send_owner(&OwnerSupervisionMessage::Ready {
            scope,
            parameters: owner.parameters(),
        })
        .map_err(map_record_error)?;
    drive_data(&signals, &supervision, data, bootstrap, &mut owner)
}

fn drive_data<B: OwnerBackend>(
    signals: &OwnerSignals,
    supervision: &RecordTransport,
    data: UnixStream,
    bootstrap: OwnerBootstrap,
    owner: &mut DroppedOwner<B>,
) -> Result<(), BrokerError> {
    let data_poll = data
        .try_clone()
        .map_err(|error| BrokerError::Io(error.kind()))?;
    let mut transport = VmnetProviderTransport::new(data, MAX_PROVIDER_TIMEOUT)
        .map_err(|_| BrokerError::Protocol)?;
    let mut state = VmnetDataOwner::new(
        bootstrap.scope().session(),
        bootstrap.scope().interface(),
        bootstrap.scope().generation(),
        owner.parameters(),
    )
    .map_err(|_| BrokerError::Protocol)?;
    let callback = CallbackWakeup::new()?;
    let mut callback_enabled = false;
    let mut readiness_outstanding = false;
    let mut deferred_readiness = 0_u16;

    loop {
        if signals.received() {
            return supervisor_stop(supervision, &mut transport, &mut state, owner, bootstrap);
        }
        let events = poll(
            &[
                data_poll.as_raw_fd(),
                supervision.poll_fd(),
                callback.poll_fd(),
            ],
            POLL_INTERVAL,
        )
        .map_err(map_record_error)?;
        let data_event = event(&events, 0)?;
        let supervisor_event = event(&events, 1)?;
        let callback_event = event(&events, 2)?;

        if supervisor_event.readable || supervisor_event.closed {
            match supervision.receive_owner() {
                Ok(OwnerSupervisionMessage::Stop { scope }) if scope == bootstrap.scope() => {
                    return supervisor_stop(
                        supervision,
                        &mut transport,
                        &mut state,
                        owner,
                        bootstrap,
                    );
                }
                Ok(
                    OwnerSupervisionMessage::Bootstrap(_)
                    | OwnerSupervisionMessage::Ready { .. }
                    | OwnerSupervisionMessage::Failed { .. }
                    | OwnerSupervisionMessage::Stop { .. }
                    | OwnerSupervisionMessage::Final { .. },
                ) => return cleanup_without_final(owner, BrokerError::Protocol),
                Err(RecordError::Disconnected) => {
                    let _ = owner.stop();
                    return Err(BrokerError::CleanupUncertain);
                }
                Err(error) => return cleanup_without_final(owner, map_record_error(error)),
            }
        }

        if callback_event.readable || callback_event.closed {
            deferred_readiness = deferred_readiness.max(callback.drain()?);
        }

        if data_event.readable || data_event.closed {
            let envelope = match transport.receive() {
                Ok(envelope) => envelope,
                Err(_) => {
                    return finish_owner(supervision, owner, bootstrap.scope());
                }
            };
            let event = match state.receive(envelope) {
                Ok(event) => event,
                Err(_) => {
                    let _ = state
                        .terminal(ProviderTerminalCode::Protocol)
                        .and_then(|frame| transport.send(frame));
                    return finish_owner(supervision, owner, bootstrap.scope());
                }
            };
            match event {
                DataOwnerEvent::Hello => {
                    transport
                        .send(state.hello_ack().map_err(|_| BrokerError::Protocol)?)
                        .map_err(|_| BrokerError::Protocol)?;
                    if !callback_enabled {
                        owner
                            .enable_readiness(callback.publisher())
                            .map_err(|_| BrokerError::CleanupUncertain)?;
                        callback_enabled = true;
                    }
                }
                DataOwnerEvent::Read { max_packets, .. } => {
                    readiness_outstanding = false;
                    match owner.read_packets(max_packets) {
                        Ok(packets) => transport
                            .send(
                                state
                                    .read_result(packets)
                                    .map_err(|_| BrokerError::Protocol)?,
                            )
                            .map_err(|_| BrokerError::Protocol)?,
                        Err(error) => {
                            send_operation_failure(&mut transport, &mut state, error)?;
                            return finish_owner(supervision, owner, bootstrap.scope());
                        }
                    }
                }
                DataOwnerEvent::Write { packets, .. } => match owner.write_packets(&packets) {
                    Ok(completed) => transport
                        .send(
                            state
                                .write_result(completed)
                                .map_err(|_| BrokerError::Protocol)?,
                        )
                        .map_err(|_| BrokerError::Protocol)?,
                    Err(error) => {
                        send_operation_failure(&mut transport, &mut state, error)?;
                        return finish_owner(supervision, owner, bootstrap.scope());
                    }
                },
                DataOwnerEvent::Stop => {
                    readiness_outstanding = false;
                    deferred_readiness = 0;
                    let cleanup = owner.stop();
                    transport
                        .send(state.stopped(cleanup).map_err(|_| BrokerError::Protocol)?)
                        .map_err(|_| BrokerError::Protocol)?;
                    if cleanup == ProviderCleanup::Uncertain {
                        return send_final(supervision, bootstrap.scope(), cleanup);
                    }
                }
                DataOwnerEvent::Shutdown => {
                    transport
                        .send(state.shutdown_ack().map_err(|_| BrokerError::Protocol)?)
                        .map_err(|_| BrokerError::Protocol)?;
                    return send_final(supervision, bootstrap.scope(), owner.stop());
                }
                DataOwnerEvent::PeerTerminal { .. } => {
                    return finish_owner(supervision, owner, bootstrap.scope());
                }
            }
        }

        if callback_enabled
            && deferred_readiness != 0
            && !readiness_outstanding
            && matches!(
                state.state(),
                DataOwnerState::Active | DataOwnerState::ReadyToRespond
            )
        {
            let estimate = deferred_readiness.min(owner.parameters().effective_read_max_packets());
            transport
                .send(
                    state
                        .readiness(estimate)
                        .map_err(|_| BrokerError::Protocol)?,
                )
                .map_err(|_| BrokerError::Protocol)?;
            deferred_readiness = 0;
            readiness_outstanding = true;
        }
    }
}

fn send_operation_failure(
    transport: &mut VmnetProviderTransport,
    state: &mut VmnetDataOwner,
    error: OwnerError,
) -> Result<(), BrokerError> {
    let OwnerError::Backend(status) = error else {
        return Err(BrokerError::Protocol);
    };
    transport
        .send(
            state
                .operation_failed(status)
                .map_err(|_| BrokerError::Protocol)?,
        )
        .map_err(|_| BrokerError::Protocol)
}

fn supervisor_stop<B: OwnerBackend>(
    supervision: &RecordTransport,
    transport: &mut VmnetProviderTransport,
    state: &mut VmnetDataOwner,
    owner: &mut DroppedOwner<B>,
    bootstrap: OwnerBootstrap,
) -> Result<(), BrokerError> {
    if !matches!(
        state.state(),
        DataOwnerState::Closed | DataOwnerState::Terminal
    ) {
        let _ = state
            .terminal(ProviderTerminalCode::Supervisor)
            .and_then(|frame| transport.send(frame));
    }
    finish_owner(supervision, owner, bootstrap.scope())
}

fn finish_owner<B: OwnerBackend>(
    supervision: &RecordTransport,
    owner: &mut DroppedOwner<B>,
    scope: crate::supervision::OwnerScope,
) -> Result<(), BrokerError> {
    send_final(supervision, scope, owner.stop())
}

fn send_final(
    supervision: &RecordTransport,
    scope: crate::supervision::OwnerScope,
    cleanup: ProviderCleanup,
) -> Result<(), BrokerError> {
    supervision
        .send_owner(&OwnerSupervisionMessage::Final { scope, cleanup })
        .map_err(map_record_error)
}

fn cleanup_without_final<B: OwnerBackend>(
    owner: &mut DroppedOwner<B>,
    error: BrokerError,
) -> Result<(), BrokerError> {
    let _ = owner.stop();
    Err(error)
}

fn event(events: &[PollEvent], index: usize) -> Result<PollEvent, BrokerError> {
    events.get(index).copied().ok_or(BrokerError::Protocol)
}

fn map_record_error(error: RecordError) -> BrokerError {
    match error {
        RecordError::Timeout => BrokerError::Timeout,
        RecordError::Disconnected | RecordError::Invalid => BrokerError::Protocol,
        RecordError::Io(kind) => BrokerError::Io(kind),
    }
}

struct CallbackWakeup {
    reader: UnixStream,
    estimate: Arc<AtomicU16>,
    publisher: crate::owner::OwnerReadinessCallback,
}

impl CallbackWakeup {
    fn new() -> Result<Self, BrokerError> {
        let (reader, writer) = UnixStream::pair().map_err(|error| BrokerError::Io(error.kind()))?;
        reader
            .set_nonblocking(true)
            .map_err(|error| BrokerError::Io(error.kind()))?;
        writer
            .set_nonblocking(true)
            .map_err(|error| BrokerError::Io(error.kind()))?;
        let estimate = Arc::new(AtomicU16::new(0));
        let publisher_estimate = Arc::clone(&estimate);
        let descriptor = writer.as_raw_fd();
        let publisher = Arc::new(move |value: u16| {
            publisher_estimate.fetch_max(value.max(1), Ordering::AcqRel);
            let byte = 1_u8;
            // SAFETY: The callback owns `writer`, keeping `descriptor` live;
            // the one-byte nonblocking send borrows no caller memory afterward.
            let _ = unsafe {
                libc::send(
                    descriptor,
                    (&raw const byte).cast(),
                    1,
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
            let _keep_alive = &writer;
        });
        Ok(Self {
            reader,
            estimate,
            publisher,
        })
    }

    fn publisher(&self) -> crate::owner::OwnerReadinessCallback {
        Arc::clone(&self.publisher)
    }

    fn poll_fd(&self) -> RawFd {
        self.reader.as_raw_fd()
    }

    fn drain(&self) -> Result<u16, BrokerError> {
        let mut bytes = [0_u8; 64];
        loop {
            // SAFETY: `bytes` is writable for the bounded nonblocking receive.
            let result = unsafe {
                libc::recv(
                    self.reader.as_raw_fd(),
                    bytes.as_mut_ptr().cast(),
                    bytes.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            if result > 0 {
                continue;
            }
            if result == 0 {
                return Err(BrokerError::Protocol);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                break;
            }
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(BrokerError::Io(error.kind()));
            }
        }
        Ok(self.estimate.swap(0, Ordering::AcqRel))
    }
}

impl std::fmt::Debug for CallbackWakeup {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CallbackWakeup(<redacted>)")
    }
}

struct OwnerSignals {
    received: Arc<AtomicBool>,
    registrations: [signal_hook::SigId; 2],
}

impl OwnerSignals {
    fn install() -> Result<Self, BrokerError> {
        let received = Arc::new(AtomicBool::new(false));
        let interrupt = signal_hook::flag::register(libc::SIGINT, Arc::clone(&received))
            .map_err(|error| BrokerError::Io(error.kind()))?;
        let terminate =
            signal_hook::flag::register(libc::SIGTERM, Arc::clone(&received)).map_err(|error| {
                signal_hook::low_level::unregister(interrupt);
                BrokerError::Io(error.kind())
            })?;
        Ok(Self {
            received,
            registrations: [interrupt, terminate],
        })
    }

    fn received(&self) -> bool {
        self.received.load(Ordering::Acquire)
    }
}

impl Drop for OwnerSignals {
    fn drop(&mut self) {
        for registration in self.registrations {
            signal_hook::low_level::unregister(registration);
        }
    }
}
