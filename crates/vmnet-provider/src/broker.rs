use std::fmt;
use std::io;

#[cfg(any(target_os = "macos", test))]
use bangbang_session::SessionId;
#[cfg(any(target_os = "macos", test))]
use bangbang_session::credential::CredentialTarget;
#[cfg(any(target_os = "macos", test))]
use bangbang_session::vmnet_provider::{
    ProviderCleanup, ProviderStatus, RequestedVmnetParameters, VmnetGeneration, VmnetInterfaceId,
    VmnetPolicySlot,
};
#[cfg(any(target_os = "macos", test))]
use std::collections::BTreeMap;

#[cfg(any(target_os = "macos", test))]
use crate::bootstrap::BrokerBootstrap;
#[cfg(any(target_os = "macos", test))]
use crate::policy::VmnetBrokerPolicy;
#[cfg(any(target_os = "macos", test))]
use crate::supervision::{OwnerBootstrap, OwnerScope};

/// Redacted broker failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerError {
    /// A private privileged entry did not have exact root identity.
    Authority,
    /// A fixed inherited descriptor was absent or had the wrong socket shape.
    Descriptor,
    /// The fixed bootstrap descriptor was absent or had the wrong socket shape.
    BootstrapDescriptor,
    /// The fixed provider descriptor was absent or had the wrong socket shape.
    ProviderDescriptor,
    /// Bootstrap or local configuration was invalid.
    InvalidConfiguration,
    /// Provider-v1 or owner supervision was invalid.
    Protocol,
    /// Exact child creation, identity, signal, or reap failed.
    Process,
    /// A fixed broker/owner deadline elapsed.
    Timeout,
    /// Backend cleanup ownership could not be proved.
    CleanupUncertain,
    /// One local descriptor operation failed.
    Io(io::ErrorKind),
}

impl fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private vmnet broker failure")
    }
}

impl std::error::Error for BrokerError {}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerLedgerState {
    Starting,
    Active,
    Final(ProviderCleanup),
}

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OwnerLedgerEntry {
    scope: OwnerScope,
    state: OwnerLedgerState,
}

#[cfg(any(target_os = "macos", test))]
pub(crate) struct BrokerLedger {
    session: SessionId,
    target: CredentialTarget,
    policy: VmnetBrokerPolicy,
    next_generation: VmnetGeneration,
    owners: BTreeMap<VmnetInterfaceId, OwnerLedgerEntry>,
    terminal: bool,
}

#[cfg(any(target_os = "macos", test))]
impl BrokerLedger {
    pub(crate) fn new(bootstrap: BrokerBootstrap) -> Result<Self, BrokerError> {
        Ok(Self {
            session: bootstrap.session(),
            target: bootstrap.target(),
            policy: VmnetBrokerPolicy::new(bootstrap.authority())
                .map_err(|_| BrokerError::InvalidConfiguration)?,
            next_generation: VmnetGeneration::MIN,
            owners: BTreeMap::new(),
            terminal: false,
        })
    }

    pub(crate) fn reserve_start(
        &mut self,
        interface: VmnetInterfaceId,
        slot: VmnetPolicySlot,
        requested: RequestedVmnetParameters,
    ) -> Result<OwnerBootstrap, ProviderStatus> {
        if self.terminal {
            return Err(ProviderStatus::CleanupUncertain);
        }
        if self.owners.len() >= self.policy.active_limit() {
            return Err(ProviderStatus::ResourceLimit);
        }
        if self.owners.contains_key(&interface) {
            return Err(ProviderStatus::InvalidArgument);
        }
        let policy = self.policy.resolve(slot, requested)?;
        let generation = self.next_generation;
        self.next_generation = generation
            .checked_next()
            .map_err(|_| ProviderStatus::ResourceLimit)?;
        let scope = OwnerScope::new(self.session, interface, generation)
            .map_err(|_| ProviderStatus::InvalidArgument)?;
        let bootstrap = OwnerBootstrap::new(scope, self.target, policy)
            .map_err(|_| ProviderStatus::InvalidArgument)?;
        self.owners.insert(
            interface,
            OwnerLedgerEntry {
                scope,
                state: OwnerLedgerState::Starting,
            },
        );
        Ok(bootstrap)
    }

