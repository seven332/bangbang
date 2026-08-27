use std::fmt;

use bangbang_session::SessionId;
use bangbang_session::credential::{CredentialMode, CredentialTarget};
use bangbang_session::vmnet_provider::{
    ProviderCleanup, ProviderStatus, RealizedVmnetParameters, RequestedVmnetParameters,
    VmnetGeneration, VmnetInterfaceId, VmnetPolicySlot,
};

use crate::bootstrap::BootstrapError;
use crate::policy::{BoundedBridgeName, ResolvedVmnetPolicy};

/// Exact size of every broker-owner supervision record.
pub const OWNER_SUPERVISION_BYTES: usize = 160;

const MAGIC: [u8; 8] = *b"BBVNOWN\0";
const VERSION: u16 = 1;
const KIND_BOOTSTRAP: u8 = 1;
const KIND_READY: u8 = 2;
const KIND_FAILED: u8 = 3;
const KIND_STOP: u8 = 4;
const KIND_FINAL: u8 = 5;
const REQUEST_MAC: u8 = 1 << 0;
const REQUEST_MTU: u8 = 1 << 1;
const REQUEST_FLAGS: u8 = REQUEST_MAC | REQUEST_MTU;
const PARAMETER_BACKEND_ID: u8 = 1 << 0;
const PARAMETER_READ_MAX: u8 = 1 << 1;
const PARAMETER_WRITE_MAX: u8 = 1 << 2;
const PARAMETER_DIRECT_AVAILABLE: u8 = 1 << 3;
const PARAMETER_DIRECT_ENABLED: u8 = 1 << 4;
const PARAMETER_FLAGS: u8 = PARAMETER_BACKEND_ID
    | PARAMETER_READ_MAX
    | PARAMETER_WRITE_MAX
    | PARAMETER_DIRECT_AVAILABLE
    | PARAMETER_DIRECT_ENABLED;

/// Exact scope shared by one broker entry and its owner.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct OwnerScope {
    session: SessionId,
    interface: VmnetInterfaceId,
    generation: VmnetGeneration,
}

impl OwnerScope {
    /// Constructs one nonzero provider-v1 owner scope.
    pub fn new(
        session: SessionId,
        interface: VmnetInterfaceId,
        generation: VmnetGeneration,
    ) -> Result<Self, BootstrapError> {
        if session.is_pre_session() {
            return Err(BootstrapError);
        }
        Ok(Self {
            session,
            interface,
            generation,
        })
    }

    /// Returns the lifecycle session.
    #[must_use]
    pub const fn session(self) -> SessionId {
        self.session
    }

    /// Returns the interface identity.
    #[must_use]
    pub const fn interface(self) -> VmnetInterfaceId {
        self.interface
    }

    /// Returns the generation identity.
    #[must_use]
    pub const fn generation(self) -> VmnetGeneration {
        self.generation
    }
}

impl fmt::Debug for OwnerScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnerScope(<redacted>)")
    }
}

/// Root-only resolved bootstrap sent from a broker to one owner.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnerBootstrap {
    scope: OwnerScope,
    target: CredentialTarget,
    policy: ResolvedVmnetPolicy,
}

impl OwnerBootstrap {
    /// Constructs one exact nonroot owner bootstrap.
    pub fn new(
        scope: OwnerScope,
        target: CredentialTarget,
        policy: ResolvedVmnetPolicy,
    ) -> Result<Self, BootstrapError> {
        if target.mode() != CredentialMode::Transition {
            return Err(BootstrapError);
        }
        Ok(Self {
            scope,
            target,
            policy,
        })
    }

    /// Returns the exact provider scope.
    #[must_use]
    pub const fn scope(&self) -> OwnerScope {
        self.scope
    }

    /// Returns the exact nonroot target.
    #[must_use]
    pub const fn target(&self) -> CredentialTarget {
        self.target
    }

