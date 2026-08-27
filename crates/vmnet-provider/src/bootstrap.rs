use std::fmt;

use bangbang_session::credential::{CredentialMode, CredentialTarget};
use bangbang_session::{
    MAX_VMNET_BRIDGE_NAME_BYTES, MAX_VMNET_BRIDGE_NAMES, SessionId, VmnetAuthority,
};

/// Exact encoded size of one broker bootstrap record.
pub const BROKER_BOOTSTRAP_BYTES: usize = 128;

const MAGIC: [u8; 8] = *b"BBVNBRK\0";
const VERSION: u16 = 1;
const FLAG_HOST: u8 = 1 << 0;
const FLAG_SHARED: u8 = 1 << 1;
const FLAGS: u8 = FLAG_HOST | FLAG_SHARED;
const BRIDGE_SLOT_BYTES: usize = MAX_VMNET_BRIDGE_NAME_BYTES + 1;

/// Redacted canonical-record failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapError;

impl fmt::Display for BootstrapError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid private vmnet provider bootstrap")
    }
}

impl std::error::Error for BootstrapError {}

/// Immutable authority supplied to one already-root broker process.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BrokerBootstrap {
    session: SessionId,
    target: CredentialTarget,
    authority: VmnetAuthority,
}

impl BrokerBootstrap {
    /// Constructs one nonempty, nonroot, session-bound bootstrap.
    pub fn new(
        session: SessionId,
        target: CredentialTarget,
        authority: VmnetAuthority,
    ) -> Result<Self, BootstrapError> {
        if session.is_pre_session()
            || target.mode() != CredentialMode::Transition
            || authority.is_denied()
        {
            return Err(BootstrapError);
        }
        Ok(Self {
            session,
            target,
            authority,
        })
    }

    /// Returns the exact lifecycle session.
    #[must_use]
    pub const fn session(self) -> SessionId {
        self.session
    }

    /// Returns the exact nonroot owner target.
    #[must_use]
    pub const fn target(self) -> CredentialTarget {
        self.target
    }

    /// Returns the immutable vmnet policy.
    #[must_use]
    pub const fn authority(self) -> VmnetAuthority {
        self.authority
    }

    /// Encodes the canonical fixed record.
    #[must_use]
    pub fn encode(self) -> [u8; BROKER_BOOTSTRAP_BYTES] {
        let mut encoded = [0_u8; BROKER_BOOTSTRAP_BYTES];
        encoded[..8].copy_from_slice(&MAGIC);
        encoded[8..10].copy_from_slice(&VERSION.to_be_bytes());
        encoded[10] = (if self.authority.allows_host() {
            FLAG_HOST
        } else {
            0
        }) | (if self.authority.allows_shared() {
            FLAG_SHARED
        } else {
            0
        });
        encoded[11] = self.authority.max_interfaces().unwrap_or(0);
        let bridge_count = (0..MAX_VMNET_BRIDGE_NAMES)
            .take_while(|index| self.authority.bridge_slot(*index).is_some())
            .count();
        encoded[12] = u8::try_from(bridge_count).unwrap_or(0);
        encoded[16..48].copy_from_slice(self.session.as_bytes());
        encoded[48..52].copy_from_slice(&self.target.uid().to_be_bytes());
        encoded[52..56].copy_from_slice(&self.target.gid().to_be_bytes());
        for index in 0..bridge_count {
            let Some(bridge) = self.authority.bridge_slot(index) else {
                continue;
            };
            let offset = 56 + index * BRIDGE_SLOT_BYTES;
            if let Some(length) = encoded.get_mut(offset) {
                *length = u8::try_from(bridge.len()).unwrap_or(0);
            }
            if let Some(destination) = encoded.get_mut(offset + 1..offset + 1 + bridge.len()) {
                destination.copy_from_slice(bridge.as_bytes());
            }
        }
        encoded
    }

