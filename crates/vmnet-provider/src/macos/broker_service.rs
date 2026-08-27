use std::collections::BTreeMap;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use bangbang_session::vmnet_provider::{
    ControlBrokerEvent, MAX_PROVIDER_TIMEOUT, ProviderCleanup, ProviderStatus,
    ProviderTerminalCode, VmnetControlBroker, VmnetProviderError, VmnetProviderTransport,
};

use crate::broker::{BrokerError, BrokerLedger};
use crate::supervision::{OwnerScope, OwnerSupervisionMessage};

use super::process::{OwnedChild, PinnedExecutable, spawn_owner};
use super::transport::{POLL_INTERVAL, PollEvent, RecordError, RecordTransport, poll};

pub(super) fn run(bootstrap: UnixStream, control: UnixStream) -> Result<(), BrokerError> {
    let signals = BrokerSignals::install()?;
    let bootstrap_transport = RecordTransport::new(bootstrap).map_err(map_record_error)?;
    let bootstrap = bootstrap_transport
        .receive_broker_bootstrap()
        .map_err(map_record_error)?;
    bootstrap_transport.shutdown();

    let executable = PinnedExecutable::current()?;
    let control_poll = control
        .try_clone()
        .map_err(|error| BrokerError::Io(error.kind()))?;
    let mut transport = VmnetProviderTransport::new(control, MAX_PROVIDER_TIMEOUT)
        .map_err(|_| BrokerError::Protocol)?;
    let mut state =
        VmnetControlBroker::new(bootstrap.session()).map_err(|_| BrokerError::Protocol)?;
    let mut ledger = BrokerLedger::new(bootstrap)?;
    let mut owners = BTreeMap::new();

    let result = (|| -> Result<(), BrokerError> {
        loop {
            if signals.received() {
                let cleanup = cleanup_all(&mut owners, &mut ledger);
                return if cleanup == ProviderCleanup::Complete {
                    Ok(())
                } else {
                    Err(BrokerError::CleanupUncertain)
                };
            }

            let keys = owners
                .iter()
                .filter_map(|(interface, owner)| {
                    owner.final_cleanup.is_none().then_some(*interface)
                })
                .collect::<Vec<_>>();
            let mut descriptors = Vec::new();
            descriptors
                .try_reserve_exact(keys.len().saturating_add(1))
                .map_err(|_| BrokerError::InvalidConfiguration)?;
            descriptors.push(control_poll.as_raw_fd());
            for key in &keys {
                descriptors.push(
                    owners
                        .get(key)
                        .ok_or(BrokerError::Protocol)?
                        .supervision
                        .poll_fd(),
                );
            }
            let events = poll(&descriptors, POLL_INTERVAL).map_err(map_record_error)?;

            for (index, key) in keys.iter().copied().enumerate() {
                let event = event(&events, index + 1)?;
                if (event.readable || event.closed)
                    && observe_owner_final(key, &mut owners, &mut ledger).is_err()
                {
                    let _ =
                        send_terminal(&mut transport, &mut state, ProviderTerminalCode::Cleanup);
                    let _ = cleanup_all(&mut owners, &mut ledger);
                    return Err(BrokerError::CleanupUncertain);
                }
            }

            for key in &keys {
                let exited_without_observed_final = {
                    let owner = owners.get_mut(key).ok_or(BrokerError::Protocol)?;
                    owner.final_cleanup.is_none() && owner.child.try_wait()?.is_some()
                };
                if exited_without_observed_final
                    && observe_owner_final(*key, &mut owners, &mut ledger).is_err()
                {
                    let _ =
                        send_terminal(&mut transport, &mut state, ProviderTerminalCode::Cleanup);
                    let _ = cleanup_all(&mut owners, &mut ledger);
                    return Err(BrokerError::CleanupUncertain);
                }
            }

            let control_event = event(&events, 0)?;
            if !control_event.readable && !control_event.closed {
                continue;
            }
            let envelope = match transport.receive() {
                Ok(envelope) => envelope,
                Err(VmnetProviderError::Disconnected | VmnetProviderError::UnexpectedEof) => {
                    return if cleanup_all(&mut owners, &mut ledger) == ProviderCleanup::Complete {
                        Ok(())
                    } else {
                        Err(BrokerError::CleanupUncertain)
                    };
                }
                Err(_) => {
                    let _ = cleanup_all(&mut owners, &mut ledger);
                    return Err(BrokerError::Protocol);
                }
            };
            let control = match state.receive(envelope) {
                Ok(event) => event,
                Err(_) => {
                    let _ =
                        send_terminal(&mut transport, &mut state, ProviderTerminalCode::Protocol);
                    let _ = cleanup_all(&mut owners, &mut ledger);
                    return Err(BrokerError::Protocol);
                }
            };
            match control {
                ControlBrokerEvent::Hello => transport
                    .send(state.hello_ack().map_err(|_| BrokerError::Protocol)?)
                    .map_err(|_| BrokerError::Protocol)?,
                ControlBrokerEvent::Start {
                    interface,
                    policy_slot,
                    requested,
                } => {
                    StartContext {
                        executable: &executable,
                        owners: &mut owners,
                        ledger: &mut ledger,
                        transport: &mut transport,
                        state: &mut state,
                    }
                    .handle(interface, policy_slot, requested)?;
                }
                ControlBrokerEvent::Stop {
                    interface,
                    generation,
                } => {
                    let scope = OwnerScope::new(bootstrap.session(), interface, generation)
                        .map_err(|_| BrokerError::Protocol)?;
                    let cleanup = stop_one(scope, &mut owners, &mut ledger);
                    let response = state.stopped(cleanup).map_err(|_| BrokerError::Protocol)?;
                    transport
                        .send(response)
                        .map_err(|_| BrokerError::Protocol)?;
                    if cleanup == ProviderCleanup::Uncertain {
                        return Err(BrokerError::CleanupUncertain);
                    }
                }
                ControlBrokerEvent::Cancel { .. } => {
                    let cleanup = cleanup_all(&mut owners, &mut ledger);
                    transport
                        .send(
                            state
                                .cancelled(cleanup)
                                .map_err(|_| BrokerError::Protocol)?,
                        )
                        .map_err(|_| BrokerError::Protocol)?;
                    return if cleanup == ProviderCleanup::Complete {
                        Ok(())
                    } else {
                        Err(BrokerError::CleanupUncertain)
                    };
                }
                ControlBrokerEvent::Shutdown => {
                    if !owners.is_empty() || !ledger.is_empty() {
                        return Err(BrokerError::Protocol);
                    }
                    transport
                        .send(state.shutdown_ack().map_err(|_| BrokerError::Protocol)?)
                        .map_err(|_| BrokerError::Protocol)?;
                    return Ok(());
                }
                ControlBrokerEvent::PeerTerminal { .. } => {
                    let cleanup = cleanup_all(&mut owners, &mut ledger);
                    return if cleanup == ProviderCleanup::Complete {
                        Ok(())
                    } else {
                        Err(BrokerError::CleanupUncertain)
                    };
                }
            }
        }
    })();
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            if cleanup_all(&mut owners, &mut ledger) == ProviderCleanup::Complete {
                Err(error)
            } else {
                Err(BrokerError::CleanupUncertain)
            }
        }
    }
}