    /// Returns the resolved bootstrap policy.
    #[must_use]
    pub const fn policy(&self) -> &ResolvedVmnetPolicy {
        &self.policy
    }
}

impl fmt::Debug for OwnerBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnerBootstrap(<redacted>)")
    }
}

/// Closed descriptor-free broker-owner supervision messages.
#[derive(Clone, PartialEq, Eq)]
pub enum OwnerSupervisionMessage {
    /// Broker-owned root bootstrap.
    Bootstrap(OwnerBootstrap),
    /// Owner proved start, realized parameters, and irreversible credential drop.
    Ready {
        /// Exact owner scope.
        scope: OwnerScope,
        /// Frozen provider parameters.
        parameters: RealizedVmnetParameters,
    },
    /// Owner could not become ready.
    Failed {
        /// Exact owner scope.
        scope: OwnerScope,
        /// Stable failure category.
        status: ProviderStatus,
        /// Backend cleanup certainty.
        cleanup: ProviderCleanup,
    },
    /// Broker-owned cancellation/stop request.
    Stop {
        /// Exact owner scope.
        scope: OwnerScope,
    },
    /// Owner reports its terminal cleanup result.
    Final {
        /// Exact owner scope.
        scope: OwnerScope,
        /// Backend cleanup certainty.
        cleanup: ProviderCleanup,
    },
}

impl OwnerSupervisionMessage {
    /// Returns the exact common scope.
    #[must_use]
    pub const fn scope(&self) -> OwnerScope {
        match self {
            Self::Bootstrap(bootstrap) => bootstrap.scope(),
            Self::Ready { scope, .. }
            | Self::Failed { scope, .. }
            | Self::Stop { scope }
            | Self::Final { scope, .. } => *scope,
        }
    }

    /// Encodes one canonical fixed record.
    #[must_use]
    pub fn encode(&self) -> [u8; OWNER_SUPERVISION_BYTES] {
        let mut encoded = [0_u8; OWNER_SUPERVISION_BYTES];
        encoded[..8].copy_from_slice(&MAGIC);
        encoded[8..10].copy_from_slice(&VERSION.to_be_bytes());
        let scope = self.scope();
        encoded[16..48].copy_from_slice(scope.session().as_bytes());
        encoded[48..52].copy_from_slice(&scope.interface().get().to_be_bytes());
        encoded[56..64].copy_from_slice(&scope.generation().get().to_be_bytes());

        match self {
            Self::Bootstrap(bootstrap) => {
                encoded[10] = KIND_BOOTSTRAP;
                encoded[64..68].copy_from_slice(&bootstrap.target().uid().to_be_bytes());
                encoded[68..72].copy_from_slice(&bootstrap.target().gid().to_be_bytes());
                let policy = bootstrap.policy();
                encoded[72] = policy.slot() as u8;
                let requested = policy.requested();
                if let Some(mac) = requested.mac() {
                    encoded[73] |= REQUEST_MAC;
                    encoded[76..82].copy_from_slice(&mac);
                }
                if let Some(mtu) = requested.mtu() {
                    encoded[73] |= REQUEST_MTU;
                    encoded[82..84].copy_from_slice(&mtu.to_be_bytes());
                }
                if let Some(bridge) = policy.bridge_name() {
                    encoded[74] = u8::try_from(bridge.len()).unwrap_or(0);
                    if let Some(destination) = encoded.get_mut(108..108 + bridge.len()) {
                        destination.copy_from_slice(bridge.as_bytes());
                    }
                }
            }
            Self::Ready { parameters, .. } => {
                encoded[10] = KIND_READY;
                encoded[76..82].copy_from_slice(&parameters.mac());
                encoded[82..84].copy_from_slice(&parameters.effective_mtu().to_be_bytes());
                encoded[84..88].copy_from_slice(&parameters.maximum_packet_bytes().to_be_bytes());
                if let Some(identity) = parameters.backend_interface_id() {
                    encoded[75] |= PARAMETER_BACKEND_ID;
                    encoded[92..108].copy_from_slice(&identity);
                }
                if let Some(value) = parameters.read_max_packets() {
                    encoded[75] |= PARAMETER_READ_MAX;
                    encoded[88..90].copy_from_slice(&value.to_be_bytes());
                }
                if let Some(value) = parameters.write_max_packets() {
                    encoded[75] |= PARAMETER_WRITE_MAX;
                    encoded[90..92].copy_from_slice(&value.to_be_bytes());
                }
                if parameters.direct_virtio_header_available() {
                    encoded[75] |= PARAMETER_DIRECT_AVAILABLE;
                }
                if parameters.direct_virtio_header_enabled() {
                    encoded[75] |= PARAMETER_DIRECT_ENABLED;
                }
            }
            Self::Failed {
                status, cleanup, ..
            } => {
                encoded[10] = KIND_FAILED;
                encoded[11] = *status as u8;
                encoded[73] = *cleanup as u8;
            }
            Self::Stop { .. } => encoded[10] = KIND_STOP,
            Self::Final { cleanup, .. } => {
                encoded[10] = KIND_FINAL;
                encoded[11] = *cleanup as u8;
            }
        }
        encoded
    }

