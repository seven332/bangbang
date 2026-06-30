//! Backend-neutral network-interface configuration model.

use std::fmt;
use std::str::FromStr;

use crate::mmio::{MmioAccessBytes, MmioAccessBytesError, MmioHandlerError};
use crate::virtio_mmio::{
    VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError, VirtioMmioDeviceConfigHandler,
};

const MAC_ADDRESS_LEN: usize = 6;
pub const VIRTIO_NET_DEVICE_ID: u32 = 1;
pub const VIRTIO_NET_QUEUE_COUNT: usize = 2;
pub const VIRTIO_NET_RX_QUEUE_INDEX: usize = 0;
pub const VIRTIO_NET_TX_QUEUE_INDEX: usize = 1;
pub const VIRTIO_NET_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_NET_QUEUE_SIZES: [u16; VIRTIO_NET_QUEUE_COUNT] =
    [VIRTIO_NET_QUEUE_SIZE; VIRTIO_NET_QUEUE_COUNT];
pub const VIRTIO_NET_CONFIG_MAC_SIZE: usize = MAC_ADDRESS_LEN;
pub const VIRTIO_NET_F_MAC: u32 = 5;
pub const VIRTIO_RING_FEATURE_EVENT_IDX: u32 = 29;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkInterfaceConfigs {
    configs: Vec<NetworkInterfaceConfig>,
}

impl NetworkInterfaceConfigs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_slice(&self) -> &[NetworkInterfaceConfig] {
        &self.configs
    }

    pub fn insert(
        &mut self,
        input: NetworkInterfaceConfigInput,
    ) -> Result<(), NetworkInterfaceConfigError> {
        let config = input.validate()?;

        if let Some(guest_mac) = config.guest_mac()
            && self.configs.iter().any(|existing| {
                existing.iface_id() != config.iface_id() && existing.guest_mac() == Some(guest_mac)
            })
        {
            return Err(NetworkInterfaceConfigError::GuestMacAddressInUse { guest_mac });
        }

        if let Some(index) = self
            .configs
            .iter()
            .position(|existing| existing.iface_id() == config.iface_id())
        {
            self.configs.remove(index);
        }

        self.configs.push(config);

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkConfigSpace {
    guest_mac: Option<GuestMacAddress>,
}

impl VirtioNetworkConfigSpace {
    pub const fn new(guest_mac: Option<GuestMacAddress>) -> Self {
        Self { guest_mac }
    }

    pub const fn guest_mac(self) -> Option<GuestMacAddress> {
        self.guest_mac
    }

    pub const fn available_features(self) -> u64 {
        let features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX);
        if self.guest_mac.is_some() {
            features | virtio_feature_bit(VIRTIO_NET_F_MAC)
        } else {
            features
        }
    }

    const fn mac_bytes(self) -> Option<[u8; VIRTIO_NET_CONFIG_MAC_SIZE]> {
        match self.guest_mac {
            Some(guest_mac) => Some(guest_mac.octets()),
            None => None,
        }
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioNetworkConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let Some(mac) = self.mac_bytes() else {
            return Err(VirtioMmioDeviceConfigError::UnsupportedRead {
                offset: access.offset(),
                len: access.len(),
            });
        };
        let bytes = read_virtio_network_mac_bytes(&mac, access)?;
        MmioAccessBytes::new(bytes).map_err(network_config_bytes_error)
    }

    fn write_device_config(
        &mut self,
        access: VirtioMmioDeviceConfigAccess,
        _data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        Err(VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
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

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

fn read_virtio_network_mac_bytes(
    mac: &[u8; VIRTIO_NET_CONFIG_MAC_SIZE],
    access: VirtioMmioDeviceConfigAccess,
) -> Result<&[u8], VirtioMmioDeviceConfigError> {
    let offset = usize::try_from(access.offset()).map_err(|_| {
        VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        }
    })?;
    let Some(end) = offset.checked_add(access.len()) else {
        return Err(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        });
    };

    mac.get(offset..end)
        .ok_or(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        })
}

