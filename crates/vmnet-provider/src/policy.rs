use std::fmt;

use bangbang_session::VmnetAuthority;
use bangbang_session::vmnet_provider::{ProviderStatus, RequestedVmnetParameters, VmnetPolicySlot};

use crate::bootstrap::BootstrapError;

/// One trusted bootstrap policy used by a single broker session.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VmnetBrokerPolicy {
    authority: VmnetAuthority,
}

impl VmnetBrokerPolicy {
    /// Constructs a nonempty broker policy.
    pub fn new(authority: VmnetAuthority) -> Result<Self, BootstrapError> {
        if authority.is_denied() {
            Err(BootstrapError)
        } else {
            Ok(Self { authority })
        }
    }

    /// Returns the bootstrap-enforced active owner maximum.
    #[must_use]
    pub const fn active_limit(self) -> usize {
        match self.authority.max_interfaces() {
            Some(value) => value as usize,
            None => 0,
        }
    }

    /// Resolves a client-visible fixed slot without accepting a client string.
    pub fn resolve(
        self,
        slot: VmnetPolicySlot,
        requested: RequestedVmnetParameters,
    ) -> Result<ResolvedVmnetPolicy, ProviderStatus> {
        match slot {
            VmnetPolicySlot::Host if self.authority.allows_host() => {
                Ok(ResolvedVmnetPolicy::Host { requested })
            }
            VmnetPolicySlot::Shared if self.authority.allows_shared() => {
                Ok(ResolvedVmnetPolicy::Shared { requested })
            }
            VmnetPolicySlot::Bridge0
            | VmnetPolicySlot::Bridge1
            | VmnetPolicySlot::Bridge2
            | VmnetPolicySlot::Bridge3 => {
                let index = match slot {
                    VmnetPolicySlot::Bridge0 => 0,
                    VmnetPolicySlot::Bridge1 => 1,
                    VmnetPolicySlot::Bridge2 => 2,
                    VmnetPolicySlot::Bridge3 => 3,
                    VmnetPolicySlot::Host | VmnetPolicySlot::Shared => {
                        return Err(ProviderStatus::PolicyDenied);
                    }
                };
                self.authority
                    .bridge_slot(index)
                    .map(|name| ResolvedVmnetPolicy::Bridged {
                        slot,
                        name: BoundedBridgeName::new(name),
                        requested,
                    })
                    .ok_or(ProviderStatus::PolicyDenied)
            }
            VmnetPolicySlot::Host | VmnetPolicySlot::Shared => Err(ProviderStatus::PolicyDenied),
        }
    }
}

impl fmt::Debug for VmnetBrokerPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetBrokerPolicy(<redacted>)")
    }
}

/// Exact broker-resolved system vmnet configuration.
#[derive(Clone, PartialEq, Eq)]
pub enum ResolvedVmnetPolicy {
    /// Bootstrap-authorized host mode.
    Host {
        /// Bounded client parameters.
        requested: RequestedVmnetParameters,
    },
    /// Bootstrap-authorized shared mode.
    Shared {
        /// Bounded client parameters.
        requested: RequestedVmnetParameters,
    },
    /// One exact bootstrap bridge slot.
    Bridged {
        /// Original fixed slot for scope validation.
        slot: VmnetPolicySlot,
        /// Trusted bounded bridge name.
        name: BoundedBridgeName,
        /// Bounded client parameters.
        requested: RequestedVmnetParameters,
    },
}

impl ResolvedVmnetPolicy {
    /// Returns the fixed slot that selected this policy.
    #[must_use]
    pub const fn slot(&self) -> VmnetPolicySlot {
        match self {
            Self::Host { .. } => VmnetPolicySlot::Host,
            Self::Shared { .. } => VmnetPolicySlot::Shared,
            Self::Bridged { slot, .. } => *slot,
        }
    }

    /// Returns the bounded requested parameters.
    #[must_use]
    pub const fn requested(&self) -> RequestedVmnetParameters {
        match self {
            Self::Host { requested }
            | Self::Shared { requested }
            | Self::Bridged { requested, .. } => *requested,
        }
    }