    /// Decodes and canonicalizes one fixed record.
    pub fn decode(encoded: &[u8]) -> Result<Self, BootstrapError> {
        if encoded.len() != OWNER_SUPERVISION_BYTES
            || encoded.get(..8) != Some(MAGIC.as_slice())
            || read_u16(encoded, 8)? != VERSION
            || encoded.get(12..16).is_none_or(|bytes| bytes != [0; 4])
            || encoded.get(52..56).is_none_or(|bytes| bytes != [0; 4])
            || encoded.get(123..).is_none_or(|bytes| bytes != [0; 37])
        {
            return Err(BootstrapError);
        }
        let scope = OwnerScope::new(
            SessionId::from_bytes(read_array(encoded, 16)?),
            VmnetInterfaceId::new(read_u32(encoded, 48)?).map_err(|_| BootstrapError)?,
            VmnetGeneration::new(read_u64(encoded, 56)?).map_err(|_| BootstrapError)?,
        )?;
        let kind = *encoded.get(10).ok_or(BootstrapError)?;
        let status = *encoded.get(11).ok_or(BootstrapError)?;
        let request_flags = *encoded.get(73).ok_or(BootstrapError)?;
        let bridge_len = usize::from(*encoded.get(74).ok_or(BootstrapError)?);
        let parameter_flags = *encoded.get(75).ok_or(BootstrapError)?;
        let bridge_storage = encoded.get(108..123).ok_or(BootstrapError)?;
        let message = match kind {
            KIND_BOOTSTRAP => {
                if status != 0
                    || request_flags & !REQUEST_FLAGS != 0
                    || parameter_flags != 0
                    || encoded.get(84..108).is_none_or(|bytes| bytes != [0; 24])
                    || bridge_len > bangbang_session::MAX_VMNET_BRIDGE_NAME_BYTES
                    || bridge_storage
                        .get(bridge_len..)
                        .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
                {
                    return Err(BootstrapError);
                }
                let target = CredentialTarget::new(read_u32(encoded, 64)?, read_u32(encoded, 68)?)
                    .map_err(|_| BootstrapError)?;
                let slot = decode_slot(*encoded.get(72).ok_or(BootstrapError)?)?;
                let mac = if request_flags & REQUEST_MAC != 0 {
                    Some(read_array(encoded, 76)?)
                } else {
                    require_zero(encoded, 76, 82)?;
                    None
                };
                let mtu = if request_flags & REQUEST_MTU != 0 {
                    Some(read_u16(encoded, 82)?)
                } else {
                    require_zero(encoded, 82, 84)?;
                    None
                };
                let requested =
                    RequestedVmnetParameters::new(mac, mtu).map_err(|_| BootstrapError)?;
                let policy = match slot {
                    VmnetPolicySlot::Host if bridge_len == 0 => {
                        ResolvedVmnetPolicy::Host { requested }
                    }
                    VmnetPolicySlot::Shared if bridge_len == 0 => {
                        ResolvedVmnetPolicy::Shared { requested }
                    }
                    VmnetPolicySlot::Bridge0
                    | VmnetPolicySlot::Bridge1
                    | VmnetPolicySlot::Bridge2
                    | VmnetPolicySlot::Bridge3
                        if bridge_len != 0 =>
                    {
                        ResolvedVmnetPolicy::Bridged {
                            slot,
                            name: BoundedBridgeName::from_bytes(
                                bridge_storage.get(..bridge_len).ok_or(BootstrapError)?,
                            )?,
                            requested,
                        }
                    }
                    VmnetPolicySlot::Host
                    | VmnetPolicySlot::Shared
                    | VmnetPolicySlot::Bridge0
                    | VmnetPolicySlot::Bridge1
                    | VmnetPolicySlot::Bridge2
                    | VmnetPolicySlot::Bridge3 => return Err(BootstrapError),
                };
                Self::Bootstrap(OwnerBootstrap::new(scope, target, policy)?)
            }
            KIND_READY => {
                if status != 0
                    || request_flags != 0
                    || bridge_len != 0
                    || parameter_flags & !PARAMETER_FLAGS != 0
                    || read_u32(encoded, 64)? != 0
                    || read_u32(encoded, 68)? != 0
                    || *encoded.get(72).ok_or(BootstrapError)? != 0
                    || bridge_storage.iter().any(|byte| *byte != 0)
                {
                    return Err(BootstrapError);
                }
                let backend_id = (parameter_flags & PARAMETER_BACKEND_ID != 0)
                    .then(|| read_array(encoded, 92))
                    .transpose()?;
                if backend_id.is_none() {
                    require_zero(encoded, 92, 108)?;
                }
                let read_max = (parameter_flags & PARAMETER_READ_MAX != 0)
                    .then(|| read_u16(encoded, 88))
                    .transpose()?;
                if read_max.is_none() {
                    require_zero(encoded, 88, 90)?;
                }
                let write_max = (parameter_flags & PARAMETER_WRITE_MAX != 0)
                    .then(|| read_u16(encoded, 90))
                    .transpose()?;
                if write_max.is_none() {
                    require_zero(encoded, 90, 92)?;
                }
                let parameters = RealizedVmnetParameters::new(
                    read_array(encoded, 76)?,
                    read_u16(encoded, 82)?,
                    read_u32(encoded, 84)?,
                )
                .and_then(|parameters| parameters.with_backend_interface_id(backend_id))
                .and_then(|parameters| parameters.with_batch_limits(read_max, write_max))
                .and_then(|parameters| {
                    parameters.with_direct_virtio_header(
                        parameter_flags & PARAMETER_DIRECT_AVAILABLE != 0,
                        parameter_flags & PARAMETER_DIRECT_ENABLED != 0,
                    )
                })
                .map_err(|_| BootstrapError)?;
                Self::Ready { scope, parameters }
            }
            KIND_FAILED => {
                require_zero_except_scope(encoded, &[11, 73])?;
                Self::Failed {
                    scope,
                    status: decode_status(status)?,
                    cleanup: decode_cleanup(request_flags)?,
                }
            }
            KIND_STOP => {
                require_zero_except_scope(encoded, &[])?;
                Self::Stop { scope }
            }
            KIND_FINAL => {
                require_zero_except_scope(encoded, &[11])?;
                Self::Final {
                    scope,
                    cleanup: decode_cleanup(status)?,
                }
            }
            _ => return Err(BootstrapError),
        };
        if message.encode().as_slice() != encoded {
            return Err(BootstrapError);
        }
        Ok(message)
    }
}