struct StartContext<'a> {
    executable: &'a PinnedExecutable,
    owners: &'a mut BTreeMap<bangbang_session::vmnet_provider::VmnetInterfaceId, MacOwner>,
    ledger: &'a mut BrokerLedger,
    transport: &'a mut VmnetProviderTransport,
    state: &'a mut VmnetControlBroker,
}

impl StartContext<'_> {
    fn handle(
        &mut self,
        interface: bangbang_session::vmnet_provider::VmnetInterfaceId,
        slot: bangbang_session::vmnet_provider::VmnetPolicySlot,
        requested: bangbang_session::vmnet_provider::RequestedVmnetParameters,
    ) -> Result<(), BrokerError> {
        let bootstrap = match self.ledger.reserve_start(interface, slot, requested) {
            Ok(bootstrap) => bootstrap,
            Err(status) => {
                self.transport
                    .send(
                        self.state
                            .start_failed(status)
                            .map_err(|_| BrokerError::Protocol)?,
                    )
                    .map_err(|_| BrokerError::Protocol)?;
                return Ok(());
            }
        };
        let scope = bootstrap.scope();
        let spawned = match spawn_owner(self.executable) {
            Ok(spawned) => spawned,
            Err(BrokerError::CleanupUncertain) => {
                self.ledger.fail_start(scope, ProviderCleanup::Uncertain)?;
                self.transport
                    .send(
                        self.state
                            .start_failed(ProviderStatus::CleanupUncertain)
                            .map_err(|_| BrokerError::Protocol)?,
                    )
                    .map_err(|_| BrokerError::Protocol)?;
                let _ = send_terminal(self.transport, self.state, ProviderTerminalCode::Cleanup);
                return Err(BrokerError::CleanupUncertain);
            }
            Err(_) => {
                self.ledger.fail_start(scope, ProviderCleanup::Complete)?;
                self.transport
                    .send(
                        self.state
                            .start_failed(ProviderStatus::BackendFailure)
                            .map_err(|_| BrokerError::Protocol)?,
                    )
                    .map_err(|_| BrokerError::Protocol)?;
                return Ok(());
            }
        };
        let mut owner = MacOwner::new(scope, spawned)?;
        if owner
            .supervision
            .send_owner(&OwnerSupervisionMessage::Bootstrap(bootstrap))
            .is_err()
        {
            let _ = owner.child.terminate_and_reap();
            self.ledger.fail_start(scope, ProviderCleanup::Uncertain)?;
            self.transport
                .send(
                    self.state
                        .start_failed(ProviderStatus::CleanupUncertain)
                        .map_err(|_| BrokerError::Protocol)?,
                )
                .map_err(|_| BrokerError::Protocol)?;
            let _ = send_terminal(self.transport, self.state, ProviderTerminalCode::Cleanup);
            return Err(BrokerError::CleanupUncertain);
        }
        let initial = match owner.supervision.receive_owner() {
            Ok(message) => message,
            Err(_) => {
                let _ = owner.child.terminate_and_reap();
                self.ledger.fail_start(scope, ProviderCleanup::Uncertain)?;
                self.transport
                    .send(
                        self.state
                            .start_failed(ProviderStatus::CleanupUncertain)
                            .map_err(|_| BrokerError::Protocol)?,
                    )
                    .map_err(|_| BrokerError::Protocol)?;
                let _ = send_terminal(self.transport, self.state, ProviderTerminalCode::Cleanup);
                return Err(BrokerError::CleanupUncertain);
            }
        };
        match initial {
            OwnerSupervisionMessage::Ready {
                scope: ready_scope,
                parameters,
            } if ready_scope == scope => {
                self.ledger.mark_ready(scope)?;
                let client_data = owner.client_data.take().ok_or(BrokerError::Protocol)?;
                self.transport
                    .send(
                        self.state
                            .started(scope.generation(), parameters, client_data)
                            .map_err(|_| BrokerError::Protocol)?,
                    )
                    .map_err(|_| BrokerError::Protocol)?;
                self.owners.insert(interface, owner);
                Ok(())
            }
            OwnerSupervisionMessage::Failed {
                scope: failed_scope,
                status,
                cleanup,
            } if failed_scope == scope => {
                let reaped = owner.child.reap_clean().is_ok();
                let cleanup = if reaped {
                    cleanup
                } else {
                    ProviderCleanup::Uncertain
                };
                self.ledger.fail_start(scope, cleanup)?;
                self.transport
                    .send(
                        self.state
                            .start_failed(status)
                            .map_err(|_| BrokerError::Protocol)?,
                    )
                    .map_err(|_| BrokerError::Protocol)?;
                if cleanup == ProviderCleanup::Uncertain {
                    let _ =
                        send_terminal(self.transport, self.state, ProviderTerminalCode::Cleanup);
                    Err(BrokerError::CleanupUncertain)
                } else {
                    Ok(())
                }
            }
            OwnerSupervisionMessage::Bootstrap(_)
            | OwnerSupervisionMessage::Ready { .. }
            | OwnerSupervisionMessage::Failed { .. }
            | OwnerSupervisionMessage::Stop { .. }
            | OwnerSupervisionMessage::Final { .. } => {
                let _ = owner.child.terminate_and_reap();
                self.ledger.fail_start(scope, ProviderCleanup::Uncertain)?;
                Err(BrokerError::Protocol)
            }
        }
    }
}