    /// Returns the trusted bridge name only for a bridge slot.
    #[must_use]
    pub fn bridge_name(&self) -> Option<&str> {
        match self {
            Self::Bridged { name, .. } => Some(name.as_str()),
            Self::Host { .. } | Self::Shared { .. } => None,
        }
    }
}

impl fmt::Debug for ResolvedVmnetPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Host { .. } => "ResolvedVmnetPolicy::Host(<redacted>)",
            Self::Shared { .. } => "ResolvedVmnetPolicy::Shared(<redacted>)",
            Self::Bridged { .. } => "ResolvedVmnetPolicy::Bridged(<redacted>)",
        })
    }
}

/// Canonical bootstrap bridge name retained without heap allocation.
#[derive(Clone, PartialEq, Eq)]
pub struct BoundedBridgeName {
    bytes: [u8; bangbang_session::MAX_VMNET_BRIDGE_NAME_BYTES],
    len: u8,
}

impl BoundedBridgeName {
    fn new(value: &str) -> Self {
        let mut bytes = [0_u8; bangbang_session::MAX_VMNET_BRIDGE_NAME_BYTES];
        let len = value.len();
        if let Some(destination) = bytes.get_mut(..len) {
            destination.copy_from_slice(value.as_bytes());
        }
        Self {
            bytes,
            len: u8::try_from(len).unwrap_or(0),
        }
    }

    pub(crate) fn from_bytes(value: &[u8]) -> Result<Self, BootstrapError> {
        let value = std::str::from_utf8(value).map_err(|_| BootstrapError)?;
        if VmnetAuthority::try_new(false, false, 1, &[value]).is_err() {
            return Err(BootstrapError);
        }
        Ok(Self::new(value))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or(&[])
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(self.as_bytes()).unwrap_or("")
    }
}

impl fmt::Debug for BoundedBridgeName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BoundedBridgeName(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requested() -> RequestedVmnetParameters {
        RequestedVmnetParameters::new(Some([2, 0, 0, 0, 0, 1]), Some(1500))
            .expect("request should validate")
    }

    #[test]
    fn resolves_only_bootstrap_owned_slots() {
        let authority = VmnetAuthority::try_new(true, false, 3, &["en0", "bridge_1"])
            .expect("authority should validate");
        let policy = VmnetBrokerPolicy::new(authority).expect("policy should validate");
        assert_eq!(policy.active_limit(), 3);
        assert!(matches!(
            policy.resolve(VmnetPolicySlot::Host, requested()),
            Ok(ResolvedVmnetPolicy::Host { .. })
        ));
        assert_eq!(
            policy.resolve(VmnetPolicySlot::Shared, requested()),
            Err(ProviderStatus::PolicyDenied)
        );
        for (slot, expected) in [
            (VmnetPolicySlot::Bridge0, Some("en0")),
            (VmnetPolicySlot::Bridge1, Some("bridge_1")),
            (VmnetPolicySlot::Bridge2, None),
            (VmnetPolicySlot::Bridge3, None),
        ] {
            let actual = policy.resolve(slot, requested());
            assert_eq!(
                actual
                    .as_ref()
                    .ok()
                    .and_then(ResolvedVmnetPolicy::bridge_name),
                expected
            );
            assert_eq!(actual.is_ok(), expected.is_some());
        }
    }

    #[test]
    fn debug_never_exposes_policy_values() {
        let policy = VmnetBrokerPolicy::new(
            VmnetAuthority::try_new(false, false, 1, &["secret_bridge"])
                .expect("authority should validate"),
        )
        .expect("policy should validate");
        let resolved = policy
            .resolve(VmnetPolicySlot::Bridge0, requested())
            .expect("slot should resolve");
        for debug in [format!("{policy:?}"), format!("{resolved:?}")] {
            assert!(!debug.contains("secret_bridge"));
            assert!(!debug.contains("1500"));
        }
    }
}