impl fmt::Debug for OwnerSupervisionMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Bootstrap(_) => "OwnerSupervisionMessage::Bootstrap(<redacted>)",
            Self::Ready { .. } => "OwnerSupervisionMessage::Ready(<redacted>)",
            Self::Failed { .. } => "OwnerSupervisionMessage::Failed(<redacted>)",
            Self::Stop { .. } => "OwnerSupervisionMessage::Stop(<redacted>)",
            Self::Final { .. } => "OwnerSupervisionMessage::Final(<redacted>)",
        })
    }
}

fn decode_slot(value: u8) -> Result<VmnetPolicySlot, BootstrapError> {
    match value {
        1 => Ok(VmnetPolicySlot::Host),
        2 => Ok(VmnetPolicySlot::Shared),
        3 => Ok(VmnetPolicySlot::Bridge0),
        4 => Ok(VmnetPolicySlot::Bridge1),
        5 => Ok(VmnetPolicySlot::Bridge2),
        6 => Ok(VmnetPolicySlot::Bridge3),
        _ => Err(BootstrapError),
    }
}

fn decode_cleanup(value: u8) -> Result<ProviderCleanup, BootstrapError> {
    match value {
        1 => Ok(ProviderCleanup::Complete),
        2 => Ok(ProviderCleanup::Uncertain),
        _ => Err(BootstrapError),
    }
}

