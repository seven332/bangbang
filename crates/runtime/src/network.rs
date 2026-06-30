//! Backend-neutral network-interface configuration model.

use std::fmt;
use std::str::FromStr;

const MAC_ADDRESS_LEN: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceConfigInput {
    path_iface_id: String,
    body_iface_id: String,
    host_dev_name: String,
    guest_mac: Option<String>,
    mtu_configured: bool,
    rx_rate_limiter_configured: bool,
    tx_rate_limiter_configured: bool,
}

impl NetworkInterfaceConfigInput {
    pub fn new(
        path_iface_id: impl Into<String>,
        body_iface_id: impl Into<String>,
        host_dev_name: impl Into<String>,
    ) -> Self {
        Self {
            path_iface_id: path_iface_id.into(),
            body_iface_id: body_iface_id.into(),
            host_dev_name: host_dev_name.into(),
            guest_mac: None,
            mtu_configured: false,
            rx_rate_limiter_configured: false,
            tx_rate_limiter_configured: false,
        }
    }

    pub fn path_iface_id(&self) -> &str {
        &self.path_iface_id
    }

    pub fn body_iface_id(&self) -> &str {
        &self.body_iface_id
    }

    pub fn host_dev_name(&self) -> &str {
        &self.host_dev_name
    }

    pub fn guest_mac(&self) -> Option<&str> {
        self.guest_mac.as_deref()
    }

    pub const fn mtu_configured(&self) -> bool {
        self.mtu_configured
    }

    pub const fn rx_rate_limiter_configured(&self) -> bool {
        self.rx_rate_limiter_configured
    }

    pub const fn tx_rate_limiter_configured(&self) -> bool {
        self.tx_rate_limiter_configured
    }

    pub fn with_guest_mac(mut self, guest_mac: impl Into<String>) -> Self {
        self.guest_mac = Some(guest_mac.into());
        self
    }

    pub const fn with_mtu_configured(mut self) -> Self {
        self.mtu_configured = true;
        self
    }

    pub const fn with_rx_rate_limiter_configured(mut self) -> Self {
        self.rx_rate_limiter_configured = true;
        self
    }

    pub const fn with_tx_rate_limiter_configured(mut self) -> Self {
        self.tx_rate_limiter_configured = true;
        self
    }