    pub(crate) fn mark_ready(&mut self, scope: OwnerScope) -> Result<(), BrokerError> {
        let entry = self.exact_entry_mut(scope)?;
        if entry.state != OwnerLedgerState::Starting {
            return Err(BrokerError::Protocol);
        }
        entry.state = OwnerLedgerState::Active;
        Ok(())
    }

    pub(crate) fn fail_start(
        &mut self,
        scope: OwnerScope,
        cleanup: ProviderCleanup,
    ) -> Result<(), BrokerError> {
        let entry = self.exact_entry(scope)?;
        if entry.state != OwnerLedgerState::Starting {
            return Err(BrokerError::Protocol);
        }
        self.owners.remove(&scope.interface());
        if cleanup == ProviderCleanup::Uncertain {
            self.terminal = true;
        }
        Ok(())
    }

    pub(crate) fn mark_final(
        &mut self,
        scope: OwnerScope,
        cleanup: ProviderCleanup,
    ) -> Result<(), BrokerError> {
        let entry = self.exact_entry_mut(scope)?;
        if entry.state == OwnerLedgerState::Starting {
            return Err(BrokerError::Protocol);
        }
        entry.state = OwnerLedgerState::Final(cleanup);
        if cleanup == ProviderCleanup::Uncertain {
            self.terminal = true;
        }
        Ok(())
    }

    pub(crate) fn cleanup_for_stop(
        &self,
        scope: OwnerScope,
    ) -> Result<Option<ProviderCleanup>, BrokerError> {
        let entry = self.exact_entry(scope)?;
        Ok(match entry.state {
            OwnerLedgerState::Final(cleanup) => Some(cleanup),
            OwnerLedgerState::Starting | OwnerLedgerState::Active => None,
        })
    }

    pub(crate) fn retire(
        &mut self,
        scope: OwnerScope,
        cleanup: ProviderCleanup,
    ) -> Result<(), BrokerError> {
        if self.cleanup_for_stop(scope)? != Some(cleanup) {
            return Err(BrokerError::Protocol);
        }
        self.owners.remove(&scope.interface());
        if cleanup == ProviderCleanup::Uncertain {
            self.terminal = true;
        }
        Ok(())
    }

    pub(crate) fn cancellation_order(&self) -> Vec<OwnerScope> {
        self.owners.values().map(|entry| entry.scope).collect()
    }

    #[cfg(test)]
    fn active_count(&self) -> usize {
        self.owners.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.owners.is_empty()
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.terminal
    }

    fn exact_entry(&self, scope: OwnerScope) -> Result<&OwnerLedgerEntry, BrokerError> {
        self.owners
            .get(&scope.interface())
            .filter(|entry| entry.scope == scope)
            .ok_or(BrokerError::Protocol)
    }

    fn exact_entry_mut(&mut self, scope: OwnerScope) -> Result<&mut OwnerLedgerEntry, BrokerError> {
        self.owners
            .get_mut(&scope.interface())
            .filter(|entry| entry.scope == scope)
            .ok_or(BrokerError::Protocol)
    }
}