fn network_config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: MmioHandlerError::new(format!("virtio-net config access bytes failed: {source}")),
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
            if !part.as_bytes().iter().all(u8::is_ascii_hexdigit) {
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
    GuestMacAddressInUse {
        guest_mac: GuestMacAddress,
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
            Self::GuestMacAddressInUse { .. } => f.write_str("network guest_mac is already in use"),
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

    use crate::memory::GuestAddress;
    use crate::mmio::{MmioAccess, MmioAccessBytes, MmioBus, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioRegister,
        VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
    };

    use super::{
        GuestMacAddress, InterfaceIdSource, NetworkInterfaceConfig, NetworkInterfaceConfigError,
        NetworkInterfaceConfigInput, NetworkInterfaceConfigs, VIRTIO_FEATURE_VERSION_1,
        VIRTIO_NET_CONFIG_MAC_SIZE, VIRTIO_NET_DEVICE_ID, VIRTIO_NET_F_MAC, VIRTIO_NET_QUEUE_COUNT,
        VIRTIO_NET_QUEUE_SIZE, VIRTIO_NET_QUEUE_SIZES, VIRTIO_NET_RX_QUEUE_INDEX,
        VIRTIO_NET_TX_QUEUE_INDEX, VIRTIO_RING_FEATURE_EVENT_IDX, VirtioNetworkConfigSpace,
    };

    const TEST_MMIO_BASE: GuestAddress = GuestAddress::new(0x1000);

    fn input() -> NetworkInterfaceConfigInput {
        NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0")
    }

    fn validate(
        input: NetworkInterfaceConfigInput,
    ) -> Result<NetworkInterfaceConfig, NetworkInterfaceConfigError> {
        input.validate()
    }

    fn test_guest_mac() -> GuestMacAddress {
        GuestMacAddress::from_bytes([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc])
    }

    fn virtio_feature_bit(feature: u32) -> u64 {
        1_u64 << feature
    }

    fn mmio_access(offset: u64, len: usize) -> MmioAccess {
        let mut bus = MmioBus::new();
        bus.insert(
            MmioRegionId::new(1),
            TEST_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("virtio-mmio test region should insert");
        let address = TEST_MMIO_BASE
            .checked_add(offset)
            .expect("test offset should not overflow");
        bus.lookup(
            address,
            u64::try_from(len).expect("test access len should fit"),
        )
        .expect("test access should resolve")
    }

    fn network_handler(
        config: VirtioNetworkConfigSpace,
    ) -> VirtioMmioRegisterHandler<VirtioNetworkConfigSpace> {
        VirtioMmioRegisterHandler::with_device_config(
            VIRTIO_NET_DEVICE_ID,
            config.available_features(),
            &VIRTIO_NET_QUEUE_SIZES,
            config,
        )
        .expect("network register handler should build")
    }

    fn read_network_config(
        config: VirtioNetworkConfigSpace,
        offset: u64,
        len: usize,
    ) -> Result<MmioAccessBytes, VirtioMmioRegisterHandlerError> {
        network_handler(config)
            .read_access(mmio_access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset, len))
    }

    fn write_network_config_after_driver(
        config: VirtioNetworkConfigSpace,
        offset: u64,
        data: &[u8],
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        let mut handler = network_handler(config);
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("ACKNOWLEDGE status should write");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("DRIVER status should write");
        handler.write_access(
            mmio_access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset, data.len()),
            MmioAccessBytes::new(data).expect("test config write bytes should build"),
        )
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
            "+1:34:56:78:9a:bc",
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

    #[test]
    fn network_interface_configs_store_multiple_interfaces() {
        let mut configs = NetworkInterfaceConfigs::new();

        configs
            .insert(input().with_guest_mac("12:34:56:78:9a:bc"))
            .expect("first interface should be stored");
        configs
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1"))
            .expect("second interface should be stored");

        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].iface_id(), "eth0");
        assert_eq!(configs.as_slice()[1].iface_id(), "eth1");
    }

    #[test]
    fn network_interface_configs_replace_duplicate_id() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input().with_guest_mac("12:34:56:78:9a:bc"))
            .expect("initial interface should be stored");

        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap1"))
            .expect("duplicate interface id should replace existing config");

        assert_eq!(configs.as_slice().len(), 1);
        let config = &configs.as_slice()[0];
        assert_eq!(config.iface_id(), "eth0");
        assert_eq!(config.host_dev_name(), "tap1");
        assert_eq!(config.guest_mac(), None);
    }

    #[test]
    fn network_interface_configs_reject_duplicate_guest_mac_without_mutating() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input().with_guest_mac("12:34:56:78:9a:bc"))
            .expect("initial interface should be stored");

        let err = configs
            .insert(
                NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1")
                    .with_guest_mac("12:34:56:78:9a:BC"),
            )
            .expect_err("duplicate guest MAC should fail");

        assert_eq!(
            err,
            NetworkInterfaceConfigError::GuestMacAddressInUse {
                guest_mac: GuestMacAddress::from_bytes([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]),
            }
        );
        assert_eq!(err.to_string(), "network guest_mac is already in use");
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].iface_id(), "eth0");
    }

    #[test]
    fn virtio_network_constants_match_firecracker_shape() {
        assert_eq!(VIRTIO_NET_DEVICE_ID, 1);
        assert_eq!(VIRTIO_NET_QUEUE_COUNT, 2);
        assert_eq!(VIRTIO_NET_RX_QUEUE_INDEX, 0);
        assert_eq!(VIRTIO_NET_TX_QUEUE_INDEX, 1);
        assert_eq!(VIRTIO_NET_QUEUE_SIZE, 256);
        assert_eq!(VIRTIO_NET_QUEUE_SIZES, [256, 256]);
        assert_eq!(VIRTIO_NET_CONFIG_MAC_SIZE, 6);
        assert_eq!(VIRTIO_NET_F_MAC, 5);
        assert_eq!(VIRTIO_RING_FEATURE_EVENT_IDX, 29);
        assert_eq!(VIRTIO_FEATURE_VERSION_1, 32);
    }

    #[test]
    fn virtio_network_config_space_tracks_guest_mac_feature() {
        let base_features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX);
        let without_mac = VirtioNetworkConfigSpace::new(None);
        let with_mac = VirtioNetworkConfigSpace::new(Some(test_guest_mac()));

        assert_eq!(without_mac.guest_mac(), None);
        assert_eq!(without_mac.available_features(), base_features);
        assert_eq!(with_mac.guest_mac(), Some(test_guest_mac()));
        assert_eq!(
            with_mac.available_features(),
            base_features | virtio_feature_bit(VIRTIO_NET_F_MAC)
        );
    }

    #[test]
    fn virtio_network_config_space_reads_mac_bytes() {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()));

        assert_eq!(
            read_network_config(config, 0, 4)
                .expect("low MAC config word should read")
                .as_slice(),
            &[0x12, 0x34, 0x56, 0x78]
        );
        assert_eq!(
            read_network_config(config, 4, 2)
                .expect("high MAC config halfword should read")
                .as_slice(),
            &[0x9a, 0xbc]
        );
        assert_eq!(
            read_network_config(config, 1, 2)
                .expect("partial MAC config read should succeed")
                .as_slice(),
            &[0x34, 0x56]
        );
        assert_eq!(
            read_network_config(config, 5, 1)
                .expect("last MAC byte should read")
                .as_slice(),
            &[0xbc]
        );
        assert_eq!(
            read_network_config(config, 2, 4)
                .expect("read ending at MAC boundary should succeed")
                .as_slice(),
            &[0x56, 0x78, 0x9a, 0xbc]
        );
    }

    #[test]
    fn virtio_network_config_space_rejects_unsupported_reads() {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()));

        assert_eq!(
            read_network_config(VirtioNetworkConfigSpace::new(None), 0, 1),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 0, len: 1 })
        );
        assert_eq!(
            read_network_config(config, 6, 1),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 6, len: 1 })
        );
        assert_eq!(
            read_network_config(config, 5, 2),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 5, len: 2 })
        );
    }

    #[test]
    fn virtio_network_config_space_rejects_writes_after_driver_status() {
        assert_eq!(
            write_network_config_after_driver(
                VirtioNetworkConfigSpace::new(Some(test_guest_mac())),
                0,
                &[1, 2, 3, 4],
            ),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite { offset: 0, len: 4 })
        );
    }

    #[test]
    fn virtio_network_config_space_runs_through_mmio_register_handler() {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()));
        let mut handler = network_handler(config);

        assert_eq!(handler.device_registers().device_id(), VIRTIO_NET_DEVICE_ID);
        assert_eq!(
            handler.device_registers().device_features(),
            config.available_features()
        );
        assert_eq!(
            handler.queue_registers().queue_count(),
            VIRTIO_NET_QUEUE_COUNT
        );
        assert_eq!(
            handler
                .queue_registers()
                .queue(
                    VIRTIO_NET_RX_QUEUE_INDEX
                        .try_into()
                        .expect("RX index should fit")
                )
                .expect("RX queue should exist")
                .max_size(),
            VIRTIO_NET_QUEUE_SIZE
        );
        assert_eq!(
            handler
                .queue_registers()
                .queue(
                    VIRTIO_NET_TX_QUEUE_INDEX
                        .try_into()
                        .expect("TX index should fit")
                )
                .expect("TX queue should exist")
                .max_size(),
            VIRTIO_NET_QUEUE_SIZE
        );

        let read = handler
            .read_access(mmio_access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 4))
            .expect("network config read should delegate through handler");
        assert_eq!(read.as_slice(), &test_guest_mac().octets()[..4]);

        handler
            .write_register(VirtioMmioRegister::QueueSel, 1)
            .expect("TX queue should be selectable");
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::QueueNumMax)
                .expect("selected TX max queue size should read"),
            u32::from(VIRTIO_NET_QUEUE_SIZE)
        );
    }
}