    pub fn validate(self) -> Result<NetworkInterfaceConfig, NetworkInterfaceConfigError> {
        NetworkInterfaceConfig::try_from(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceConfig {
    iface_id: String,
    host_dev_name: String,
    guest_mac: Option<GuestMacAddress>,
}

impl NetworkInterfaceConfig {
    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    pub fn host_dev_name(&self) -> &str {
        &self.host_dev_name
    }

    pub const fn guest_mac(&self) -> Option<GuestMacAddress> {
        self.guest_mac
    }
}

impl TryFrom<NetworkInterfaceConfigInput> for NetworkInterfaceConfig {
    type Error = NetworkInterfaceConfigError;

    fn try_from(input: NetworkInterfaceConfigInput) -> Result<Self, Self::Error> {
        validate_interface_id(InterfaceIdSource::Path, &input.path_iface_id)?;
        validate_interface_id(InterfaceIdSource::Body, &input.body_iface_id)?;
        if input.path_iface_id != input.body_iface_id {
            return Err(NetworkInterfaceConfigError::MismatchedInterfaceId {
                path_iface_id: input.path_iface_id,
                body_iface_id: input.body_iface_id,
            });
        }

        if input.host_dev_name.is_empty() {
            return Err(NetworkInterfaceConfigError::EmptyHostDeviceName);
        }

        if input.mtu_configured {
            return Err(NetworkInterfaceConfigError::UnsupportedMtu);
        }
        if input.rx_rate_limiter_configured {
            return Err(NetworkInterfaceConfigError::UnsupportedRxRateLimiter);
        }
        if input.tx_rate_limiter_configured {
            return Err(NetworkInterfaceConfigError::UnsupportedTxRateLimiter);
        }

        let guest_mac = input
            .guest_mac
            .map(|guest_mac| GuestMacAddress::from_str(&guest_mac))
            .transpose()?;

        Ok(Self {
            iface_id: input.path_iface_id,
            host_dev_name: input.host_dev_name,
            guest_mac,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuestMacAddress {
    bytes: [u8; MAC_ADDRESS_LEN],
}

impl GuestMacAddress {
    pub const fn from_bytes(bytes: [u8; MAC_ADDRESS_LEN]) -> Self {
        Self { bytes }
    }

    pub const fn octets(self) -> [u8; MAC_ADDRESS_LEN] {
        self.bytes
    }
}

impl fmt::Display for GuestMacAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [first, second, third, fourth, fifth, sixth] = self.bytes;
        write!(
            f,
            "{first:02x}:{second:02x}:{third:02x}:{fourth:02x}:{fifth:02x}:{sixth:02x}"
        )
    }
}

impl FromStr for GuestMacAddress {
    type Err = NetworkInterfaceConfigError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut parts = value.split(':');
        let mut bytes = [0_u8; MAC_ADDRESS_LEN];

        for byte in &mut bytes {
            let Some(part) = parts.next() else {
                return Err(NetworkInterfaceConfigError::InvalidGuestMacAddress {
                    guest_mac: value.to_string(),
                });
            };
            if part.len() != 2 {
                return Err(NetworkInterfaceConfigError::InvalidGuestMacAddress {
                    guest_mac: value.to_string(),
                });
            }
            *byte = u8::from_str_radix(part, 16).map_err(|_| {
                NetworkInterfaceConfigError::InvalidGuestMacAddress {
                    guest_mac: value.to_string(),
                }
            })?;
        }

        if parts.next().is_some() {
            return Err(NetworkInterfaceConfigError::InvalidGuestMacAddress {
                guest_mac: value.to_string(),
            });
        }

        Ok(Self { bytes })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceIdSource {
    Path,
    Body,
}

impl fmt::Display for InterfaceIdSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path => f.write_str("path iface_id"),
            Self::Body => f.write_str("body iface_id"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkInterfaceConfigError {
    EmptyInterfaceId {
        source: InterfaceIdSource,
    },
    InvalidInterfaceId {
        source: InterfaceIdSource,
        iface_id: String,
    },
    MismatchedInterfaceId {
        path_iface_id: String,
        body_iface_id: String,
    },
    EmptyHostDeviceName,
    InvalidGuestMacAddress {
        guest_mac: String,
    },
    UnsupportedMtu,
    UnsupportedRxRateLimiter,
    UnsupportedTxRateLimiter,
}

impl fmt::Display for NetworkInterfaceConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInterfaceId { source } => write!(f, "{source} must not be empty"),
            Self::InvalidInterfaceId { source, .. } => {
                write!(
                    f,
                    "{source} must contain only alphanumeric characters or '_'"
                )
            }
            Self::MismatchedInterfaceId { .. } => {
                f.write_str("path iface_id must match body iface_id")
            }
            Self::EmptyHostDeviceName => f.write_str("network host_dev_name must not be empty"),
            Self::InvalidGuestMacAddress { .. } => {
                f.write_str("network guest_mac must be six colon-separated hex octets")
            }
            Self::UnsupportedMtu => f.write_str("network mtu is not supported"),
            Self::UnsupportedRxRateLimiter => {
                f.write_str("network rx_rate_limiter is not supported")
            }
            Self::UnsupportedTxRateLimiter => {
                f.write_str("network tx_rate_limiter is not supported")
            }
        }
    }
}

impl std::error::Error for NetworkInterfaceConfigError {}

fn validate_interface_id(
    source: InterfaceIdSource,
    iface_id: &str,
) -> Result<(), NetworkInterfaceConfigError> {
    if iface_id.is_empty() {
        return Err(NetworkInterfaceConfigError::EmptyInterfaceId { source });
    }

    if !iface_id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(NetworkInterfaceConfigError::InvalidInterfaceId {
            source,
            iface_id: iface_id.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{
        GuestMacAddress, InterfaceIdSource, NetworkInterfaceConfig, NetworkInterfaceConfigError,
        NetworkInterfaceConfigInput,
    };

    fn input() -> NetworkInterfaceConfigInput {
        NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0")
    }

    fn validate(
        input: NetworkInterfaceConfigInput,
    ) -> Result<NetworkInterfaceConfig, NetworkInterfaceConfigError> {
        input.validate()
    }

    #[test]
    fn accepts_minimal_network_interface_config() {
        let config = validate(input()).expect("minimal network config should be valid");

        assert_eq!(config.iface_id(), "eth0");
        assert_eq!(config.host_dev_name(), "tap0");
        assert_eq!(config.guest_mac(), None);
    }

    #[test]
    fn accepts_network_interface_config_with_guest_mac() {
        let config = validate(input().with_guest_mac("12:34:56:78:9a:BC"))
            .expect("network config with guest MAC should be valid");

        let guest_mac = config.guest_mac().expect("guest MAC should be stored");
        assert_eq!(guest_mac.octets(), [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        assert_eq!(guest_mac.to_string(), "12:34:56:78:9a:bc");
    }

    #[test]
    fn accepts_firecracker_id_character_set() {
        let id = "net_\u{00e9}1";
        let config = validate(NetworkInterfaceConfigInput::new(id, id, "tap0"))
            .expect("Firecracker-compatible network id should be valid");

        assert_eq!(config.iface_id(), id);
    }

    #[test]
    fn rejects_empty_interface_ids() {
        assert_eq!(
            validate(NetworkInterfaceConfigInput::new("", "eth0", "tap0")),
            Err(NetworkInterfaceConfigError::EmptyInterfaceId {
                source: InterfaceIdSource::Path,
            })
        );
        assert_eq!(
            validate(NetworkInterfaceConfigInput::new("eth0", "", "tap0")),
            Err(NetworkInterfaceConfigError::EmptyInterfaceId {
                source: InterfaceIdSource::Body,
            })
        );
    }

    #[test]
    fn rejects_invalid_interface_ids_without_echoing_them() {
        let invalid = "bad/id\nsecret";
        let err = validate(NetworkInterfaceConfigInput::new(invalid, invalid, "tap0"))
            .expect_err("invalid path id should fail");

        assert_eq!(
            err,
            NetworkInterfaceConfigError::InvalidInterfaceId {
                source: InterfaceIdSource::Path,
                iface_id: invalid.to_string(),
            }
        );
        assert_eq!(
            err.to_string(),
            "path iface_id must contain only alphanumeric characters or '_'"
        );
        assert!(!err.to_string().contains(invalid));

        let err = validate(NetworkInterfaceConfigInput::new("eth0", invalid, "tap0"))
            .expect_err("invalid body id should fail");
        assert_eq!(
            err,
            NetworkInterfaceConfigError::InvalidInterfaceId {
                source: InterfaceIdSource::Body,
                iface_id: invalid.to_string(),
            }
        );
        assert_eq!(
            err.to_string(),
            "body iface_id must contain only alphanumeric characters or '_'"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn rejects_mismatched_interface_ids_without_echoing_them() {
        let err = validate(NetworkInterfaceConfigInput::new("eth0", "eth1", "tap0"))
            .expect_err("mismatched ids should fail");

        assert_eq!(
            err,
            NetworkInterfaceConfigError::MismatchedInterfaceId {
                path_iface_id: "eth0".to_string(),
                body_iface_id: "eth1".to_string(),
            }
        );
        assert_eq!(err.to_string(), "path iface_id must match body iface_id");
        assert!(!err.to_string().contains("eth1"));
    }

    #[test]
    fn rejects_empty_host_device_name_without_echoing_values() {
        let err = validate(NetworkInterfaceConfigInput::new("eth0", "eth0", ""))
            .expect_err("empty host device name should fail");

        assert_eq!(err, NetworkInterfaceConfigError::EmptyHostDeviceName);
        assert_eq!(err.to_string(), "network host_dev_name must not be empty");
    }

    #[test]
    fn rejects_invalid_guest_mac_addresses_without_echoing_them() {
        for invalid in [
            "",
            ":",
            "12:34:56:78:9a",
            "12:34:56:78:9a:bc:de",
            "12::56:78:9a:bc",
            "12:34:56:78:9a:b",
            "12:34:56:78:9a:bbb",
            "12:34:56:78:9a:xx",
            "12:34:56:78:9a:bc ",
            "123456789abc",
        ] {
            let err = validate(input().with_guest_mac(invalid))
                .expect_err("invalid guest MAC should fail");

            assert_eq!(
                err,
                NetworkInterfaceConfigError::InvalidGuestMacAddress {
                    guest_mac: invalid.to_string(),
                }
            );
            assert_eq!(
                err.to_string(),
                "network guest_mac must be six colon-separated hex octets"
            );
            if !invalid.is_empty() {
                assert!(!err.to_string().contains(invalid));
            }
        }
    }

    #[test]
    fn guest_mac_address_parses_and_displays_normalized_lowercase() {
        let mac = GuestMacAddress::from_str("12:34:56:78:9a:BC").expect("guest MAC should parse");

        assert_eq!(mac.octets(), [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        assert_eq!(mac.to_string(), "12:34:56:78:9a:bc");
        assert_eq!(
            GuestMacAddress::from_bytes([0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa]).to_string(),
            "ff:ee:dd:cc:bb:aa"
        );
    }

    #[test]
    fn rejects_deferred_fields() {
        assert_eq!(
            validate(input().with_mtu_configured()),
            Err(NetworkInterfaceConfigError::UnsupportedMtu)
        );
        assert_eq!(
            validate(input().with_rx_rate_limiter_configured()),
            Err(NetworkInterfaceConfigError::UnsupportedRxRateLimiter)
        );
        assert_eq!(
            validate(input().with_tx_rate_limiter_configured()),
            Err(NetworkInterfaceConfigError::UnsupportedTxRateLimiter)
        );
    }

    #[test]
    fn network_interface_config_input_exposes_firecracker_shape() {
        let input = input()
            .with_guest_mac("12:34:56:78:9a:bc")
            .with_mtu_configured()
            .with_rx_rate_limiter_configured()
            .with_tx_rate_limiter_configured();

        assert_eq!(input.path_iface_id(), "eth0");
        assert_eq!(input.body_iface_id(), "eth0");
        assert_eq!(input.host_dev_name(), "tap0");
        assert_eq!(input.guest_mac(), Some("12:34:56:78:9a:bc"));
        assert!(input.mtu_configured());
        assert!(input.rx_rate_limiter_configured());
        assert!(input.tx_rate_limiter_configured());
    }

    #[test]
    fn network_interface_config_errors_display_without_sources() {
        let err = NetworkInterfaceConfigError::UnsupportedRxRateLimiter;

        assert_eq!(err.to_string(), "network rx_rate_limiter is not supported");
        assert!(std::error::Error::source(&err).is_none());
    }
}