#[cfg(any(target_os = "macos", test))]
impl fmt::Debug for BrokerLedger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrokerLedger")
            .field("session", &"<redacted>")
            .field("target", &"<redacted>")
            .field("policy", &"<redacted>")
            .field("owner_count", &self.owners.len())
            .field("terminal", &self.terminal)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use bangbang_session::VmnetAuthority;

    use super::*;

    fn ledger(maximum: u8) -> BrokerLedger {
        BrokerLedger::new(
            BrokerBootstrap::new(
                SessionId::from_bytes([4; 32]),
                CredentialTarget::new(501, 20).expect("target should validate"),
                VmnetAuthority::try_new(true, true, maximum, &["en0"])
                    .expect("authority should validate"),
            )
            .expect("bootstrap should validate"),
        )
        .expect("ledger should construct")
    }

    fn interface(value: u32) -> VmnetInterfaceId {
        VmnetInterfaceId::new(value).expect("interface should validate")
    }

    fn requested() -> RequestedVmnetParameters {
        RequestedVmnetParameters::new(None, None).expect("request should validate")
    }

    #[test]
    fn enforces_policy_capacity_and_monotonic_generation_reuse() {
        let mut ledger = ledger(4);
        let mut scopes = Vec::new();
        for value in 1..=4 {
            let bootstrap = ledger
                .reserve_start(interface(value), VmnetPolicySlot::Shared, requested())
                .expect("start should reserve");
            let scope = bootstrap.scope();
            assert_eq!(scope.generation().get(), u64::from(value));
            ledger.mark_ready(scope).expect("ready should commit");
            scopes.push(scope);
        }
        assert_eq!(ledger.active_count(), 4);
        assert_eq!(
            ledger.reserve_start(interface(5), VmnetPolicySlot::Shared, requested()),
            Err(ProviderStatus::ResourceLimit)
        );
        let first = scopes[0];
        ledger
            .mark_final(first, ProviderCleanup::Complete)
            .expect("final should record");
        ledger
            .retire(first, ProviderCleanup::Complete)
            .expect("owner should retire");
        let reused = ledger
            .reserve_start(interface(1), VmnetPolicySlot::Host, requested())
            .expect("interface should reuse");
        assert_eq!(reused.scope().generation().get(), 5);
    }

    #[test]
    fn one_failure_preserves_independent_owners_and_cancellation_is_ordered() {
        let mut ledger = ledger(4);
        let first = ledger
            .reserve_start(interface(3), VmnetPolicySlot::Host, requested())
            .expect("first should reserve")
            .scope();
        ledger.mark_ready(first).expect("first should start");
        let second = ledger
            .reserve_start(interface(1), VmnetPolicySlot::Shared, requested())
            .expect("second should reserve")
            .scope();
        ledger
            .fail_start(second, ProviderCleanup::Complete)
            .expect("second should fail cleanly");
        assert_eq!(ledger.active_count(), 1);
        assert_eq!(ledger.cancellation_order(), [first]);
    }

    #[test]
    fn owner_first_completion_waits_for_correlated_stop_and_uncertainty_is_terminal() {
        let mut ledger = ledger(2);
        let scope = ledger
            .reserve_start(interface(1), VmnetPolicySlot::Shared, requested())
            .expect("start should reserve")
            .scope();
        ledger.mark_ready(scope).expect("ready should commit");
        ledger
            .mark_final(scope, ProviderCleanup::Complete)
            .expect("data-first final should record");
        assert_eq!(
            ledger.cleanup_for_stop(scope),
            Ok(Some(ProviderCleanup::Complete))
        );
        assert_eq!(ledger.active_count(), 1);
        ledger
            .retire(scope, ProviderCleanup::Complete)
            .expect("control stop should retire");
        assert!(ledger.is_empty());

        let uncertain = ledger
            .reserve_start(interface(2), VmnetPolicySlot::Host, requested())
            .expect("start should reserve")
            .scope();
        ledger.mark_ready(uncertain).expect("ready should commit");
        ledger
            .mark_final(uncertain, ProviderCleanup::Uncertain)
            .expect("uncertainty should record");
        assert!(ledger.is_terminal());
    }

    #[test]
    fn stale_scope_and_debug_are_fail_closed_and_redacted() {
        let mut ledger = ledger(1);
        let scope = ledger
            .reserve_start(interface(1), VmnetPolicySlot::Shared, requested())
            .expect("start should reserve")
            .scope();
        let stale = OwnerScope::new(
            scope.session(),
            scope.interface(),
            VmnetGeneration::new(99).expect("generation should validate"),
        )
        .expect("scope should validate");
        assert_eq!(ledger.mark_ready(stale), Err(BrokerError::Protocol));
        let debug = format!("{ledger:?}");
        assert!(!debug.contains("501"));
        assert!(!debug.contains("en0"));
        assert!(!debug.contains("0404"));
    }
}