fn observe_owner_final(
    interface: bangbang_session::vmnet_provider::VmnetInterfaceId,
    owners: &mut BTreeMap<bangbang_session::vmnet_provider::VmnetInterfaceId, MacOwner>,
    ledger: &mut BrokerLedger,
) -> Result<(), BrokerError> {
    let owner = owners.get_mut(&interface).ok_or(BrokerError::Protocol)?;
    if owner.final_cleanup.is_some() {
        return Err(BrokerError::Protocol);
    }
    match owner.supervision.receive_owner() {
        Ok(OwnerSupervisionMessage::Final { scope, cleanup }) if scope == owner.scope => {
            ledger.mark_final(scope, cleanup)?;
            if owner.child.reap_clean().is_err() {
                owner.final_cleanup = Some(ProviderCleanup::Uncertain);
                return Err(BrokerError::CleanupUncertain);
            }
            owner.final_cleanup = Some(cleanup);
            if cleanup == ProviderCleanup::Complete {
                Ok(())
            } else {
                Err(BrokerError::CleanupUncertain)
            }
        }
        Ok(
            OwnerSupervisionMessage::Bootstrap(_)
            | OwnerSupervisionMessage::Ready { .. }
            | OwnerSupervisionMessage::Failed { .. }
            | OwnerSupervisionMessage::Stop { .. }
            | OwnerSupervisionMessage::Final { .. },
        )
        | Err(_) => Err(BrokerError::Protocol),
    }
}