fn decode_status(value: u8) -> Result<ProviderStatus, BootstrapError> {
    match value {
        1 => Ok(ProviderStatus::PolicyDenied),
        2 => Ok(ProviderStatus::ResourceLimit),
        3 => Ok(ProviderStatus::NotAuthorized),
        4 => Ok(ProviderStatus::SharingServiceBusy),
        5 => Ok(ProviderStatus::InvalidArgument),
        6 => Ok(ProviderStatus::MemoryFailure),
        7 => Ok(ProviderStatus::PacketTooBig),
        8 => Ok(ProviderStatus::BufferExhausted),
        9 => Ok(ProviderStatus::TooManyPackets),
        10 => Ok(ProviderStatus::SetupIncomplete),
        11 => Ok(ProviderStatus::BackendFailure),
        12 => Ok(ProviderStatus::CleanupUncertain),
        _ => Err(BootstrapError),
    }
}

fn require_zero(bytes: &[u8], start: usize, end: usize) -> Result<(), BootstrapError> {
    if bytes
        .get(start..end)
        .is_some_and(|value| value.iter().all(|byte| *byte == 0))
    {
        Ok(())
    } else {
        Err(BootstrapError)
    }
}

fn require_zero_except_scope(bytes: &[u8], exceptions: &[usize]) -> Result<(), BootstrapError> {
    for index in 11..OWNER_SUPERVISION_BYTES {
        if (16..52).contains(&index) || (56..64).contains(&index) || exceptions.contains(&index) {
            continue;
        }
        if bytes.get(index) != Some(&0) {
            return Err(BootstrapError);
        }
    }
    Ok(())
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], BootstrapError> {
    bytes
        .get(offset..offset + N)
        .ok_or(BootstrapError)?
        .try_into()
        .map_err(|_| BootstrapError)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, BootstrapError> {
    read_array(bytes, offset).map(u16::from_be_bytes)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, BootstrapError> {
    read_array(bytes, offset).map(u32::from_be_bytes)
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, BootstrapError> {
    read_array(bytes, offset).map(u64::from_be_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> OwnerScope {
        OwnerScope::new(
            SessionId::from_bytes([3; 32]),
            VmnetInterfaceId::new(7).expect("interface should validate"),
            VmnetGeneration::new(9).expect("generation should validate"),
        )
        .expect("scope should validate")
    }

    fn requested() -> RequestedVmnetParameters {
        RequestedVmnetParameters::new(Some([2, 0, 0, 0, 0, 1]), Some(1500))
            .expect("request should validate")
    }

    fn realized() -> RealizedVmnetParameters {
        RealizedVmnetParameters::new([2, 0, 0, 0, 0, 2], 1500, 2048)
            .expect("parameters should validate")
            .with_backend_interface_id(Some([8; 16]))
            .expect("identity should validate")
            .with_batch_limits(Some(8), Some(9))
            .expect("batch should validate")
            .with_direct_virtio_header(true, true)
            .expect("header should validate")
    }

    fn bootstrap() -> OwnerSupervisionMessage {
        OwnerSupervisionMessage::Bootstrap(
            OwnerBootstrap::new(
                scope(),
                CredentialTarget::new(501, 20).expect("target should validate"),
                ResolvedVmnetPolicy::Bridged {
                    slot: VmnetPolicySlot::Bridge1,
                    name: BoundedBridgeName::from_bytes(b"secret_bridge")
                        .expect("bridge should validate"),
                    requested: requested(),
                },
            )
            .expect("bootstrap should validate"),
        )
    }

    #[test]
    fn every_closed_message_round_trips() {
        let messages = [
            bootstrap(),
            OwnerSupervisionMessage::Ready {
                scope: scope(),
                parameters: realized(),
            },
            OwnerSupervisionMessage::Failed {
                scope: scope(),
                status: ProviderStatus::NotAuthorized,
                cleanup: ProviderCleanup::Complete,
            },
            OwnerSupervisionMessage::Stop { scope: scope() },
            OwnerSupervisionMessage::Final {
                scope: scope(),
                cleanup: ProviderCleanup::Uncertain,
            },
        ];
        for message in messages {
            let encoded = message.encode();
            assert_eq!(
                OwnerSupervisionMessage::decode(&encoded),
                Ok(message.clone())
            );
            let debug = format!("{message:?}");
            assert!(!debug.contains("secret_bridge"));
            assert!(!debug.contains("501"));
            assert!(!debug.contains("1500"));
        }
    }

    #[test]
    fn rejects_reserved_wrong_role_scope_and_trailing_data() {
        let encoded = bootstrap().encode();
        for offset in [0, 8, 10, 12, 52, 74, 123] {
            let mut damaged = encoded;
            damaged[offset] ^= 0xff;
            assert_eq!(
                OwnerSupervisionMessage::decode(&damaged),
                Err(BootstrapError)
            );
        }
        for range in [16..48, 48..52, 56..64, 64..72] {
            let mut damaged = encoded;
            damaged[range].fill(0);
            assert_eq!(
                OwnerSupervisionMessage::decode(&damaged),
                Err(BootstrapError)
            );
        }
        assert_eq!(
            OwnerSupervisionMessage::decode(&encoded[..159]),
            Err(BootstrapError)
        );

        let mut stop = OwnerSupervisionMessage::Stop { scope: scope() }.encode();
        stop[84] = 1;
        assert_eq!(OwnerSupervisionMessage::decode(&stop), Err(BootstrapError));
    }

    #[test]
    fn supervision_has_no_packet_or_worker_control_payload() {
        for message in [
            bootstrap(),
            OwnerSupervisionMessage::Stop { scope: scope() },
            OwnerSupervisionMessage::Final {
                scope: scope(),
                cleanup: ProviderCleanup::Complete,
            },
        ] {
            let debug = format!("{message:?}");
            for forbidden in ["packet", "readiness", "write", "command", "path"] {
                assert!(!debug.contains(forbidden));
            }
        }

        let source = include_str!("supervision.rs");
        for forbidden in [
            concat!("Vmnet", "PacketBatch"),
            concat!("Data", "Message"),
            concat!("Control", "Message"),
            concat!("Vmnet", "ReadinessEpoch"),
            concat!("Provider", "Operation"),
            concat!("Unix", "Stream"),
        ] {
            assert!(!source.contains(forbidden));
        }
    }
}