    /// Decodes and revalidates one exact canonical record.
    pub fn decode(encoded: &[u8]) -> Result<Self, BootstrapError> {
        if encoded.len() != BROKER_BOOTSTRAP_BYTES
            || encoded.get(..8) != Some(MAGIC.as_slice())
            || read_u16(encoded, 8)? != VERSION
            || encoded.get(13..16).is_none_or(|bytes| bytes != [0; 3])
            || encoded.get(120..).is_none_or(|bytes| bytes != [0; 8])
        {
            return Err(BootstrapError);
        }
        let flags = *encoded.get(10).ok_or(BootstrapError)?;
        if flags & !FLAGS != 0 {
            return Err(BootstrapError);
        }
        let maximum = *encoded.get(11).ok_or(BootstrapError)?;
        let bridge_count = usize::from(*encoded.get(12).ok_or(BootstrapError)?);
        if bridge_count > MAX_VMNET_BRIDGE_NAMES {
            return Err(BootstrapError);
        }

        let mut names = [""; MAX_VMNET_BRIDGE_NAMES];
        for (index, name) in names.iter_mut().enumerate() {
            let offset = 56 + index * BRIDGE_SLOT_BYTES;
            let length = usize::from(*encoded.get(offset).ok_or(BootstrapError)?);
            let slot = encoded
                .get(offset + 1..offset + BRIDGE_SLOT_BYTES)
                .ok_or(BootstrapError)?;
            if index < bridge_count {
                if length == 0
                    || length > MAX_VMNET_BRIDGE_NAME_BYTES
                    || slot
                        .get(length..)
                        .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
                {
                    return Err(BootstrapError);
                }
                *name = std::str::from_utf8(slot.get(..length).ok_or(BootstrapError)?)
                    .map_err(|_| BootstrapError)?;
            } else if length != 0 || slot != [0; MAX_VMNET_BRIDGE_NAME_BYTES] {
                return Err(BootstrapError);
            }
        }

        let session = SessionId::from_bytes(
            encoded
                .get(16..48)
                .ok_or(BootstrapError)?
                .try_into()
                .map_err(|_| BootstrapError)?,
        );
        let target = CredentialTarget::new(read_u32(encoded, 48)?, read_u32(encoded, 52)?)
            .map_err(|_| BootstrapError)?;
        let authority = VmnetAuthority::try_new(
            flags & FLAG_HOST != 0,
            flags & FLAG_SHARED != 0,
            maximum,
            names.get(..bridge_count).ok_or(BootstrapError)?,
        )
        .map_err(|_| BootstrapError)?;
        let decoded = Self::new(session, target, authority)?;
        if decoded.encode().as_slice() != encoded {
            return Err(BootstrapError);
        }
        Ok(decoded)
    }
}

impl fmt::Debug for BrokerBootstrap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BrokerBootstrap(<redacted>)")
    }
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, BootstrapError> {
    bytes
        .get(offset..offset + 2)
        .ok_or(BootstrapError)?
        .try_into()
        .map(u16::from_be_bytes)
        .map_err(|_| BootstrapError)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, BootstrapError> {
    bytes
        .get(offset..offset + 4)
        .ok_or(BootstrapError)?
        .try_into()
        .map(u32::from_be_bytes)
        .map_err(|_| BootstrapError)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrap() -> BrokerBootstrap {
        BrokerBootstrap::new(
            SessionId::from_bytes([7; 32]),
            CredentialTarget::new(501, 20).expect("target should validate"),
            VmnetAuthority::try_new(true, true, 4, &["en0", "bridge_1"])
                .expect("authority should validate"),
        )
        .expect("bootstrap should validate")
    }

    #[test]
    fn canonical_record_round_trips_and_is_redacted() {
        let value = bootstrap();
        let encoded = value.encode();
        assert_eq!(BrokerBootstrap::decode(&encoded), Ok(value));
        assert_eq!(format!("{value:?}"), "BrokerBootstrap(<redacted>)");
        assert!(!format!("{value:?}").contains("501"));
    }

    #[test]
    fn rejects_root_denied_and_pre_session_bootstraps() {
        let session = SessionId::from_bytes([1; 32]);
        let target = CredentialTarget::new(501, 20).expect("target should validate");
        assert_eq!(
            BrokerBootstrap::new(
                session,
                CredentialTarget::new(0, 0).expect("root"),
                bootstrap().authority()
            ),
            Err(BootstrapError)
        );
        assert_eq!(
            BrokerBootstrap::new(session, target, VmnetAuthority::denied()),
            Err(BootstrapError)
        );
        assert_eq!(
            BrokerBootstrap::new(SessionId::pre_session(), target, bootstrap().authority()),
            Err(BootstrapError)
        );
    }

    #[test]
    fn rejects_every_field_class_and_noncanonical_padding() {
        let encoded = bootstrap().encode();
        for (offset, replacement) in [
            (0, 0_u8),
            (9, 0_u8),
            (10, 0x80_u8),
            (11, 0_u8),
            (12, 5_u8),
            (13, 1_u8),
            (56, 0_u8),
            (59, 1_u8),
            (120, 1_u8),
        ] {
            let mut damaged = encoded;
            damaged[offset] = replacement;
            assert_eq!(BrokerBootstrap::decode(&damaged), Err(BootstrapError));
        }
        let mut pre_session = encoded;
        pre_session[16..48].fill(0);
        assert_eq!(BrokerBootstrap::decode(&pre_session), Err(BootstrapError));
        let mut root_target = encoded;
        root_target[48..56].fill(0);
        assert_eq!(BrokerBootstrap::decode(&root_target), Err(BootstrapError));
        assert_eq!(
            BrokerBootstrap::decode(&encoded[..127]),
            Err(BootstrapError)
        );
    }
}