fn stop_one(
    scope: OwnerScope,
    owners: &mut BTreeMap<bangbang_session::vmnet_provider::VmnetInterfaceId, MacOwner>,
    ledger: &mut BrokerLedger,
) -> ProviderCleanup {
    let Some(owner) = owners.get_mut(&scope.interface()) else {
        return ProviderCleanup::Uncertain;
    };
    if owner.scope != scope {
        return ProviderCleanup::Uncertain;
    }
    let cleanup = match owner.final_cleanup {
        Some(cleanup) => cleanup,
        None => {
            if owner
                .supervision
                .send_owner(&OwnerSupervisionMessage::Stop { scope })
                .is_err()
            {
                let _ = owner.child.terminate_and_reap();
                ProviderCleanup::Uncertain
            } else {
                match owner.supervision.receive_owner() {
                    Ok(OwnerSupervisionMessage::Final {
                        scope: final_scope,
                        cleanup,
                    }) if final_scope == scope => {
                        let _ = ledger.mark_final(scope, cleanup);
                        if owner.child.reap_clean().is_ok() {
                            cleanup
                        } else {
                            ProviderCleanup::Uncertain
                        }
                    }
                    Ok(
                        OwnerSupervisionMessage::Bootstrap(_)
                        | OwnerSupervisionMessage::Ready { .. }
                        | OwnerSupervisionMessage::Failed { .. }
                        | OwnerSupervisionMessage::Stop { .. }
                        | OwnerSupervisionMessage::Final { .. },
                    )
                    | Err(_) => {
                        let _ = owner.child.terminate_and_reap();
                        ProviderCleanup::Uncertain
                    }
                }
            }
        }
    };
    if ledger.retire(scope, cleanup).is_err() {
        ledger.mark_final(scope, cleanup).ok();
        if ledger.retire(scope, cleanup).is_err() {
            return ProviderCleanup::Uncertain;
        }
    }
    owners.remove(&scope.interface());
    cleanup
}

fn cleanup_all(
    owners: &mut BTreeMap<bangbang_session::vmnet_provider::VmnetInterfaceId, MacOwner>,
    ledger: &mut BrokerLedger,
) -> ProviderCleanup {
    let scopes = ledger.cancellation_order();
    let mut aggregate = ProviderCleanup::Complete;
    for scope in scopes {
        if stop_one(scope, owners, ledger) == ProviderCleanup::Uncertain {
            aggregate = ProviderCleanup::Uncertain;
        }
    }
    for owner in owners.values_mut() {
        let _ = owner.child.terminate_and_reap();
        aggregate = ProviderCleanup::Uncertain;
    }
    owners.clear();
    if ledger.is_terminal() {
        ProviderCleanup::Uncertain
    } else {
        aggregate
    }
}

fn send_terminal(
    transport: &mut VmnetProviderTransport,
    state: &mut VmnetControlBroker,
    code: ProviderTerminalCode,
) -> Result<(), BrokerError> {
    transport
        .send(state.terminal(code).map_err(|_| BrokerError::Protocol)?)
        .map_err(|_| BrokerError::Protocol)
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

struct MacOwner {
    scope: OwnerScope,
    child: OwnedChild,
    supervision: RecordTransport,
    client_data: Option<UnixStream>,
    final_cleanup: Option<ProviderCleanup>,
}

impl MacOwner {
    fn new(scope: OwnerScope, spawned: super::process::SpawnedOwner) -> Result<Self, BrokerError> {
        Ok(Self {
            scope,
            child: spawned.child,
            supervision: RecordTransport::new(spawned.supervision).map_err(map_record_error)?,
            client_data: Some(spawned.client_data),
            final_cleanup: None,
        })
    }
}

impl std::fmt::Debug for MacOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MacOwner(<redacted>)")
    }
}

struct BrokerSignals {
    received: Arc<AtomicBool>,
    registrations: [signal_hook::SigId; 2],
}

impl BrokerSignals {
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

impl Drop for BrokerSignals {
    fn drop(&mut self) {
        for registration in self.registrations {
            signal_hook::low_level::unregister(registration);
        }
    }
}
