//! Backend-neutral network-interface configuration model.

use std::collections::{BTreeMap, TryReserveError};
use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryRange,
};
use crate::metrics::SharedNetworkInterfaceMetrics;
use crate::mmio::{
    MmioAccessBytes, MmioAccessBytesError, MmioBusError, MmioDispatchError, MmioDispatcher,
    MmioHandlerError, MmioHandlerLookupError, MmioRegion, MmioRegionId,
};
pub use crate::network_packet::VirtioNetworkPacketEnvelope;
use crate::network_packet::{VirtioNetworkPacketPlan, VirtioNetworkPacketPlanError};
use crate::token_bucket::{
    PersistedTokenBucketState, PersistedTokenBucketStateError, TokenBucket, TokenBucketConfig,
    TokenBucketSnapshot,
};
use crate::virtio::VirtioInterruptIntent;
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError,
    VirtioMmioDeviceActivationHandler, VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError,
    VirtioMmioDeviceConfigHandler, VirtioMmioDeviceRegisters, VirtioMmioQueueRegisterError,
    VirtioMmioQueueRegisters, VirtioMmioQueueState, VirtioMmioRegisterHandler,
    VirtioMmioRegisterHandlerError, VirtioMmioTransportState,
};
use crate::virtio_pci::{
    VirtioPciDeviceOperationError, VirtioPciEndpoint, VirtioPciEndpointError,
    VirtioPciTransportState,
};
use crate::virtio_queue::{
    VirtqueueAvailableRing, VirtqueueAvailableRingError, VirtqueueDescriptor,
    VirtqueueDescriptorChain, VirtqueueDescriptorChainOptions, VirtqueueNotificationSuppression,
    VirtqueueUsedRing, VirtqueueUsedRingError, VirtqueueUsedRingPublication,
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
pub const VIRTIO_NET_CONFIG_MTU_OFFSET: u64 = 10;
pub const VIRTIO_NET_CONFIG_MTU_SIZE: usize = 2;
pub const VIRTIO_NET_MIN_MTU: u16 = 68;
pub const VIRTIO_NET_MAX_MTU: u16 = u16::MAX;
pub const VIRTIO_NET_F_CSUM: u32 = 0;
pub const VIRTIO_NET_F_GUEST_CSUM: u32 = 1;
pub const VIRTIO_NET_F_MTU: u32 = 3;
pub const VIRTIO_NET_F_MAC: u32 = 5;
pub const VIRTIO_NET_F_GUEST_TSO4: u32 = 7;
pub const VIRTIO_NET_F_GUEST_TSO6: u32 = 8;
pub const VIRTIO_NET_F_GUEST_UFO: u32 = 10;
pub const VIRTIO_NET_F_HOST_TSO4: u32 = 11;
pub const VIRTIO_NET_F_HOST_TSO6: u32 = 12;
pub const VIRTIO_NET_F_HOST_UFO: u32 = 14;
pub const VIRTIO_NET_F_MRG_RXBUF: u32 = 15;
pub const VIRTIO_RING_FEATURE_INDIRECT_DESC: u32 = 28;
pub const VIRTIO_RING_FEATURE_EVENT_IDX: u32 = 29;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;
pub const VIRTIO_NET_TX_HEADER_SIZE: u32 = 12;
pub const VIRTIO_NET_MAX_BUFFER_SIZE: u64 = 65_562;
pub const VIRTIO_NET_RX_MIN_BUFFER_SIZE: u64 = 1_526;
pub const VIRTIO_NET_RX_LARGE_BUFFER_SIZE: u64 = VIRTIO_NET_MAX_BUFFER_SIZE;
pub const MAX_NETWORK_INTERFACE_COUNT: usize = 16;

pub const VIRTIO_NET_HDR_F_NEEDS_CSUM: u8 = 1;
pub const VIRTIO_NET_HDR_F_DATA_VALID: u8 = 2;
pub const VIRTIO_NET_HDR_F_RSC_INFO: u8 = 4;
pub const VIRTIO_NET_HDR_GSO_NONE: u8 = 0;
pub const VIRTIO_NET_HDR_GSO_TCPV4: u8 = 1;
pub const VIRTIO_NET_HDR_GSO_UDP: u8 = 3;
pub const VIRTIO_NET_HDR_GSO_TCPV6: u8 = 4;
pub const VIRTIO_NET_HDR_GSO_ECN: u8 = 0x80;

const VIRTIO_NET_RX_QUEUE_INDEX_U32: u32 = 0;
const VIRTIO_NET_TX_QUEUE_INDEX_U32: u32 = 1;

pub type VirtioNetworkMmioHandler =
    VirtioMmioRegisterHandler<VirtioNetworkConfigSpace, VirtioNetworkDevice>;
pub type VirtioNetworkPciEndpoint =
    VirtioPciEndpoint<VirtioNetworkConfigSpace, VirtioNetworkDevice>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkFeatureCapabilities {
    checksum: bool,
    guest_checksum: bool,
    guest_tso4: bool,
    guest_tso6: bool,
    guest_ufo: bool,
    host_tso4: bool,
    host_tso6: bool,
    host_ufo: bool,
    merged_rx_buffers: bool,
}

impl VirtioNetworkFeatureCapabilities {
    pub const fn complete_software() -> Self {
        Self {
            checksum: true,
            guest_checksum: true,
            guest_tso4: true,
            guest_tso6: true,
            guest_ufo: true,
            host_tso4: true,
            host_tso6: true,
            host_ufo: true,
            merged_rx_buffers: true,
        }
    }

    pub const fn none() -> Self {
        Self {
            checksum: false,
            guest_checksum: false,
            guest_tso4: false,
            guest_tso6: false,
            guest_ufo: false,
            host_tso4: false,
            host_tso6: false,
            host_ufo: false,
            merged_rx_buffers: false,
        }
    }

    pub const fn checksum(self) -> bool {
        self.checksum
    }

    pub const fn guest_checksum(self) -> bool {
        self.guest_checksum
    }

    pub const fn guest_tso4(self) -> bool {
        self.guest_tso4
    }

    pub const fn guest_tso6(self) -> bool {
        self.guest_tso6
    }

    pub const fn guest_ufo(self) -> bool {
        self.guest_ufo
    }

    pub const fn host_tso4(self) -> bool {
        self.host_tso4
    }

    pub const fn host_tso6(self) -> bool {
        self.host_tso6
    }

    pub const fn host_ufo(self) -> bool {
        self.host_ufo
    }

    pub const fn merged_rx_buffers(self) -> bool {
        self.merged_rx_buffers
    }

    pub const fn with_checksum(mut self, supported: bool) -> Self {
        self.checksum = supported;
        self
    }

    pub const fn with_guest_checksum(mut self, supported: bool) -> Self {
        self.guest_checksum = supported;
        self
    }

    pub const fn with_guest_tso4(mut self, supported: bool) -> Self {
        self.guest_tso4 = supported;
        self
    }

    pub const fn with_guest_tso6(mut self, supported: bool) -> Self {
        self.guest_tso6 = supported;
        self
    }

    pub const fn with_guest_ufo(mut self, supported: bool) -> Self {
        self.guest_ufo = supported;
        self
    }

    pub const fn with_host_tso4(mut self, supported: bool) -> Self {
        self.host_tso4 = supported;
        self
    }

    pub const fn with_host_tso6(mut self, supported: bool) -> Self {
        self.host_tso6 = supported;
        self
    }

    pub const fn with_host_ufo(mut self, supported: bool) -> Self {
        self.host_ufo = supported;
        self
    }

    pub const fn with_merged_rx_buffers(mut self, supported: bool) -> Self {
        self.merged_rx_buffers = supported;
        self
    }

    pub const fn feature_bits(self) -> u64 {
        let mut features = 0;
        if self.checksum {
            features |= virtio_feature_bit(VIRTIO_NET_F_CSUM);
        }
        if self.guest_checksum {
            features |= virtio_feature_bit(VIRTIO_NET_F_GUEST_CSUM);
        }
        if self.guest_tso4 && self.guest_checksum {
            features |= virtio_feature_bit(VIRTIO_NET_F_GUEST_TSO4);
        }
        if self.guest_tso6 && self.guest_checksum {
            features |= virtio_feature_bit(VIRTIO_NET_F_GUEST_TSO6);
        }
        if self.guest_ufo && self.guest_checksum {
            features |= virtio_feature_bit(VIRTIO_NET_F_GUEST_UFO);
        }
        if self.host_tso4 && self.checksum {
            features |= virtio_feature_bit(VIRTIO_NET_F_HOST_TSO4);
        }
        if self.host_tso6 && self.checksum {
            features |= virtio_feature_bit(VIRTIO_NET_F_HOST_TSO6);
        }
        if self.host_ufo && self.checksum {
            features |= virtio_feature_bit(VIRTIO_NET_F_HOST_UFO);
        }
        if self.merged_rx_buffers {
            features |= virtio_feature_bit(VIRTIO_NET_F_MRG_RXBUF);
        }
        features
    }

    pub const fn is_dependency_complete(self) -> bool {
        (!self.guest_tso4 && !self.guest_tso6 && !self.guest_ufo || self.guest_checksum)
            && (!self.host_tso4 && !self.host_tso6 && !self.host_ufo || self.checksum)
    }

    pub const fn supports(self, feature: u32) -> bool {
        self.feature_bits() & virtio_feature_bit(feature) != 0
    }
}

impl Default for VirtioNetworkFeatureCapabilities {
    fn default() -> Self {
        Self::complete_software()
    }
}

#[derive(Debug, Clone)]
struct VirtioNetworkTransportMetrics {
    interface: SharedNetworkInterfaceMetrics,
    aggregate: Option<SharedNetworkInterfaceMetrics>,
}

impl VirtioNetworkTransportMetrics {
    fn for_interface(interface: SharedNetworkInterfaceMetrics) -> Self {
        Self {
            interface,
            aggregate: None,
        }
    }

    fn with_aggregate(
        interface: SharedNetworkInterfaceMetrics,
        aggregate: SharedNetworkInterfaceMetrics,
    ) -> Self {
        Self {
            interface,
            aggregate: Some(aggregate),
        }
    }

    fn record_activation_failure(&self) {
        self.interface.record_activation_failure();
        if let Some(aggregate) = &self.aggregate {
            aggregate.record_activation_failure();
        }
    }

    fn record_config_failure(&self) {
        self.interface.record_config_failure();
        if let Some(aggregate) = &self.aggregate {
            aggregate.record_config_failure();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkRateLimiterConfig {
    bandwidth: Option<NetworkTokenBucketConfig>,
    ops: Option<NetworkTokenBucketConfig>,
}

impl NetworkRateLimiterConfig {
    pub const fn new(
        bandwidth: Option<NetworkTokenBucketConfig>,
        ops: Option<NetworkTokenBucketConfig>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    pub const fn bandwidth(self) -> Option<NetworkTokenBucketConfig> {
        self.bandwidth
    }

    pub const fn ops(self) -> Option<NetworkTokenBucketConfig> {
        self.ops
    }

    pub const fn is_configured(self) -> bool {
        self.bandwidth.is_some() || self.ops.is_some()
    }

    const fn normalized(self) -> Option<Self> {
        let bandwidth = enabled_network_token_bucket(self.bandwidth);
        let ops = enabled_network_token_bucket(self.ops);
        if bandwidth.is_none() && ops.is_none() {
            None
        } else {
            Some(Self { bandwidth, ops })
        }
    }

    fn applied_to(self, existing: Option<Self>) -> Option<Self> {
        let bandwidth =
            updated_network_token_bucket(existing.and_then(Self::bandwidth), self.bandwidth);
        let ops = updated_network_token_bucket(existing.and_then(Self::ops), self.ops);
        if bandwidth.is_none() && ops.is_none() {
            None
        } else {
            Some(Self { bandwidth, ops })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkTokenBucketConfig {
    size: u64,
    one_time_burst: Option<u64>,
    refill_time: u64,
}

impl NetworkTokenBucketConfig {
    pub const fn new(size: u64, one_time_burst: Option<u64>, refill_time: u64) -> Self {
        Self {
            size,
            one_time_burst,
            refill_time,
        }
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn one_time_burst(self) -> Option<u64> {
        self.one_time_burst
    }

    pub const fn refill_time(self) -> u64 {
        self.refill_time
    }

    const fn token_bucket_config(self) -> TokenBucketConfig {
        TokenBucketConfig::new(self.size, self.one_time_burst, self.refill_time)
    }

    const fn is_enabled(self) -> bool {
        self.token_bucket_config().is_enabled()
    }
}

const fn enabled_network_token_bucket(
    bucket: Option<NetworkTokenBucketConfig>,
) -> Option<NetworkTokenBucketConfig> {
    match bucket {
        Some(bucket) if bucket.is_enabled() => Some(bucket),
        Some(_) | None => None,
    }
}

const fn updated_network_token_bucket(
    existing: Option<NetworkTokenBucketConfig>,
    update: Option<NetworkTokenBucketConfig>,
) -> Option<NetworkTokenBucketConfig> {
    match update {
        Some(bucket) if bucket.is_enabled() => Some(bucket),
        Some(_) => None,
        None => existing,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceConfigInput {
    path_iface_id: String,
    body_iface_id: String,
    host_dev_name: String,
    guest_mac: Option<String>,
    mtu: Option<u16>,
    rx_rate_limiter: Option<NetworkRateLimiterConfig>,
    tx_rate_limiter: Option<NetworkRateLimiterConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceUpdateInput {
    path_iface_id: String,
    body_iface_id: String,
    rx_rate_limiter: Option<NetworkRateLimiterConfig>,
    tx_rate_limiter: Option<NetworkRateLimiterConfig>,
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
            mtu: None,
            rx_rate_limiter: None,
            tx_rate_limiter: None,
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

    pub const fn mtu(&self) -> Option<u16> {
        self.mtu
    }

    pub const fn rx_rate_limiter(&self) -> Option<NetworkRateLimiterConfig> {
        self.rx_rate_limiter
    }

    pub const fn tx_rate_limiter(&self) -> Option<NetworkRateLimiterConfig> {
        self.tx_rate_limiter
    }

    pub fn with_guest_mac(mut self, guest_mac: impl Into<String>) -> Self {
        self.guest_mac = Some(guest_mac.into());
        self
    }

    pub const fn with_mtu(mut self, mtu: u16) -> Self {
        self.mtu = Some(mtu);
        self
    }

    pub const fn with_rx_rate_limiter(mut self, rate_limiter: NetworkRateLimiterConfig) -> Self {
        self.rx_rate_limiter = Some(rate_limiter);
        self
    }

    pub const fn with_tx_rate_limiter(mut self, rate_limiter: NetworkRateLimiterConfig) -> Self {
        self.tx_rate_limiter = Some(rate_limiter);
        self
    }

    pub fn validate(self) -> Result<NetworkInterfaceConfig, NetworkInterfaceConfigError> {
        NetworkInterfaceConfig::try_from(self)
    }
}

impl NetworkInterfaceUpdateInput {
    pub fn new(path_iface_id: impl Into<String>, body_iface_id: impl Into<String>) -> Self {
        Self {
            path_iface_id: path_iface_id.into(),
            body_iface_id: body_iface_id.into(),
            rx_rate_limiter: None,
            tx_rate_limiter: None,
        }
    }

    pub fn path_iface_id(&self) -> &str {
        &self.path_iface_id
    }

    pub fn body_iface_id(&self) -> &str {
        &self.body_iface_id
    }

    pub const fn rx_rate_limiter(&self) -> Option<NetworkRateLimiterConfig> {
        self.rx_rate_limiter
    }

    pub const fn tx_rate_limiter(&self) -> Option<NetworkRateLimiterConfig> {
        self.tx_rate_limiter
    }

    pub const fn with_rx_rate_limiter(mut self, rate_limiter: NetworkRateLimiterConfig) -> Self {
        self.rx_rate_limiter = Some(rate_limiter);
        self
    }

    pub const fn with_tx_rate_limiter(mut self, rate_limiter: NetworkRateLimiterConfig) -> Self {
        self.tx_rate_limiter = Some(rate_limiter);
        self
    }

    pub fn validate(self) -> Result<NetworkInterfaceUpdate, NetworkInterfaceUpdateError> {
        NetworkInterfaceUpdate::try_from(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceConfig {
    iface_id: String,
    host_dev_name: String,
    guest_mac: Option<GuestMacAddress>,
    mtu: Option<u16>,
    rx_rate_limiter: Option<NetworkRateLimiterConfig>,
    tx_rate_limiter: Option<NetworkRateLimiterConfig>,
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

    pub const fn mtu(&self) -> Option<u16> {
        self.mtu
    }

    pub const fn rx_rate_limiter(&self) -> Option<NetworkRateLimiterConfig> {
        self.rx_rate_limiter
    }

    pub const fn tx_rate_limiter(&self) -> Option<NetworkRateLimiterConfig> {
        self.tx_rate_limiter
    }

    fn updated(&self, update: &NetworkInterfaceUpdate) -> Self {
        Self {
            iface_id: self.iface_id.clone(),
            host_dev_name: self.host_dev_name.clone(),
            guest_mac: self.guest_mac,
            mtu: self.mtu,
            rx_rate_limiter: match update.rx_rate_limiter() {
                Some(rate_limiter) => rate_limiter.applied_to(self.rx_rate_limiter),
                None => self.rx_rate_limiter,
            },
            tx_rate_limiter: match update.tx_rate_limiter() {
                Some(rate_limiter) => rate_limiter.applied_to(self.tx_rate_limiter),
                None => self.tx_rate_limiter,
            },
        }
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

        if let Some(mtu) = input.mtu
            && !(VIRTIO_NET_MIN_MTU..=VIRTIO_NET_MAX_MTU).contains(&mtu)
        {
            return Err(NetworkInterfaceConfigError::InvalidMtu { mtu });
        }
        let guest_mac = input
            .guest_mac
            .map(|guest_mac| GuestMacAddress::from_str(&guest_mac))
            .transpose()?;

        Ok(Self {
            iface_id: input.path_iface_id,
            host_dev_name: input.host_dev_name,
            guest_mac,
            mtu: input.mtu,
            rx_rate_limiter: input
                .rx_rate_limiter
                .and_then(NetworkRateLimiterConfig::normalized),
            tx_rate_limiter: input
                .tx_rate_limiter
                .and_then(NetworkRateLimiterConfig::normalized),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkInterfaceUpdate {
    iface_id: String,
    rx_rate_limiter: Option<NetworkRateLimiterConfig>,
    tx_rate_limiter: Option<NetworkRateLimiterConfig>,
}

impl NetworkInterfaceUpdate {
    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    pub const fn rx_rate_limiter(&self) -> Option<NetworkRateLimiterConfig> {
        self.rx_rate_limiter
    }

    pub const fn tx_rate_limiter(&self) -> Option<NetworkRateLimiterConfig> {
        self.tx_rate_limiter
    }

    pub const fn is_noop(&self) -> bool {
        self.rx_rate_limiter.is_none() && self.tx_rate_limiter.is_none()
    }
}

impl TryFrom<NetworkInterfaceUpdateInput> for NetworkInterfaceUpdate {
    type Error = NetworkInterfaceUpdateError;

    fn try_from(input: NetworkInterfaceUpdateInput) -> Result<Self, Self::Error> {
        validate_interface_update_id(InterfaceIdSource::Path, &input.path_iface_id)?;
        validate_interface_update_id(InterfaceIdSource::Body, &input.body_iface_id)?;
        if input.path_iface_id != input.body_iface_id {
            return Err(NetworkInterfaceUpdateError::MismatchedInterfaceId {
                path_iface_id: input.path_iface_id,
                body_iface_id: input.body_iface_id,
            });
        }

        Ok(Self {
            iface_id: input.path_iface_id,
            rx_rate_limiter: input
                .rx_rate_limiter
                .filter(|rate_limiter| rate_limiter.is_configured()),
            tx_rate_limiter: input
                .tx_rate_limiter
                .filter(|rate_limiter| rate_limiter.is_configured()),
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkInterfaceConfigs {
    configs: Vec<NetworkInterfaceConfig>,
}

/// A validated runtime-only network insertion whose packet I/O and endpoint are
/// not yet live.
#[derive(Debug)]
pub struct PreparedNetworkInterfaceConfigInsert {
    config: NetworkInterfaceConfig,
}

impl PreparedNetworkInterfaceConfigInsert {
    pub const fn config(&self) -> &NetworkInterfaceConfig {
        &self.config
    }
}

/// A validated runtime-only network removal whose live device is not yet
/// removed.
#[derive(Debug)]
pub struct PreparedNetworkInterfaceConfigRemoval {
    iface_id: String,
    index: usize,
}

impl PreparedNetworkInterfaceConfigRemoval {
    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }
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
        let existing_index = self
            .configs
            .iter()
            .position(|existing| existing.iface_id() == config.iface_id());

        if let Some(guest_mac) = config.guest_mac()
            && self.configs.iter().any(|existing| {
                existing.iface_id() != config.iface_id() && existing.guest_mac() == Some(guest_mac)
            })
        {
            return Err(NetworkInterfaceConfigError::GuestMacAddressInUse { guest_mac });
        }

        if existing_index.is_none() {
            validate_network_interface_count(self.configs.len().saturating_add(1))?;
        }

        if let Some(index) = existing_index {
            self.configs.remove(index);
        }

        self.configs.push(config);

        Ok(())
    }

    /// Validates and reserves storage for a post-start insertion without
    /// changing the configured network projection.
    pub fn prepare_runtime_insert(
        &mut self,
        input: NetworkInterfaceConfigInput,
    ) -> Result<PreparedNetworkInterfaceConfigInsert, NetworkRuntimeMutationError> {
        let config = input
            .validate()
            .map_err(NetworkRuntimeMutationError::InvalidConfig)?;
        if self
            .configs
            .iter()
            .any(|existing| existing.iface_id() == config.iface_id())
        {
            return Err(NetworkRuntimeMutationError::DuplicateInterface {
                iface_id: config.iface_id().to_string(),
            });
        }
        if let Some(guest_mac) = config.guest_mac()
            && self
                .configs
                .iter()
                .any(|existing| existing.guest_mac() == Some(guest_mac))
        {
            return Err(NetworkRuntimeMutationError::InvalidConfig(
                NetworkInterfaceConfigError::GuestMacAddressInUse { guest_mac },
            ));
        }
        validate_network_interface_count(self.configs.len().saturating_add(1))
            .map_err(NetworkRuntimeMutationError::InvalidConfig)?;
        self.configs
            .try_reserve_exact(1)
            .map_err(|_| NetworkRuntimeMutationError::ConfigurationAllocation)?;
        Ok(PreparedNetworkInterfaceConfigInsert { config })
    }

    /// Publishes a prepared runtime insertion after its packet I/O and live
    /// endpoint have committed successfully.
    pub fn commit_runtime_insert(&mut self, prepared: PreparedNetworkInterfaceConfigInsert) {
        debug_assert!(
            !self
                .configs
                .iter()
                .any(|existing| existing.iface_id() == prepared.config.iface_id())
        );
        debug_assert!(self.configs.len() < self.configs.capacity());
        self.configs.push(prepared.config);
    }

    /// Validates a post-start removal without changing the configured network
    /// projection.
    pub fn prepare_runtime_removal(
        &self,
        iface_id: &str,
    ) -> Result<PreparedNetworkInterfaceConfigRemoval, NetworkRuntimeMutationError> {
        match validate_interface_id(InterfaceIdSource::Path, iface_id) {
            Ok(()) => {}
            Err(NetworkInterfaceConfigError::EmptyInterfaceId { .. }) => {
                return Err(NetworkRuntimeMutationError::EmptyInterfaceId);
            }
            Err(NetworkInterfaceConfigError::InvalidInterfaceId { .. }) => {
                return Err(NetworkRuntimeMutationError::InvalidInterfaceId {
                    iface_id: iface_id.to_string(),
                });
            }
            Err(_) => {
                return Err(NetworkRuntimeMutationError::InvalidInterfaceId {
                    iface_id: iface_id.to_string(),
                });
            }
        }
        let Some((index, _)) = self
            .configs
            .iter()
            .enumerate()
            .find(|(_, config)| config.iface_id() == iface_id)
        else {
            return Err(NetworkRuntimeMutationError::UnknownInterface {
                iface_id: iface_id.to_string(),
            });
        };
        Ok(PreparedNetworkInterfaceConfigRemoval {
            iface_id: iface_id.to_string(),
            index,
        })
    }

    /// Commits a prepared removal after live teardown succeeds.
    pub fn commit_runtime_removal(&mut self, prepared: PreparedNetworkInterfaceConfigRemoval) {
        debug_assert_eq!(
            self.configs
                .get(prepared.index)
                .map(NetworkInterfaceConfig::iface_id),
            Some(prepared.iface_id.as_str())
        );
        self.configs.remove(prepared.index);
    }

    pub fn validate_update(
        &self,
        input: NetworkInterfaceUpdateInput,
    ) -> Result<NetworkInterfaceUpdate, NetworkInterfaceUpdateError> {
        let update = input.validate()?;

        if !self
            .configs
            .iter()
            .any(|config| config.iface_id() == update.iface_id())
        {
            return Err(NetworkInterfaceUpdateError::UnknownInterface {
                iface_id: update.iface_id().to_string(),
            });
        }

        Ok(update)
    }

    pub fn prepare_update(
        &self,
        input: NetworkInterfaceUpdateInput,
    ) -> Result<(NetworkInterfaceUpdate, NetworkInterfaceConfig), NetworkInterfaceUpdateError> {
        let update = self.validate_update(input)?;
        let Some(existing) = self
            .configs
            .iter()
            .find(|config| config.iface_id() == update.iface_id())
        else {
            return Err(NetworkInterfaceUpdateError::UnknownInterface {
                iface_id: update.iface_id().to_string(),
            });
        };
        let config = existing.updated(&update);

        Ok((update, config))
    }

    pub fn commit_update(
        &mut self,
        config: NetworkInterfaceConfig,
    ) -> Result<(), NetworkInterfaceUpdateError> {
        let iface_id = config.iface_id().to_string();
        let Some(existing) = self
            .configs
            .iter_mut()
            .find(|existing| existing.iface_id() == iface_id)
        else {
            return Err(NetworkInterfaceUpdateError::UnknownInterface { iface_id });
        };

        *existing = config;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct VirtioNetworkConfigSpace {
    guest_mac: Option<GuestMacAddress>,
    mtu: Option<u16>,
    network_features: u64,
    metrics: Option<VirtioNetworkTransportMetrics>,
}

impl PartialEq for VirtioNetworkConfigSpace {
    fn eq(&self, other: &Self) -> bool {
        self.guest_mac == other.guest_mac
            && self.mtu == other.mtu
            && self.network_features == other.network_features
    }
}

impl Eq for VirtioNetworkConfigSpace {}

impl VirtioNetworkConfigSpace {
    pub const fn new(guest_mac: Option<GuestMacAddress>, mtu: Option<u16>) -> Self {
        Self::with_feature_capabilities(
            guest_mac,
            mtu,
            VirtioNetworkFeatureCapabilities::complete_software(),
        )
    }

    pub const fn with_feature_capabilities(
        guest_mac: Option<GuestMacAddress>,
        mtu: Option<u16>,
        capabilities: VirtioNetworkFeatureCapabilities,
    ) -> Self {
        Self {
            guest_mac,
            mtu,
            network_features: capabilities.feature_bits(),
            metrics: None,
        }
    }

    pub const fn guest_mac(&self) -> Option<GuestMacAddress> {
        self.guest_mac
    }

    pub const fn mtu(&self) -> Option<u16> {
        self.mtu
    }

    pub const fn available_features(&self) -> u64 {
        let mut features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_INDIRECT_DESC)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX)
            | self.network_features;
        if self.guest_mac.is_some() {
            features |= virtio_feature_bit(VIRTIO_NET_F_MAC);
        }
        if self.mtu.is_some() {
            features |= virtio_feature_bit(VIRTIO_NET_F_MTU);
        }

        features
    }

    const fn mac_bytes(&self) -> Option<[u8; VIRTIO_NET_CONFIG_MAC_SIZE]> {
        match self.guest_mac {
            Some(guest_mac) => Some(guest_mac.octets()),
            None => None,
        }
    }

    pub fn attach_metrics(&mut self, metrics: SharedNetworkInterfaceMetrics) {
        self.metrics = Some(VirtioNetworkTransportMetrics::for_interface(metrics));
    }

    fn attach_metrics_with_aggregate(
        &mut self,
        interface: SharedNetworkInterfaceMetrics,
        aggregate: SharedNetworkInterfaceMetrics,
    ) {
        self.metrics = Some(VirtioNetworkTransportMetrics::with_aggregate(
            interface, aggregate,
        ));
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioNetworkConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let result = read_virtio_network_config_bytes(self.mac_bytes(), self.mtu, access);
        if result.is_err()
            && let Some(metrics) = &self.metrics
        {
            metrics.record_config_failure();
        }
        result
    }

    fn write_device_config(
        &mut self,
        access: VirtioMmioDeviceConfigAccess,
        _data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        let error = VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        };
        if let Some(metrics) = &self.metrics {
            metrics.record_config_failure();
        }
        Err(error)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioNetworkTxHeader {
    flags: u8,
    gso_type: u8,
    header_len: u16,
    gso_size: u16,
    checksum_start: u16,
    checksum_offset: u16,
    num_buffers: u16,
}

impl VirtioNetworkTxHeader {
    pub const fn new() -> Self {
        Self {
            flags: 0,
            gso_type: VIRTIO_NET_HDR_GSO_NONE,
            header_len: 0,
            gso_size: 0,
            checksum_start: 0,
            checksum_offset: 0,
            num_buffers: 0,
        }
    }

    pub const fn with_flags(mut self, flags: u8) -> Self {
        self.flags = flags;
        self
    }

    pub const fn with_gso_type(mut self, gso_type: u8) -> Self {
        self.gso_type = gso_type;
        self
    }

    pub const fn with_header_len(mut self, header_len: u16) -> Self {
        self.header_len = header_len;
        self
    }

    pub const fn with_gso_size(mut self, gso_size: u16) -> Self {
        self.gso_size = gso_size;
        self
    }

    pub const fn with_checksum_start(mut self, checksum_start: u16) -> Self {
        self.checksum_start = checksum_start;
        self
    }

    pub const fn with_checksum_offset(mut self, checksum_offset: u16) -> Self {
        self.checksum_offset = checksum_offset;
        self
    }

    pub const fn with_num_buffers(mut self, num_buffers: u16) -> Self {
        self.num_buffers = num_buffers;
        self
    }

    pub const fn flags(self) -> u8 {
        self.flags
    }

    pub const fn gso_type(self) -> u8 {
        self.gso_type
    }

    pub const fn header_len(self) -> u16 {
        self.header_len
    }

    pub const fn gso_size(self) -> u16 {
        self.gso_size
    }

    pub const fn checksum_start(self) -> u16 {
        self.checksum_start
    }

    pub const fn checksum_offset(self) -> u16 {
        self.checksum_offset
    }

    pub const fn num_buffers(self) -> u16 {
        self.num_buffers
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkTxPayloadSegment {
    descriptor_index: u16,
    address: GuestAddress,
    len: u32,
}

impl VirtioNetworkTxPayloadSegment {
    const fn new(descriptor_index: u16, address: GuestAddress, len: u32) -> Self {
        Self {
            descriptor_index,
            address,
            len,
        }
    }

    pub const fn descriptor_index(self) -> u16 {
        self.descriptor_index
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn len(self) -> u32 {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioNetworkTxFrame {
    descriptor_head: u16,
    header: VirtioNetworkTxHeader,
    payload_segments: Vec<VirtioNetworkTxPayloadSegment>,
    payload_len: u64,
    negotiated_features: u64,
}

impl VirtioNetworkTxFrame {
    pub fn parse(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioNetworkTxFrameParseError> {
        Self::parse_with_features(memory, chain, 0)
    }

    pub fn parse_with_features(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
        negotiated_features: u64,
    ) -> Result<Self, VirtioNetworkTxFrameParseError> {
        let descriptor_head = chain.head_index();
        let header_descriptor = network_descriptor_at(chain, 0, 1)?;
        validate_tx_header_descriptor(header_descriptor)?;
        let header = read_virtio_network_tx_header(memory, header_descriptor)?;

        let mut payload_segments = Vec::new();
        payload_segments
            .try_reserve_exact(chain.len())
            .map_err(
                |source| VirtioNetworkTxFrameParseError::PayloadSegmentsAllocationFailed {
                    descriptor_count: chain.len(),
                    source,
                },
            )?;

        let mut payload_len = 0;
        if let Some(segment) = header_descriptor_payload_segment(header_descriptor)? {
            payload_len =
                push_tx_payload_segment(memory, &mut payload_segments, payload_len, segment)?;
        }

        for descriptor in chain.descriptors().iter().skip(1) {
            validate_tx_payload_descriptor(descriptor)?;
            let segment = VirtioNetworkTxPayloadSegment::new(
                descriptor.index(),
                descriptor.address(),
                descriptor.len(),
            );
            payload_len =
                push_tx_payload_segment(memory, &mut payload_segments, payload_len, segment)?;
        }

        if payload_segments.is_empty() {
            return Err(VirtioNetworkTxFrameParseError::MissingPayload { descriptor_head });
        }

        Ok(Self {
            descriptor_head,
            header,
            payload_segments,
            payload_len,
            negotiated_features,
        })
    }

    pub const fn descriptor_head(&self) -> u16 {
        self.descriptor_head
    }

    pub const fn header(&self) -> VirtioNetworkTxHeader {
        self.header
    }

    pub fn payload_segments(&self) -> &[VirtioNetworkTxPayloadSegment] {
        &self.payload_segments
    }

    pub const fn payload_len(&self) -> u64 {
        self.payload_len
    }

    pub const fn negotiated_features(&self) -> u64 {
        self.negotiated_features
    }

    pub fn frame_len(&self) -> u64 {
        u64::from(VIRTIO_NET_TX_HEADER_SIZE) + self.payload_len
    }

    pub fn prepare_packet(
        &self,
        memory: &GuestMemory,
    ) -> Result<VirtioNetworkPacketPlan, VirtioNetworkTxPacketPrepareError> {
        let packet_len = usize::try_from(self.payload_len).map_err(|_| {
            VirtioNetworkTxPacketPrepareError::PayloadLengthTooLarge {
                len: self.payload_len,
            }
        })?;
        let mut packet = Vec::new();
        packet.try_reserve_exact(packet_len).map_err(|source| {
            VirtioNetworkTxPacketPrepareError::PacketAllocation {
                len: packet_len,
                source,
            }
        })?;
        for segment in &self.payload_segments {
            let segment_len = usize::try_from(segment.len()).map_err(|_| {
                VirtioNetworkTxPacketPrepareError::SegmentLengthTooLarge {
                    descriptor_index: segment.descriptor_index(),
                    len: segment.len(),
                }
            })?;
            let start = packet.len();
            let end = start.checked_add(segment_len).ok_or(
                VirtioNetworkTxPacketPrepareError::PayloadLengthTooLarge {
                    len: self.payload_len,
                },
            )?;
            packet.resize(end, 0);
            let destination = packet.get_mut(start..end).ok_or(
                VirtioNetworkTxPacketPrepareError::PayloadLengthTooLarge {
                    len: self.payload_len,
                },
            )?;
            memory
                .read_slice(destination, segment.address())
                .map_err(|source| VirtioNetworkTxPacketPrepareError::SegmentRead {
                    descriptor_index: segment.descriptor_index(),
                    source,
                })?;
        }
        VirtioNetworkPacketPlan::prepare(self.header, self.negotiated_features, packet)
            .map_err(VirtioNetworkTxPacketPrepareError::Semantic)
    }
}

#[derive(Debug)]
pub enum VirtioNetworkTxPacketPrepareError {
    PayloadLengthTooLarge {
        len: u64,
    },
    PacketAllocation {
        len: usize,
        source: TryReserveError,
    },
    SegmentLengthTooLarge {
        descriptor_index: u16,
        len: u32,
    },
    SegmentRead {
        descriptor_index: u16,
        source: GuestMemoryAccessError,
    },
    Semantic(VirtioNetworkPacketPlanError),
}

impl fmt::Display for VirtioNetworkTxPacketPrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PayloadLengthTooLarge { len } => {
                write!(
                    formatter,
                    "virtio-net TX payload length {len} does not fit host usize"
                )
            }
            Self::PacketAllocation { len, source } => write!(
                formatter,
                "failed to reserve virtio-net TX packet buffer of {len} bytes: {source}"
            ),
            Self::SegmentLengthTooLarge {
                descriptor_index,
                len,
            } => write!(
                formatter,
                "virtio-net TX payload descriptor {descriptor_index} length {len} does not fit host usize"
            ),
            Self::SegmentRead {
                descriptor_index,
                source,
            } => write!(
                formatter,
                "failed to read virtio-net TX payload descriptor {descriptor_index}: {source}"
            ),
            Self::Semantic(source) => {
                write!(formatter, "invalid virtio-net TX semantics: {source}")
            }
        }
    }
}

impl std::error::Error for VirtioNetworkTxPacketPrepareError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PacketAllocation { source, .. } => Some(source),
            Self::SegmentRead { source, .. } => Some(source),
            Self::Semantic(source) => Some(source),
            Self::PayloadLengthTooLarge { .. } | Self::SegmentLengthTooLarge { .. } => None,
        }
    }
}

pub trait VirtioNetworkTxPacketSink {
    fn transmit_frame(
        &mut self,
        memory: &GuestMemory,
        frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError>;

    /// Transmits a frame whose guest bytes and packet semantics were captured
    /// before used-ring publication. Sinks that inspect packet bytes must
    /// override this method and use `packet` instead of rereading guest memory.
    fn transmit_prepared_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
        _packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        Err(VirtioNetworkTxPacketSinkError::new(
            "virtio-net sink does not consume prevalidated packet plans",
        ))
    }

    /// Returns whether this sink implements the two-phase bounded batch seam.
    fn supports_staged_batch(&self) -> bool {
        false
    }

    /// Copies all guest-owned bytes needed by one frame into sink-owned
    /// staging before the used descriptor is published.
    fn stage_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        Err(VirtioNetworkTxPacketSinkError::new(
            "virtio-net sink does not support staged batches",
        ))
    }

    /// Stages a prevalidated, owned packet plan. Production sinks that inspect
    /// packet bytes must override this method so guest memory is not reread.
    fn stage_prepared_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
        _packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
        Err(VirtioNetworkTxPacketSinkError::new(
            "virtio-net sink does not stage prevalidated packet plans",
        ))
    }

    /// Commits the one currently staged frame after used-ring publication.
    fn commit_staged_frame(&mut self) -> VirtioNetworkTxPacketCommit {
        VirtioNetworkTxPacketCommit::Immediate(Err(VirtioNetworkTxPacketSinkError::new(
            "virtio-net sink does not support staged batches",
        )))
    }

    /// Discards the one uncommitted staged frame after a publication failure.
    fn discard_staged_frame(&mut self) {}

    /// Flushes committed frames and appends exactly one ordered result per
    /// committed frame to `results`.
    fn flush_staged_frames(
        &mut self,
        _results: &mut Vec<
            Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError>,
        >,
    ) {
    }

    fn take_backend_metrics(&mut self) -> VirtioNetworkBackendMetrics {
        VirtioNetworkBackendMetrics::default()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioNetworkLatencyAggregate {
    min_us: u64,
    max_us: u64,
    sum_us: u64,
    samples: u64,
}

impl VirtioNetworkLatencyAggregate {
    pub const fn new(min_us: u64, max_us: u64, sum_us: u64, samples: u64) -> Self {
        if samples == 0 {
            return Self {
                min_us: 0,
                max_us: 0,
                sum_us: 0,
                samples: 0,
            };
        }
        Self {
            min_us,
            max_us,
            sum_us,
            samples,
        }
    }

    pub const fn min_us(self) -> u64 {
        self.min_us
    }

    pub const fn max_us(self) -> u64 {
        self.max_us
    }

    pub const fn sum_us(self) -> u64 {
        self.sum_us
    }

    pub const fn samples(self) -> u64 {
        self.samples
    }

    pub const fn from_sample(duration: Duration) -> Self {
        let micros = duration.as_micros();
        let value = if micros > u64::MAX as u128 {
            u64::MAX
        } else {
            micros as u64
        };
        Self::new(value, value, value, 1)
    }

    pub const fn merged_with(self, other: Self) -> Self {
        if self.samples == 0 {
            return other;
        }
        if other.samples == 0 {
            return self;
        }
        Self {
            min_us: if self.min_us < other.min_us {
                self.min_us
            } else {
                other.min_us
            },
            max_us: if self.max_us > other.max_us {
                self.max_us
            } else {
                other.max_us
            },
            sum_us: self.sum_us.saturating_add(other.sum_us),
            samples: self.samples.saturating_add(other.samples),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioNetworkBackendMetrics {
    vmnet_read_count: u64,
    vmnet_read_fails: u64,
    vmnet_read_packets_count: u64,
    vmnet_read_partial_batches: u64,
    vmnet_write_count: u64,
    vmnet_write_fails: u64,
    vmnet_write_packets_count: u64,
    vmnet_write_partial_batches: u64,
    tx_spoofed_mac_count: u64,
    vmnet_read_latency: VirtioNetworkLatencyAggregate,
    vmnet_write_latency: VirtioNetworkLatencyAggregate,
}

impl VirtioNetworkBackendMetrics {
    pub const fn vmnet_read_count(self) -> u64 {
        self.vmnet_read_count
    }

    pub const fn vmnet_read_fails(self) -> u64 {
        self.vmnet_read_fails
    }

    pub const fn vmnet_read_packets_count(self) -> u64 {
        self.vmnet_read_packets_count
    }

    pub const fn vmnet_read_partial_batches(self) -> u64 {
        self.vmnet_read_partial_batches
    }

    pub const fn vmnet_write_count(self) -> u64 {
        self.vmnet_write_count
    }

    pub const fn vmnet_write_fails(self) -> u64 {
        self.vmnet_write_fails
    }

    pub const fn vmnet_write_packets_count(self) -> u64 {
        self.vmnet_write_packets_count
    }

    pub const fn vmnet_write_partial_batches(self) -> u64 {
        self.vmnet_write_partial_batches
    }

    pub const fn tx_spoofed_mac_count(self) -> u64 {
        self.tx_spoofed_mac_count
    }

    pub const fn vmnet_read_latency(self) -> VirtioNetworkLatencyAggregate {
        self.vmnet_read_latency
    }

    pub const fn vmnet_write_latency(self) -> VirtioNetworkLatencyAggregate {
        self.vmnet_write_latency
    }

    pub fn record_vmnet_read(
        &mut self,
        requested: usize,
        completed: Result<usize, ()>,
        duration: Duration,
    ) {
        self.vmnet_read_count = self.vmnet_read_count.saturating_add(1);
        self.vmnet_read_latency = self
            .vmnet_read_latency
            .merged_with(VirtioNetworkLatencyAggregate::from_sample(duration));
        match completed {
            Ok(completed) => {
                self.vmnet_read_packets_count = self
                    .vmnet_read_packets_count
                    .saturating_add(usize_to_u64_saturating(completed));
                if completed != 0 && completed < requested {
                    self.vmnet_read_partial_batches =
                        self.vmnet_read_partial_batches.saturating_add(1);
                }
            }
            Err(()) => self.vmnet_read_fails = self.vmnet_read_fails.saturating_add(1),
        }
    }

    pub fn record_vmnet_write(
        &mut self,
        requested: usize,
        completed: Result<usize, ()>,
        duration: Duration,
    ) {
        self.vmnet_write_count = self.vmnet_write_count.saturating_add(1);
        self.vmnet_write_latency = self
            .vmnet_write_latency
            .merged_with(VirtioNetworkLatencyAggregate::from_sample(duration));
        match completed {
            Ok(completed) => {
                self.vmnet_write_packets_count = self
                    .vmnet_write_packets_count
                    .saturating_add(usize_to_u64_saturating(completed));
                if completed < requested {
                    self.vmnet_write_partial_batches =
                        self.vmnet_write_partial_batches.saturating_add(1);
                }
            }
            Err(()) => self.vmnet_write_fails = self.vmnet_write_fails.saturating_add(1),
        }
    }

    pub fn record_spoofed_mac(&mut self) {
        self.tx_spoofed_mac_count = self.tx_spoofed_mac_count.saturating_add(1);
    }

    pub const fn merged_with(self, other: Self) -> Self {
        Self {
            vmnet_read_count: self.vmnet_read_count.saturating_add(other.vmnet_read_count),
            vmnet_read_fails: self.vmnet_read_fails.saturating_add(other.vmnet_read_fails),
            vmnet_read_packets_count: self
                .vmnet_read_packets_count
                .saturating_add(other.vmnet_read_packets_count),
            vmnet_read_partial_batches: self
                .vmnet_read_partial_batches
                .saturating_add(other.vmnet_read_partial_batches),
            vmnet_write_count: self
                .vmnet_write_count
                .saturating_add(other.vmnet_write_count),
            vmnet_write_fails: self
                .vmnet_write_fails
                .saturating_add(other.vmnet_write_fails),
            vmnet_write_packets_count: self
                .vmnet_write_packets_count
                .saturating_add(other.vmnet_write_packets_count),
            vmnet_write_partial_batches: self
                .vmnet_write_partial_batches
                .saturating_add(other.vmnet_write_partial_batches),
            tx_spoofed_mac_count: self
                .tx_spoofed_mac_count
                .saturating_add(other.tx_spoofed_mac_count),
            vmnet_read_latency: self
                .vmnet_read_latency
                .merged_with(other.vmnet_read_latency),
            vmnet_write_latency: self
                .vmnet_write_latency
                .merged_with(other.vmnet_write_latency),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioNetworkTxPacketStage {
    /// The frame is staged. A detour or other immediate side effect requires
    /// all earlier committed external frames to flush first.
    Staged { flush_before_commit: bool },
    /// The current committed batch must flush before this frame can fit.
    FlushRequired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioNetworkTxPacketCommit {
    Deferred,
    Immediate(Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioNetworkTxPacketDisposition {
    Forwarded,
    Detoured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioNetworkTxPacketSinkError {
    message: String,
}

impl VirtioNetworkTxPacketSinkError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for VirtioNetworkTxPacketSinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VirtioNetworkTxPacketSinkError {}

#[derive(Debug, Default)]
struct NoopVirtioNetworkTxPacketSink;

impl VirtioNetworkTxPacketSink for NoopVirtioNetworkTxPacketSink {
    fn transmit_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        Ok(VirtioNetworkTxPacketDisposition::Forwarded)
    }

    fn transmit_prepared_frame(
        &mut self,
        _memory: &GuestMemory,
        _frame: &VirtioNetworkTxFrame,
        _packet: &VirtioNetworkPacketPlan,
    ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
        Ok(VirtioNetworkTxPacketDisposition::Forwarded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioNetworkRateLimiter {
    bandwidth: Option<TokenBucket>,
    ops: Option<TokenBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VirtioNetworkRateLimiterReservation {
    bandwidth: Option<TokenBucketSnapshot>,
    ops: Option<TokenBucketSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioNetworkRateLimiterReduction {
    Allowed(VirtioNetworkRateLimiterReservation),
    Throttled { retry_after: Duration },
}

impl VirtioNetworkRateLimiter {
    pub fn new(config: NetworkRateLimiterConfig) -> Option<Self> {
        Self::new_at(config, Instant::now())
    }

    fn new_at(config: NetworkRateLimiterConfig, now: Instant) -> Option<Self> {
        let bandwidth = config
            .bandwidth()
            .and_then(|bucket| TokenBucket::new_at(bucket.token_bucket_config(), now));
        let ops = config
            .ops()
            .and_then(|bucket| TokenBucket::new_at(bucket.token_bucket_config(), now));

        if bandwidth.is_none() && ops.is_none() {
            None
        } else {
            Some(Self { bandwidth, ops })
        }
    }

    fn updated_at(
        existing: Option<&Self>,
        update: NetworkRateLimiterConfig,
        now: Instant,
    ) -> Option<Self> {
        let bandwidth = match update.bandwidth() {
            Some(config) => TokenBucket::new_at(config.token_bucket_config(), now),
            None => existing.and_then(|limiter| limiter.bandwidth.clone()),
        };
        let ops = match update.ops() {
            Some(config) => TokenBucket::new_at(config.token_bucket_config(), now),
            None => existing.and_then(|limiter| limiter.ops.clone()),
        };

        if bandwidth.is_none() && ops.is_none() {
            None
        } else {
            Some(Self { bandwidth, ops })
        }
    }

    fn reduce_at(&mut self, bytes: u64, now: Instant) -> VirtioNetworkRateLimiterReduction {
        let reservation = VirtioNetworkRateLimiterReservation {
            bandwidth: self.bandwidth.as_ref().map(TokenBucket::snapshot),
            ops: self.ops.as_ref().map(TokenBucket::snapshot),
        };

        if let Some(ops) = self.ops.as_mut()
            && let Some(retry_after) = ops.reduce_with_retry_at(1, now).retry_after()
        {
            self.restore(reservation);
            return VirtioNetworkRateLimiterReduction::Throttled { retry_after };
        }
        if let Some(bandwidth) = self.bandwidth.as_mut()
            && let Some(retry_after) = bandwidth
                .reduce_allow_overconsumption_with_retry_at(bytes, now)
                .retry_after()
        {
            self.restore(reservation);
            return VirtioNetworkRateLimiterReduction::Throttled { retry_after };
        }

        VirtioNetworkRateLimiterReduction::Allowed(reservation)
    }

    fn restore(&mut self, reservation: VirtioNetworkRateLimiterReservation) {
        if let (Some(bucket), Some(snapshot)) = (self.bandwidth.as_mut(), reservation.bandwidth) {
            bucket.restore(snapshot);
        }
        if let (Some(bucket), Some(snapshot)) = (self.ops.as_mut(), reservation.ops) {
            bucket.restore(snapshot);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkRxPacket<'a> {
    bytes: &'a [u8],
}

impl fmt::Debug for VirtioNetworkRxPacket<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkRxPacket")
            .field("bytes", &"[REDACTED]")
            .field("len", &self.bytes.len())
            .finish()
    }
}

impl<'a> VirtioNetworkRxPacket<'a> {
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    pub const fn len(self) -> usize {
        self.bytes.len()
    }

    pub const fn is_empty(self) -> bool {
        self.bytes.is_empty()
    }
}

pub trait VirtioNetworkRxPacketSource {
    /// Starts one bounded owner-side RX dispatch pass.
    fn begin_rx_dispatch(&mut self) {}

    /// Returns whether this source owns persistent host-readiness or a retained
    /// host packet that may be consumed during an already-entered owner pass.
    fn host_readiness_hint(&self) -> bool {
        false
    }

    /// Returns whether an RX retry is known to be useful after TX dispatch.
    ///
    /// Implementations must keep this cheap, non-consuming, and nonblocking.
    /// Sources that would need to perform host I/O to answer should keep the
    /// default `false` value and wait for a normal RX queue notification.
    fn retry_after_tx_hint(&self) -> bool {
        false
    }

    fn peek_packet(
        &mut self,
    ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError>;

    fn consume_packet(&mut self);

    fn take_backend_metrics(&mut self) -> VirtioNetworkBackendMetrics {
        VirtioNetworkBackendMetrics::default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioNetworkRxPacketSourceError {
    message: String,
}

impl VirtioNetworkRxPacketSourceError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for VirtioNetworkRxPacketSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VirtioNetworkRxPacketSourceError {}

#[derive(Debug, Default)]
struct EmptyVirtioNetworkRxPacketSource;

impl VirtioNetworkRxPacketSource for EmptyVirtioNetworkRxPacketSource {
    fn peek_packet(
        &mut self,
    ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
        Ok(None)
    }

    fn consume_packet(&mut self) {}
}

#[derive(Debug)]
pub enum VirtioNetworkTxFrameParseError {
    DescriptorChainTooShort {
        expected: usize,
        actual: usize,
    },
    HeaderDescriptorWriteOnly {
        index: u16,
    },
    HeaderDescriptorTooSmall {
        index: u16,
        len: u32,
        min: u32,
    },
    InvalidHeaderLayout,
    ReadHeader {
        address: GuestAddress,
        source: GuestMemoryAccessError,
    },
    MissingPayload {
        descriptor_head: u16,
    },
    PayloadDescriptorWriteOnly {
        index: u16,
    },
    PayloadDescriptorEmpty {
        index: u16,
    },
    PayloadDescriptorRangeOverflow {
        index: u16,
        address: GuestAddress,
        len: u32,
    },
    PayloadDescriptorAccess {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    FrameTooLarge {
        len: u64,
        max: u64,
    },
    PayloadSegmentsAllocationFailed {
        descriptor_count: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtioNetworkTxFrameParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DescriptorChainTooShort { expected, actual } => {
                write!(
                    f,
                    "virtio-net TX descriptor chain has {actual} descriptors; expected at least {expected}"
                )
            }
            Self::HeaderDescriptorWriteOnly { index } => {
                write!(f, "virtio-net TX header descriptor {index} is write-only")
            }
            Self::HeaderDescriptorTooSmall { index, len, min } => {
                write!(
                    f,
                    "virtio-net TX header descriptor {index} has length {len}; expected at least {min}"
                )
            }
            Self::InvalidHeaderLayout => f.write_str("virtio-net TX header layout is invalid"),
            Self::ReadHeader { address, source } => {
                write!(
                    f,
                    "failed to read virtio-net TX header at {address}: {source}"
                )
            }
            Self::MissingPayload { descriptor_head } => {
                write!(
                    f,
                    "virtio-net TX descriptor chain headed by {descriptor_head} has no packet payload"
                )
            }
            Self::PayloadDescriptorWriteOnly { index } => {
                write!(f, "virtio-net TX payload descriptor {index} is write-only")
            }
            Self::PayloadDescriptorEmpty { index } => {
                write!(f, "virtio-net TX payload descriptor {index} is empty")
            }
            Self::PayloadDescriptorRangeOverflow {
                index,
                address,
                len,
            } => {
                write!(
                    f,
                    "virtio-net TX payload descriptor {index} at {address} with length {len} overflows address space"
                )
            }
            Self::PayloadDescriptorAccess {
                index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "virtio-net TX payload descriptor {index} at {address} with length {len} is not fully mapped: {source}"
                )
            }
            Self::FrameTooLarge { len, max } => {
                write!(f, "virtio-net TX frame length {len} exceeds maximum {max}")
            }
            Self::PayloadSegmentsAllocationFailed {
                descriptor_count,
                source,
            } => {
                write!(
                    f,
                    "failed to reserve virtio-net TX payload segment storage for {descriptor_count} descriptors: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioNetworkTxFrameParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadHeader { source, .. } | Self::PayloadDescriptorAccess { source, .. } => {
                Some(source)
            }
            Self::PayloadSegmentsAllocationFailed { source, .. } => Some(source),
            Self::DescriptorChainTooShort { .. }
            | Self::HeaderDescriptorWriteOnly { .. }
            | Self::HeaderDescriptorTooSmall { .. }
            | Self::InvalidHeaderLayout
            | Self::MissingPayload { .. }
            | Self::PayloadDescriptorWriteOnly { .. }
            | Self::PayloadDescriptorEmpty { .. }
            | Self::PayloadDescriptorRangeOverflow { .. }
            | Self::FrameTooLarge { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkRxBufferSegment {
    descriptor_index: u16,
    address: GuestAddress,
    len: u32,
}

impl VirtioNetworkRxBufferSegment {
    const fn new(descriptor_index: u16, address: GuestAddress, len: u32) -> Self {
        Self {
            descriptor_index,
            address,
            len,
        }
    }

    pub const fn descriptor_index(self) -> u16 {
        self.descriptor_index
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn len(self) -> u32 {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioNetworkRxBuffer {
    descriptor_head: u16,
    segments: Vec<VirtioNetworkRxBufferSegment>,
    len: u64,
}

impl VirtioNetworkRxBuffer {
    pub fn parse(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioNetworkRxBufferParseError> {
        Self::parse_with_minimum(memory, chain, VIRTIO_NET_RX_MIN_BUFFER_SIZE)
    }

    fn parse_with_minimum(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
        minimum_len: u64,
    ) -> Result<Self, VirtioNetworkRxBufferParseError> {
        if chain.is_empty() {
            return Err(VirtioNetworkRxBufferParseError::DescriptorChainTooShort {
                expected: 1,
                actual: chain.len(),
            });
        }

        let descriptor_head = chain.head_index();
        let mut segments = Vec::new();
        segments.try_reserve_exact(chain.len()).map_err(|source| {
            VirtioNetworkRxBufferParseError::BufferSegmentsAllocationFailed {
                descriptor_count: chain.len(),
                source,
            }
        })?;

        let mut len = 0;
        for descriptor in chain.descriptors() {
            validate_rx_buffer_descriptor(descriptor)?;
            let segment = VirtioNetworkRxBufferSegment::new(
                descriptor.index(),
                descriptor.address(),
                descriptor.len(),
            );
            len = push_rx_buffer_segment(memory, &mut segments, len, segment)?;
        }

        if len < minimum_len {
            return Err(VirtioNetworkRxBufferParseError::BufferTooSmall {
                len,
                min: minimum_len,
            });
        }

        Ok(Self {
            descriptor_head,
            segments,
            len,
        })
    }

    pub const fn descriptor_head(&self) -> u16 {
        self.descriptor_head
    }

    pub fn segments(&self) -> &[VirtioNetworkRxBufferSegment] {
        &self.segments
    }

    pub const fn len(&self) -> u64 {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Debug)]
pub enum VirtioNetworkRxBufferParseError {
    DescriptorChainTooShort {
        expected: usize,
        actual: usize,
    },
    BufferDescriptorReadOnly {
        index: u16,
    },
    BufferDescriptorEmpty {
        index: u16,
    },
    BufferDescriptorRangeOverflow {
        index: u16,
        address: GuestAddress,
        len: u32,
    },
    BufferDescriptorAccess {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    BufferLengthOverflow {
        current: u64,
        len: u32,
    },
    BufferTooSmall {
        len: u64,
        min: u64,
    },
    BufferSegmentsAllocationFailed {
        descriptor_count: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtioNetworkRxBufferParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DescriptorChainTooShort { expected, actual } => {
                write!(
                    f,
                    "virtio-net RX descriptor chain has {actual} descriptors; expected at least {expected}"
                )
            }
            Self::BufferDescriptorReadOnly { index } => {
                write!(f, "virtio-net RX buffer descriptor {index} is read-only")
            }
            Self::BufferDescriptorEmpty { index } => {
                write!(f, "virtio-net RX buffer descriptor {index} is empty")
            }
            Self::BufferDescriptorRangeOverflow {
                index,
                address,
                len,
            } => {
                write!(
                    f,
                    "virtio-net RX buffer descriptor {index} at {address} with length {len} overflows address space"
                )
            }
            Self::BufferDescriptorAccess {
                index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "virtio-net RX buffer descriptor {index} at {address} with length {len} is not fully mapped: {source}"
                )
            }
            Self::BufferLengthOverflow { current, len } => {
                write!(
                    f,
                    "virtio-net RX buffer length overflows when adding descriptor length {len} to current length {current}"
                )
            }
            Self::BufferTooSmall { len, min } => {
                write!(
                    f,
                    "virtio-net RX buffer length {len} is smaller than required minimum {min}"
                )
            }
            Self::BufferSegmentsAllocationFailed {
                descriptor_count,
                source,
            } => {
                write!(
                    f,
                    "failed to reserve virtio-net RX buffer segment storage for {descriptor_count} descriptors: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioNetworkRxBufferParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::BufferDescriptorAccess { source, .. } => Some(source),
            Self::BufferSegmentsAllocationFailed { source, .. } => Some(source),
            Self::DescriptorChainTooShort { .. }
            | Self::BufferDescriptorReadOnly { .. }
            | Self::BufferDescriptorEmpty { .. }
            | Self::BufferDescriptorRangeOverflow { .. }
            | Self::BufferLengthOverflow { .. }
            | Self::BufferTooSmall { .. } => None,
        }
    }
}

/// Detached token-bucket state used by the network capture contract.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkTokenBucketCaptureState {
    config: NetworkTokenBucketConfig,
    budget: u64,
    one_time_burst: u64,
    age_nanos: u64,
}

impl VirtioNetworkTokenBucketCaptureState {
    const fn from_persisted(
        config: NetworkTokenBucketConfig,
        state: PersistedTokenBucketState,
    ) -> Self {
        Self {
            config,
            budget: state.budget(),
            one_time_burst: state.one_time_burst(),
            age_nanos: state.age_nanos(),
        }
    }

    pub const fn config(self) -> NetworkTokenBucketConfig {
        self.config
    }

    pub const fn budget(self) -> u64 {
        self.budget
    }

    pub const fn one_time_burst(self) -> u64 {
        self.one_time_burst
    }

    pub const fn age_nanos(self) -> u64 {
        self.age_nanos
    }
}

impl fmt::Debug for VirtioNetworkTokenBucketCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkTokenBucketCaptureState")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Detached bandwidth and operations limiter state for one direction.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkRateLimiterCaptureState {
    bandwidth: Option<VirtioNetworkTokenBucketCaptureState>,
    ops: Option<VirtioNetworkTokenBucketCaptureState>,
}

impl VirtioNetworkRateLimiterCaptureState {
    pub const fn bandwidth(self) -> Option<VirtioNetworkTokenBucketCaptureState> {
        self.bandwidth
    }

    pub const fn ops(self) -> Option<VirtioNetworkTokenBucketCaptureState> {
        self.ops
    }

    pub const fn is_configured(self) -> bool {
        self.bandwidth.is_some() || self.ops.is_some()
    }
}

impl fmt::Debug for VirtioNetworkRateLimiterCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkRateLimiterCaptureState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioNetworkRateLimiterCaptureError {
    MissingRateLimiter,
    UnexpectedRateLimiter,
    MissingBandwidthBucket,
    UnexpectedBandwidthBucket,
    InvalidBandwidthBucket,
    MissingOpsBucket,
    UnexpectedOpsBucket,
    InvalidOpsBucket,
}

impl fmt::Display for VirtioNetworkRateLimiterCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingRateLimiter => "configured network rate limiter is missing",
            Self::UnexpectedRateLimiter => "unconfigured network rate limiter is present",
            Self::MissingBandwidthBucket => "configured network bandwidth bucket is missing",
            Self::UnexpectedBandwidthBucket => "unconfigured network bandwidth bucket is present",
            Self::InvalidBandwidthBucket => "network bandwidth bucket state is invalid",
            Self::MissingOpsBucket => "configured network operations bucket is missing",
            Self::UnexpectedOpsBucket => "unconfigured network operations bucket is present",
            Self::InvalidOpsBucket => "network operations bucket state is invalid",
        })
    }
}

impl std::error::Error for VirtioNetworkRateLimiterCaptureError {}

fn capture_network_token_bucket_state_at(
    config: Option<NetworkTokenBucketConfig>,
    bucket: Option<&TokenBucket>,
    now: Instant,
    missing: VirtioNetworkRateLimiterCaptureError,
    unexpected: VirtioNetworkRateLimiterCaptureError,
    invalid: VirtioNetworkRateLimiterCaptureError,
) -> Result<Option<VirtioNetworkTokenBucketCaptureState>, VirtioNetworkRateLimiterCaptureError> {
    match (config, bucket) {
        (Some(config), Some(bucket)) => bucket
            .persisted_state_at(config.token_bucket_config(), now)
            .map(|state| {
                Some(VirtioNetworkTokenBucketCaptureState::from_persisted(
                    config, state,
                ))
            })
            .map_err(|_: PersistedTokenBucketStateError| invalid),
        (Some(_), None) => Err(missing),
        (None, Some(_)) => Err(unexpected),
        (None, None) => Ok(None),
    }
}

fn capture_network_rate_limiter_state_at(
    config: Option<NetworkRateLimiterConfig>,
    limiter: Option<&VirtioNetworkRateLimiter>,
    now: Instant,
) -> Result<VirtioNetworkRateLimiterCaptureState, VirtioNetworkRateLimiterCaptureError> {
    let config = config.and_then(NetworkRateLimiterConfig::normalized);
    let (config, limiter) = match (config, limiter) {
        (Some(config), Some(limiter)) => (config, limiter),
        (Some(_), None) => return Err(VirtioNetworkRateLimiterCaptureError::MissingRateLimiter),
        (None, Some(_)) => {
            return Err(VirtioNetworkRateLimiterCaptureError::UnexpectedRateLimiter);
        }
        (None, None) => {
            return Ok(VirtioNetworkRateLimiterCaptureState {
                bandwidth: None,
                ops: None,
            });
        }
    };

    let bandwidth = capture_network_token_bucket_state_at(
        config.bandwidth(),
        limiter.bandwidth.as_ref(),
        now,
        VirtioNetworkRateLimiterCaptureError::MissingBandwidthBucket,
        VirtioNetworkRateLimiterCaptureError::UnexpectedBandwidthBucket,
        VirtioNetworkRateLimiterCaptureError::InvalidBandwidthBucket,
    )?;
    let ops = capture_network_token_bucket_state_at(
        config.ops(),
        limiter.ops.as_ref(),
        now,
        VirtioNetworkRateLimiterCaptureError::MissingOpsBucket,
        VirtioNetworkRateLimiterCaptureError::UnexpectedOpsBucket,
        VirtioNetworkRateLimiterCaptureError::InvalidOpsBucket,
    )?;
    Ok(VirtioNetworkRateLimiterCaptureState { bandwidth, ops })
}

/// Host-time-free retry disposition for reconstructible network work.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VirtioNetworkRetryCaptureState {
    None,
    Immediate,
    After { remaining_nanos: u64 },
}

impl VirtioNetworkRetryCaptureState {
    pub const fn has_retry(self) -> bool {
        !matches!(self, Self::None)
    }

    pub const fn remaining_nanos(self) -> Option<u64> {
        match self {
            Self::None | Self::Immediate => None,
            Self::After { remaining_nanos } => Some(remaining_nanos),
        }
    }
}

impl fmt::Debug for VirtioNetworkRetryCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let disposition = match self {
            Self::None => "none",
            Self::Immediate => "immediate",
            Self::After { .. } => "delayed",
        };
        formatter
            .debug_tuple("VirtioNetworkRetryCaptureState")
            .field(&disposition)
            .finish()
    }
}

fn network_retry_capture_state(
    retry_after: Duration,
) -> Result<VirtioNetworkRetryCaptureState, VirtioNetworkDeviceCaptureError> {
    let remaining_nanos = u64::try_from(retry_after.as_nanos())
        .map_err(|_| VirtioNetworkDeviceCaptureError::RetryDurationOverflow)?;
    if remaining_nanos == 0 {
        Ok(VirtioNetworkRetryCaptureState::Immediate)
    } else {
        Ok(VirtioNetworkRetryCaptureState::After { remaining_nanos })
    }
}

/// Detached queue cursors and negotiated behavior. Ring addresses live in the
/// paired transport value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkQueueCaptureState {
    next_available: u16,
    next_used: u16,
    event_idx_enabled: bool,
    negotiated_features: u64,
}

impl VirtioNetworkQueueCaptureState {
    pub const fn next_available(self) -> u16 {
        self.next_available
    }

    pub const fn next_used(self) -> u16 {
        self.next_used
    }

    pub const fn event_idx_enabled(self) -> bool {
        self.event_idx_enabled
    }

    pub const fn negotiated_features(self) -> u64 {
        self.negotiated_features
    }
}

impl fmt::Debug for VirtioNetworkQueueCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkQueueCaptureState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioNetworkQueueCaptureError {
    TransportMismatch,
    FeatureMismatch,
    AvailableRingInvalid,
    UsedRingInvalid,
    QueueRangeInvalid,
    QueueRangesOverlap,
    UsedCursorMismatch,
    AvailableCursorOutOfBounds,
    UnpublishedDescriptorCountMismatch,
    PendingDescriptorMissing,
    PendingDescriptorDuplicated,
}

impl fmt::Display for VirtioNetworkQueueCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TransportMismatch => "active network queue does not match its transport",
            Self::FeatureMismatch => "active network queue feature state does not match transport",
            Self::AvailableRingInvalid => "network available ring is invalid",
            Self::UsedRingInvalid => "network used ring is invalid",
            Self::QueueRangeInvalid => "network queue range is invalid",
            Self::QueueRangesOverlap => "network queue ranges overlap",
            Self::UsedCursorMismatch => "network used cursor does not match guest memory",
            Self::AvailableCursorOutOfBounds => {
                "network available cursor is inconsistent with guest memory"
            }
            Self::UnpublishedDescriptorCountMismatch => {
                "network queue has consumed-but-unpublished descriptors"
            }
            Self::PendingDescriptorMissing => {
                "rate-limited network queue does not retain an available descriptor"
            }
            Self::PendingDescriptorDuplicated => {
                "rate-limited network queue retains its descriptor more than once"
            }
        })
    }
}

impl std::error::Error for VirtioNetworkQueueCaptureError {}

fn capture_network_queue_state(
    transport: &VirtioMmioQueueState,
    available: &VirtqueueAvailableRing,
    used: &VirtqueueUsedRing,
    event_idx_enabled: bool,
    negotiated_features: u64,
    memory: &GuestMemory,
    pending_rate_limited_queue: bool,
) -> Result<VirtioNetworkQueueCaptureState, VirtioNetworkQueueCaptureError> {
    if !transport.ready()
        || transport.size() != available.queue_size()
        || transport.descriptor_table() != available.descriptor_table()
        || transport.driver_ring() != available.available_ring()
        || transport.device_ring() != used.used_ring()
        || available.queue_size() != used.queue_size()
    {
        return Err(VirtioNetworkQueueCaptureError::TransportMismatch);
    }
    let expected_event_idx =
        virtio_feature_enabled(negotiated_features, VIRTIO_RING_FEATURE_EVENT_IDX);
    let expected_indirect =
        virtio_feature_enabled(negotiated_features, VIRTIO_RING_FEATURE_INDIRECT_DESC);
    if event_idx_enabled != expected_event_idx
        || available.descriptor_chain_options().indirect_descriptors() != expected_indirect
    {
        return Err(VirtioNetworkQueueCaptureError::FeatureMismatch);
    }
    available
        .validate_mapped(memory)
        .map_err(|_| VirtioNetworkQueueCaptureError::AvailableRingInvalid)?;
    used.validate_mapped(memory)
        .map_err(|_| VirtioNetworkQueueCaptureError::UsedRingInvalid)?;
    let ranges = network_queue_ranges(available, used)?;
    if ranges[0].overlaps(ranges[1])
        || ranges[0].overlaps(ranges[2])
        || ranges[1].overlaps(ranges[2])
    {
        return Err(VirtioNetworkQueueCaptureError::QueueRangesOverlap);
    }

    let used_index = used
        .used_index(memory)
        .map_err(|_| VirtioNetworkQueueCaptureError::UsedRingInvalid)?;
    if used_index != used.next_used() {
        return Err(VirtioNetworkQueueCaptureError::UsedCursorMismatch);
    }
    let available_index = available
        .available_index(memory)
        .map_err(|_| VirtioNetworkQueueCaptureError::AvailableRingInvalid)?;
    let available_count = available_index.wrapping_sub(available.next_avail());
    if available_count > available.queue_size() {
        return Err(VirtioNetworkQueueCaptureError::AvailableCursorOutOfBounds);
    }
    if available.next_avail().wrapping_sub(used.next_used()) != 0 {
        return Err(VirtioNetworkQueueCaptureError::UnpublishedDescriptorCountMismatch);
    }
    if pending_rate_limited_queue {
        let mut cursor = available.clone();
        let pending = cursor
            .pop_descriptor_chain(memory)
            .map_err(|_| VirtioNetworkQueueCaptureError::AvailableRingInvalid)?
            .ok_or(VirtioNetworkQueueCaptureError::PendingDescriptorMissing)?;
        let pending_head = descriptor_chain_head(&pending)
            .ok_or(VirtioNetworkQueueCaptureError::PendingDescriptorMissing)?;
        while let Some(chain) = cursor
            .pop_descriptor_chain(memory)
            .map_err(|_| VirtioNetworkQueueCaptureError::AvailableRingInvalid)?
        {
            if descriptor_chain_head(&chain) == Some(pending_head) {
                return Err(VirtioNetworkQueueCaptureError::PendingDescriptorDuplicated);
            }
        }
    }

    Ok(VirtioNetworkQueueCaptureState {
        next_available: available.next_avail(),
        next_used: used.next_used(),
        event_idx_enabled,
        negotiated_features,
    })
}

fn network_queue_ranges(
    available: &VirtqueueAvailableRing,
    used: &VirtqueueUsedRing,
) -> Result<[GuestMemoryRange; 3], VirtioNetworkQueueCaptureError> {
    Ok([
        available
            .descriptor_table_range()
            .map_err(|_| VirtioNetworkQueueCaptureError::QueueRangeInvalid)?,
        available
            .available_ring_range()
            .map_err(|_| VirtioNetworkQueueCaptureError::QueueRangeInvalid)?,
        used.used_ring_range()
            .map_err(|_| VirtioNetworkQueueCaptureError::QueueRangeInvalid)?,
    ])
}

fn validate_network_queue_pair_ranges(
    rx: &VirtioNetworkRxQueue,
    tx: &VirtioNetworkTxQueue,
) -> Result<(), VirtioNetworkQueueCaptureError> {
    let rx_ranges = network_queue_ranges(&rx.available, &rx.used)?;
    let tx_ranges = network_queue_ranges(&tx.available, &tx.used)?;
    if rx_ranges.iter().any(|rx_range| {
        tx_ranges
            .iter()
            .any(|tx_range| rx_range.overlaps(*tx_range))
    }) {
        return Err(VirtioNetworkQueueCaptureError::QueueRangesOverlap);
    }
    Ok(())
}

/// Encoding-independent, detached virtio-net device state.
#[derive(Clone, PartialEq, Eq)]
pub struct VirtioNetworkDeviceCaptureState {
    profile: NetworkDeviceProfile,
    available_features: u64,
    negotiated_features: u64,
    active_rx_queue: Option<VirtioNetworkQueueCaptureState>,
    active_tx_queue: Option<VirtioNetworkQueueCaptureState>,
    rx_rate_limiter: VirtioNetworkRateLimiterCaptureState,
    tx_rate_limiter: VirtioNetworkRateLimiterCaptureState,
    source_rx_cache_normalized: bool,
    source_rx_retry_normalized: bool,
    tx_retry: VirtioNetworkRetryCaptureState,
}

/// Ephemeral source-side facts used by the owning backend to validate its
/// retry scheduler. This value is deliberately not embedded in the detached
/// device state because cached RX work is not reconstructible.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkCaptureValidation {
    source_rx_retry: Option<VirtioNetworkRetryCaptureState>,
}

struct VirtioNetworkDeviceCaptureInput<'a> {
    config: &'a NetworkInterfaceConfig,
    profile: NetworkDeviceProfile,
    config_space: &'a VirtioNetworkConfigSpace,
    device_registers: &'a VirtioMmioDeviceRegisters,
    queue_registers: &'a VirtioMmioQueueRegisters,
    transport_activated: bool,
    memory: &'a GuestMemory,
    provider_cached_rx_len: Option<usize>,
    now: Instant,
}

impl VirtioNetworkCaptureValidation {
    pub const fn source_rx_retry(self) -> Option<VirtioNetworkRetryCaptureState> {
        self.source_rx_retry
    }
}

impl fmt::Debug for VirtioNetworkCaptureValidation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkCaptureValidation")
            .field("state", &"<redacted>")
            .finish()
    }
}

impl VirtioNetworkDeviceCaptureState {
    pub const fn profile(&self) -> NetworkDeviceProfile {
        self.profile
    }

    pub const fn available_features(&self) -> u64 {
        self.available_features
    }

    pub const fn negotiated_features(&self) -> u64 {
        self.negotiated_features
    }

    pub const fn active_rx_queue(&self) -> Option<VirtioNetworkQueueCaptureState> {
        self.active_rx_queue
    }

    pub const fn active_tx_queue(&self) -> Option<VirtioNetworkQueueCaptureState> {
        self.active_tx_queue
    }

    pub const fn rx_rate_limiter(&self) -> VirtioNetworkRateLimiterCaptureState {
        self.rx_rate_limiter
    }

    pub const fn tx_rate_limiter(&self) -> VirtioNetworkRateLimiterCaptureState {
        self.tx_rate_limiter
    }

    /// Reports that a source-owned cached RX packet was validated and
    /// deliberately excluded from fresh, lossy restore state.
    pub const fn source_rx_cache_normalized(&self) -> bool {
        self.source_rx_cache_normalized
    }

    /// Reports that source-only cached RX work was validated and deliberately
    /// normalized out of the reconstructible retry state.
    pub const fn source_rx_retry_normalized(&self) -> bool {
        self.source_rx_retry_normalized
    }

    pub const fn tx_retry(&self) -> VirtioNetworkRetryCaptureState {
        self.tx_retry
    }
}

impl fmt::Debug for VirtioNetworkDeviceCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkDeviceCaptureState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct VirtioNetworkMmioCaptureState {
    device: VirtioNetworkDeviceCaptureState,
    transport: VirtioMmioTransportState,
}

impl VirtioNetworkMmioCaptureState {
    pub const fn device(&self) -> &VirtioNetworkDeviceCaptureState {
        &self.device
    }

    pub const fn transport(&self) -> &VirtioMmioTransportState {
        &self.transport
    }
}

impl fmt::Debug for VirtioNetworkMmioCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkMmioCaptureState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct VirtioNetworkPciCaptureState {
    device: VirtioNetworkDeviceCaptureState,
    transport: VirtioPciTransportState,
}

impl VirtioNetworkPciCaptureState {
    pub const fn device(&self) -> &VirtioNetworkDeviceCaptureState {
        &self.device
    }

    pub const fn transport(&self) -> &VirtioPciTransportState {
        &self.transport
    }
}

impl fmt::Debug for VirtioNetworkPciCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioNetworkPciCaptureState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
pub enum VirtioNetworkDeviceCaptureError {
    DeviceIdMismatch,
    RequestedProfileMismatch,
    ConfigSpaceMismatch,
    AvailableFeaturesMismatch,
    NegotiatedFeaturesUnsupported,
    RequiredFeatureNotAcknowledged,
    ActivationMismatch,
    QueueCountMismatch,
    QueueMaxSizeMismatch,
    RxQueue(VirtioNetworkQueueCaptureError),
    TxQueue(VirtioNetworkQueueCaptureError),
    QueueRangesOverlap,
    RxRateLimiter(VirtioNetworkRateLimiterCaptureError),
    TxRateLimiter(VirtioNetworkRateLimiterCaptureError),
    PendingRxWithoutCache,
    PendingRxWithoutRateLimiter,
    CachedRxPacketInvalid,
    PendingTxWithoutRateLimiter,
    PendingTxFrameInvalid,
    RetryDurationOverflow,
}

impl fmt::Display for VirtioNetworkDeviceCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DeviceIdMismatch => {
                formatter.write_str("virtio-net transport has the wrong device id")
            }
            Self::RequestedProfileMismatch => {
                formatter.write_str("virtio-net requested and realized profiles disagree")
            }
            Self::ConfigSpaceMismatch => {
                formatter.write_str("virtio-net config space does not match the realized profile")
            }
            Self::AvailableFeaturesMismatch => {
                formatter.write_str("virtio-net available features do not match the device profile")
            }
            Self::NegotiatedFeaturesUnsupported => {
                formatter.write_str("virtio-net negotiated unsupported features")
            }
            Self::RequiredFeatureNotAcknowledged => {
                formatter.write_str("active virtio-net transport did not acknowledge VERSION_1")
            }
            Self::ActivationMismatch => {
                formatter.write_str("virtio-net device and transport activation state disagree")
            }
            Self::QueueCountMismatch => {
                formatter.write_str("virtio-net transport must contain exactly two queues")
            }
            Self::QueueMaxSizeMismatch => {
                formatter.write_str("virtio-net queue maximum size is invalid")
            }
            Self::RxQueue(source) => write!(
                formatter,
                "virtio-net RX queue is not capture-ready: {source}"
            ),
            Self::TxQueue(source) => write!(
                formatter,
                "virtio-net TX queue is not capture-ready: {source}"
            ),
            Self::QueueRangesOverlap => {
                formatter.write_str("virtio-net RX and TX queue ranges overlap")
            }
            Self::RxRateLimiter(source) => write!(
                formatter,
                "virtio-net RX limiter is not capture-ready: {source}"
            ),
            Self::TxRateLimiter(source) => write!(
                formatter,
                "virtio-net TX limiter is not capture-ready: {source}"
            ),
            Self::PendingRxWithoutCache => {
                formatter.write_str("virtio-net RX retry has no retained provider packet")
            }
            Self::PendingRxWithoutRateLimiter => {
                formatter.write_str("virtio-net RX retry has no configured limiter")
            }
            Self::CachedRxPacketInvalid => {
                formatter.write_str("virtio-net cached RX packet length is invalid")
            }
            Self::PendingTxWithoutRateLimiter => {
                formatter.write_str("virtio-net TX retry has no configured limiter")
            }
            Self::PendingTxFrameInvalid => {
                formatter.write_str("virtio-net TX retry descriptor is invalid")
            }
            Self::RetryDurationOverflow => {
                formatter.write_str("virtio-net retry duration is out of bounds")
            }
        }
    }
}

impl std::error::Error for VirtioNetworkDeviceCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RxQueue(source) | Self::TxQueue(source) => Some(source),
            Self::RxRateLimiter(source) | Self::TxRateLimiter(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioNetworkPciCaptureError {
    Device(VirtioNetworkDeviceCaptureError),
    Endpoint(VirtioPciEndpointError),
}

impl fmt::Display for VirtioNetworkPciCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Device(_) => "PCI virtio-net device capture failed",
            Self::Endpoint(_) => "PCI virtio-net transport capture failed",
        })
    }
}

impl std::error::Error for VirtioNetworkPciCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Device(source) => Some(source),
            Self::Endpoint(source) => Some(source),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct VirtioNetworkDevice {
    active_rx_queue: Option<VirtioNetworkRxQueue>,
    active_tx_queue: Option<VirtioNetworkTxQueue>,
    rx_rate_limiter: Option<VirtioNetworkRateLimiter>,
    tx_rate_limiter: Option<VirtioNetworkRateLimiter>,
    pending_rate_limited_rx_queue: bool,
    pending_rate_limited_tx_queue: bool,
    metrics: Option<VirtioNetworkTransportMetrics>,
}

impl PartialEq for VirtioNetworkDevice {
    fn eq(&self, other: &Self) -> bool {
        self.active_rx_queue == other.active_rx_queue
            && self.active_tx_queue == other.active_tx_queue
            && self.rx_rate_limiter == other.rx_rate_limiter
            && self.tx_rate_limiter == other.tx_rate_limiter
            && self.pending_rate_limited_rx_queue == other.pending_rate_limited_rx_queue
            && self.pending_rate_limited_tx_queue == other.pending_rate_limited_tx_queue
    }
}

impl Eq for VirtioNetworkDevice {}

impl VirtioNetworkDevice {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_rate_limiters(
        rx_rate_limiter: Option<NetworkRateLimiterConfig>,
        tx_rate_limiter: Option<NetworkRateLimiterConfig>,
    ) -> Self {
        Self::with_rate_limiters_at(rx_rate_limiter, tx_rate_limiter, Instant::now())
    }

    fn with_rate_limiters_at(
        rx_rate_limiter: Option<NetworkRateLimiterConfig>,
        tx_rate_limiter: Option<NetworkRateLimiterConfig>,
        now: Instant,
    ) -> Self {
        Self {
            active_rx_queue: None,
            active_tx_queue: None,
            rx_rate_limiter: rx_rate_limiter
                .and_then(|rate_limiter| VirtioNetworkRateLimiter::new_at(rate_limiter, now)),
            tx_rate_limiter: tx_rate_limiter
                .and_then(|rate_limiter| VirtioNetworkRateLimiter::new_at(rate_limiter, now)),
            pending_rate_limited_rx_queue: false,
            pending_rate_limited_tx_queue: false,
            metrics: None,
        }
    }

    pub fn attach_metrics(&mut self, metrics: SharedNetworkInterfaceMetrics) {
        self.metrics = Some(VirtioNetworkTransportMetrics::for_interface(metrics));
    }

    fn attach_metrics_with_aggregate(
        &mut self,
        interface: SharedNetworkInterfaceMetrics,
        aggregate: SharedNetworkInterfaceMetrics,
    ) {
        self.metrics = Some(VirtioNetworkTransportMetrics::with_aggregate(
            interface, aggregate,
        ));
    }

    pub fn is_activated(&self) -> bool {
        self.active_rx_queue.is_some() && self.active_tx_queue.is_some()
    }

    pub fn active_rx_queue(&self) -> Option<VirtioMmioQueueState> {
        self.active_rx_queue
            .as_ref()
            .map(VirtioNetworkRxQueue::queue_state)
    }

    pub const fn active_rx_dispatch_queue(&self) -> Option<&VirtioNetworkRxQueue> {
        self.active_rx_queue.as_ref()
    }

    pub fn active_tx_queue(&self) -> Option<VirtioMmioQueueState> {
        self.active_tx_queue
            .as_ref()
            .map(VirtioNetworkTxQueue::queue_state)
    }

    pub const fn active_tx_dispatch_queue(&self) -> Option<&VirtioNetworkTxQueue> {
        self.active_tx_queue.as_ref()
    }

    pub const fn rx_rate_limiter(&self) -> Option<&VirtioNetworkRateLimiter> {
        self.rx_rate_limiter.as_ref()
    }

    pub const fn tx_rate_limiter(&self) -> Option<&VirtioNetworkRateLimiter> {
        self.tx_rate_limiter.as_ref()
    }

    pub const fn has_pending_rate_limited_rx_queue(&self) -> bool {
        self.pending_rate_limited_rx_queue
    }

    pub const fn has_pending_rate_limited_tx_queue(&self) -> bool {
        self.pending_rate_limited_tx_queue
    }

    pub const fn has_pending_rate_limited_queue_work(&self) -> bool {
        self.pending_rate_limited_rx_queue || self.pending_rate_limited_tx_queue
    }

    fn capture_state_at(
        &self,
        input: VirtioNetworkDeviceCaptureInput<'_>,
    ) -> Result<
        (
            VirtioNetworkDeviceCaptureState,
            VirtioNetworkCaptureValidation,
        ),
        VirtioNetworkDeviceCaptureError,
    > {
        let VirtioNetworkDeviceCaptureInput {
            config,
            profile,
            config_space,
            device_registers,
            queue_registers,
            transport_activated,
            memory,
            provider_cached_rx_len,
            now,
        } = input;
        if device_registers.device_id() != VIRTIO_NET_DEVICE_ID {
            return Err(VirtioNetworkDeviceCaptureError::DeviceIdMismatch);
        }
        if config
            .guest_mac()
            .is_some_and(|requested| profile.guest_mac() != Some(requested))
            || config
                .mtu()
                .is_some_and(|requested| profile.mtu() != Some(requested))
            || !profile.feature_capabilities().is_dependency_complete()
        {
            return Err(VirtioNetworkDeviceCaptureError::RequestedProfileMismatch);
        }
        if config_space.guest_mac != profile.guest_mac()
            || config_space.mtu != profile.mtu()
            || config_space.network_features != profile.feature_capabilities().feature_bits()
        {
            return Err(VirtioNetworkDeviceCaptureError::ConfigSpaceMismatch);
        }
        let available_features = config_space.available_features();
        if device_registers.device_features() != available_features {
            return Err(VirtioNetworkDeviceCaptureError::AvailableFeaturesMismatch);
        }
        let negotiated_features = device_registers.driver_features();
        if negotiated_features & !available_features != 0 {
            return Err(VirtioNetworkDeviceCaptureError::NegotiatedFeaturesUnsupported);
        }
        let queues_active = match (&self.active_rx_queue, &self.active_tx_queue) {
            (Some(_), Some(_)) => true,
            (None, None) => false,
            _ => return Err(VirtioNetworkDeviceCaptureError::ActivationMismatch),
        };
        if queues_active != transport_activated {
            return Err(VirtioNetworkDeviceCaptureError::ActivationMismatch);
        }
        if transport_activated
            && !virtio_feature_enabled(negotiated_features, VIRTIO_FEATURE_VERSION_1)
        {
            return Err(VirtioNetworkDeviceCaptureError::RequiredFeatureNotAcknowledged);
        }
        if queue_registers.queue_count() != VIRTIO_NET_QUEUE_COUNT {
            return Err(VirtioNetworkDeviceCaptureError::QueueCountMismatch);
        }
        let rx_transport = queue_registers
            .queue(VIRTIO_NET_RX_QUEUE_INDEX_U32)
            .map_err(|_| VirtioNetworkDeviceCaptureError::QueueCountMismatch)?;
        let tx_transport = queue_registers
            .queue(VIRTIO_NET_TX_QUEUE_INDEX_U32)
            .map_err(|_| VirtioNetworkDeviceCaptureError::QueueCountMismatch)?;
        if rx_transport.max_size() != VIRTIO_NET_QUEUE_SIZE
            || tx_transport.max_size() != VIRTIO_NET_QUEUE_SIZE
        {
            return Err(VirtioNetworkDeviceCaptureError::QueueMaxSizeMismatch);
        }
        let provider_cached_rx_used_len = match provider_cached_rx_len {
            Some(0) => return Err(VirtioNetworkDeviceCaptureError::CachedRxPacketInvalid),
            Some(packet_len) => Some(
                rx_packet_used_len(packet_len)
                    .map_err(|_| VirtioNetworkDeviceCaptureError::CachedRxPacketInvalid)?,
            ),
            None => None,
        };
        if self.pending_rate_limited_rx_queue && provider_cached_rx_used_len.is_none() {
            return Err(VirtioNetworkDeviceCaptureError::PendingRxWithoutCache);
        }

        let rx_rate_limiter = capture_network_rate_limiter_state_at(
            config.rx_rate_limiter(),
            self.rx_rate_limiter.as_ref(),
            now,
        )
        .map_err(VirtioNetworkDeviceCaptureError::RxRateLimiter)?;
        let tx_rate_limiter = capture_network_rate_limiter_state_at(
            config.tx_rate_limiter(),
            self.tx_rate_limiter.as_ref(),
            now,
        )
        .map_err(VirtioNetworkDeviceCaptureError::TxRateLimiter)?;
        if self.pending_rate_limited_rx_queue && !rx_rate_limiter.is_configured() {
            return Err(VirtioNetworkDeviceCaptureError::PendingRxWithoutRateLimiter);
        }
        if self.pending_rate_limited_tx_queue && !tx_rate_limiter.is_configured() {
            return Err(VirtioNetworkDeviceCaptureError::PendingTxWithoutRateLimiter);
        }

        let active_rx_queue = match self.active_rx_queue.as_ref() {
            Some(queue) => {
                if queue.negotiated_features != negotiated_features {
                    return Err(VirtioNetworkDeviceCaptureError::RxQueue(
                        VirtioNetworkQueueCaptureError::FeatureMismatch,
                    ));
                }
                Some(
                    capture_network_queue_state(
                        rx_transport,
                        &queue.available,
                        &queue.used,
                        queue.event_idx_enabled,
                        queue.negotiated_features,
                        memory,
                        self.pending_rate_limited_rx_queue,
                    )
                    .map_err(VirtioNetworkDeviceCaptureError::RxQueue)?,
                )
            }
            None => None,
        };
        let active_tx_queue = match self.active_tx_queue.as_ref() {
            Some(queue) => {
                if queue.negotiated_features != negotiated_features {
                    return Err(VirtioNetworkDeviceCaptureError::TxQueue(
                        VirtioNetworkQueueCaptureError::FeatureMismatch,
                    ));
                }
                Some(
                    capture_network_queue_state(
                        tx_transport,
                        &queue.available,
                        &queue.used,
                        queue.event_idx_enabled,
                        queue.negotiated_features,
                        memory,
                        self.pending_rate_limited_tx_queue,
                    )
                    .map_err(VirtioNetworkDeviceCaptureError::TxQueue)?,
                )
            }
            None => None,
        };
        if let (Some(rx), Some(tx)) = (&self.active_rx_queue, &self.active_tx_queue) {
            validate_network_queue_pair_ranges(rx, tx)
                .map_err(|_| VirtioNetworkDeviceCaptureError::QueueRangesOverlap)?;
        }

        let source_rx_retry = if self.pending_rate_limited_rx_queue {
            let bytes_written_to_guest = provider_cached_rx_used_len
                .ok_or(VirtioNetworkDeviceCaptureError::PendingRxWithoutCache)?;
            let mut limiter = self
                .rx_rate_limiter
                .clone()
                .ok_or(VirtioNetworkDeviceCaptureError::PendingRxWithoutRateLimiter)?;
            match limiter.reduce_at(u64::from(bytes_written_to_guest), now) {
                VirtioNetworkRateLimiterReduction::Allowed(_) => {
                    Some(VirtioNetworkRetryCaptureState::Immediate)
                }
                VirtioNetworkRateLimiterReduction::Throttled { retry_after } => {
                    Some(network_retry_capture_state(retry_after)?)
                }
            }
        } else {
            None
        };

        let tx_retry = if self.pending_rate_limited_tx_queue {
            let queue = self
                .active_tx_queue
                .as_ref()
                .ok_or(VirtioNetworkDeviceCaptureError::ActivationMismatch)?;
            let mut available = queue.available.clone();
            let chain = available
                .pop_descriptor_chain(memory)
                .map_err(|_| VirtioNetworkDeviceCaptureError::PendingTxFrameInvalid)?
                .ok_or(VirtioNetworkDeviceCaptureError::PendingTxFrameInvalid)?;
            let frame = VirtioNetworkTxFrame::parse_with_features(
                memory,
                &chain,
                queue.negotiated_features,
            )
            .map_err(|_| VirtioNetworkDeviceCaptureError::PendingTxFrameInvalid)?;
            let mut limiter = self
                .tx_rate_limiter
                .clone()
                .ok_or(VirtioNetworkDeviceCaptureError::PendingTxWithoutRateLimiter)?;
            match limiter.reduce_at(frame.frame_len(), now) {
                VirtioNetworkRateLimiterReduction::Allowed(_) => {
                    VirtioNetworkRetryCaptureState::Immediate
                }
                VirtioNetworkRateLimiterReduction::Throttled { retry_after } => {
                    network_retry_capture_state(retry_after)?
                }
            }
        } else {
            VirtioNetworkRetryCaptureState::None
        };

        Ok((
            VirtioNetworkDeviceCaptureState {
                profile,
                available_features,
                negotiated_features,
                active_rx_queue,
                active_tx_queue,
                rx_rate_limiter,
                tx_rate_limiter,
                source_rx_cache_normalized: provider_cached_rx_used_len.is_some(),
                source_rx_retry_normalized: source_rx_retry.is_some(),
                tx_retry,
            },
            VirtioNetworkCaptureValidation { source_rx_retry },
        ))
    }

    pub fn update_rate_limiters(&mut self, update: &NetworkInterfaceUpdate) {
        self.update_rate_limiters_at(update, Instant::now());
    }

    fn update_rate_limiters_at(&mut self, update: &NetworkInterfaceUpdate, now: Instant) {
        let rx_rate_limiter = match update.rx_rate_limiter() {
            Some(config) => {
                VirtioNetworkRateLimiter::updated_at(self.rx_rate_limiter.as_ref(), config, now)
            }
            None => self.rx_rate_limiter.clone(),
        };
        let tx_rate_limiter = match update.tx_rate_limiter() {
            Some(config) => {
                VirtioNetworkRateLimiter::updated_at(self.tx_rate_limiter.as_ref(), config, now)
            }
            None => self.tx_rate_limiter.clone(),
        };

        self.rx_rate_limiter = rx_rate_limiter;
        self.tx_rate_limiter = tx_rate_limiter;
    }

    pub fn activate_network(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioNetworkDeviceActivationError> {
        if self.is_activated() {
            return Err(VirtioNetworkDeviceActivationError::AlreadyActive);
        }

        let event_idx_enabled =
            virtio_feature_enabled(activation.driver_features(), VIRTIO_RING_FEATURE_EVENT_IDX);
        let indirect_descriptors_enabled = virtio_feature_enabled(
            activation.driver_features(),
            VIRTIO_RING_FEATURE_INDIRECT_DESC,
        );
        let active_rx_queue = active_network_queue_state(activation, VIRTIO_NET_RX_QUEUE_INDEX_U32)
            .and_then(|queue| {
                VirtioNetworkRxQueue::from_mmio_queue_state_with_event_idx(
                    queue,
                    event_idx_enabled,
                    indirect_descriptors_enabled,
                    activation.driver_features(),
                )
                .map_err(|source| {
                    VirtioNetworkDeviceActivationError::RxQueueBuild {
                        queue_index: VIRTIO_NET_RX_QUEUE_INDEX_U32,
                        source,
                    }
                })
            })?;
        let active_tx_queue = active_network_queue_state(activation, VIRTIO_NET_TX_QUEUE_INDEX_U32)
            .and_then(|queue| {
                VirtioNetworkTxQueue::from_mmio_queue_state_with_event_idx(
                    queue,
                    event_idx_enabled,
                    indirect_descriptors_enabled,
                    activation.driver_features(),
                )
                .map_err(|source| {
                    VirtioNetworkDeviceActivationError::TxQueueBuild {
                        queue_index: VIRTIO_NET_TX_QUEUE_INDEX_U32,
                        source,
                    }
                })
            })?;

        self.active_rx_queue = Some(active_rx_queue);
        self.active_tx_queue = Some(active_tx_queue);

        Ok(())
    }

    pub fn reset(&mut self) {
        self.active_rx_queue = None;
        self.active_tx_queue = None;
        self.pending_rate_limited_rx_queue = false;
        self.pending_rate_limited_tx_queue = false;
    }

    fn dispatch_drained_queue_notifications_with_tx_sink(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
    ) -> Result<VirtioNetworkDeviceNotificationDispatch, VirtioNetworkDeviceNotificationError> {
        let mut rx_source = EmptyVirtioNetworkRxPacketSource;
        self.dispatch_drained_queue_notifications_with_packet_io(
            memory,
            drained_notifications,
            tx_sink,
            &mut rx_source,
        )
    }

    fn dispatch_drained_queue_notifications_with_packet_io(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
        rx_source: &mut (impl VirtioNetworkRxPacketSource + ?Sized),
    ) -> Result<VirtioNetworkDeviceNotificationDispatch, VirtioNetworkDeviceNotificationError> {
        self.dispatch_drained_queue_notifications_with_packet_io_at(
            memory,
            drained_notifications,
            tx_sink,
            rx_source,
            Instant::now(),
        )
    }

    fn dispatch_drained_queue_notifications_with_packet_io_at(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
        rx_source: &mut (impl VirtioNetworkRxPacketSource + ?Sized),
        now: Instant,
    ) -> Result<VirtioNetworkDeviceNotificationDispatch, VirtioNetworkDeviceNotificationError> {
        let host_readiness = rx_source.host_readiness_hint();
        if drained_notifications.is_empty()
            && !self.has_pending_rate_limited_queue_work()
            && !host_readiness
        {
            return Ok(VirtioNetworkDeviceNotificationDispatch::new(
                drained_notifications,
                None,
                None,
                None,
            ));
        }

        if !self.is_activated() && drained_notifications.is_empty() && host_readiness {
            return Ok(VirtioNetworkDeviceNotificationDispatch::new(
                drained_notifications,
                None,
                None,
                None,
            ));
        }

        if !self.is_activated() {
            return Err(VirtioNetworkDeviceNotificationError::Inactive {
                drained_notifications,
            });
        }

        let rx_rate_limiter_event = self.pending_rate_limited_rx_queue;
        let tx_rate_limiter_event = self.pending_rate_limited_tx_queue;

        if let Some(queue_index) = drained_notifications.iter().copied().find(|queue_index| {
            *queue_index != VIRTIO_NET_RX_QUEUE_INDEX && *queue_index != VIRTIO_NET_TX_QUEUE_INDEX
        }) {
            return Err(VirtioNetworkDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        let dispatch_rx = drained_notifications
            .iter()
            .copied()
            .any(|queue_index| queue_index == VIRTIO_NET_RX_QUEUE_INDEX)
            || self.pending_rate_limited_rx_queue
            || host_readiness;
        let dispatch_tx = drained_notifications
            .iter()
            .copied()
            .any(|queue_index| queue_index == VIRTIO_NET_TX_QUEUE_INDEX)
            || self.pending_rate_limited_tx_queue;

        let rx_queue_dispatch = if dispatch_rx {
            let Some(queue) = self.active_rx_queue.as_mut() else {
                return Err(VirtioNetworkDeviceNotificationError::Inactive {
                    drained_notifications,
                });
            };

            match queue.dispatch_with_source_hint_policy(
                memory,
                rx_source,
                false,
                self.rx_rate_limiter.as_mut(),
                now,
            ) {
                Ok(dispatch) => {
                    self.pending_rate_limited_rx_queue =
                        dispatch.rate_limiter_throttled_packets() != 0;
                    Some(dispatch)
                }
                Err(source) => {
                    return Err(VirtioNetworkDeviceNotificationError::RxQueueDispatch {
                        drained_notifications,
                        completed_tx_dispatch: None,
                        completed_initial_rx_dispatch: None,
                        source,
                    });
                }
            }
        } else {
            None
        };

        let tx_queue_dispatch = if dispatch_tx {
            let Some(queue) = self.active_tx_queue.as_mut() else {
                return Err(VirtioNetworkDeviceNotificationError::Inactive {
                    drained_notifications,
                });
            };

            match queue.dispatch_with_sink_at(memory, tx_sink, self.tx_rate_limiter.as_mut(), now) {
                Ok(dispatch) => {
                    self.pending_rate_limited_tx_queue =
                        dispatch.rate_limiter_throttled_frames() != 0;
                    Some(dispatch)
                }
                Err(source) => {
                    return Err(VirtioNetworkDeviceNotificationError::TxQueueDispatch {
                        drained_notifications,
                        completed_rx_dispatch: rx_queue_dispatch.map(Box::new),
                        source,
                    });
                }
            }
        } else {
            None
        };

        let post_tx_rx_queue_dispatch = if dispatch_tx
            && !self.pending_rate_limited_rx_queue
            && rx_source.retry_after_tx_hint()
        {
            let Some(queue) = self.active_rx_queue.as_mut() else {
                return Err(VirtioNetworkDeviceNotificationError::Inactive {
                    drained_notifications,
                });
            };

            match queue.dispatch_ready_source(memory, rx_source, self.rx_rate_limiter.as_mut(), now)
            {
                Ok(dispatch) => {
                    self.pending_rate_limited_rx_queue =
                        dispatch.rate_limiter_throttled_packets() != 0;
                    Some(dispatch)
                }
                Err(source) => {
                    return Err(VirtioNetworkDeviceNotificationError::RxQueueDispatch {
                        drained_notifications,
                        completed_tx_dispatch: tx_queue_dispatch.map(Box::new),
                        completed_initial_rx_dispatch: rx_queue_dispatch.map(Box::new),
                        source,
                    });
                }
            }
        } else {
            None
        };

        Ok(VirtioNetworkDeviceNotificationDispatch::new(
            drained_notifications,
            rx_queue_dispatch,
            tx_queue_dispatch,
            post_tx_rx_queue_dispatch,
        )
        .with_rate_limiter_events(rx_rate_limiter_event, tx_rate_limiter_event))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioNetworkRxQueue {
    queue_state: VirtioMmioQueueState,
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
    event_idx_enabled: bool,
    negotiated_features: u64,
}

impl VirtioNetworkRxQueue {
    pub fn from_mmio_queue_state(
        queue: VirtioMmioQueueState,
    ) -> Result<Self, VirtioNetworkRxQueueBuildError> {
        Self::from_mmio_queue_state_with_event_idx(queue, false, false, 0)
    }

    fn from_mmio_queue_state_with_event_idx(
        queue: VirtioMmioQueueState,
        event_idx_enabled: bool,
        indirect_descriptors_enabled: bool,
        negotiated_features: u64,
    ) -> Result<Self, VirtioNetworkRxQueueBuildError> {
        if !queue.ready() {
            return Err(VirtioNetworkRxQueueBuildError::QueueNotReady);
        }

        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioNetworkRxQueueBuildError::AvailableRing { source })?;
        let available = available.with_descriptor_chain_options(
            VirtqueueDescriptorChainOptions::new()
                .with_indirect_descriptors(indirect_descriptors_enabled),
        );
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioNetworkRxQueueBuildError::UsedRing { source })?;

        Ok(Self {
            queue_state: queue,
            available,
            used,
            event_idx_enabled,
            negotiated_features,
        })
    }

    pub const fn queue_state(&self) -> VirtioMmioQueueState {
        self.queue_state
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
    }

    pub const fn event_idx_enabled(&self) -> bool {
        self.event_idx_enabled
    }

    pub const fn negotiated_features(&self) -> u64 {
        self.negotiated_features
    }

    pub fn dispatch(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioNetworkRxQueueDispatch, VirtioNetworkRxQueueDispatchError> {
        let mut source = EmptyVirtioNetworkRxPacketSource;
        self.dispatch_with_source(memory, &mut source)
    }

    pub fn dispatch_with_source(
        &mut self,
        memory: &mut GuestMemory,
        rx_source: &mut (impl VirtioNetworkRxPacketSource + ?Sized),
    ) -> Result<VirtioNetworkRxQueueDispatch, VirtioNetworkRxQueueDispatchError> {
        self.dispatch_with_source_hint_policy(memory, rx_source, false, None, Instant::now())
    }

    fn dispatch_ready_source(
        &mut self,
        memory: &mut GuestMemory,
        rx_source: &mut (impl VirtioNetworkRxPacketSource + ?Sized),
        rate_limiter: Option<&mut VirtioNetworkRateLimiter>,
        now: Instant,
    ) -> Result<VirtioNetworkRxQueueDispatch, VirtioNetworkRxQueueDispatchError> {
        self.dispatch_with_source_hint_policy(memory, rx_source, true, rate_limiter, now)
    }

    fn dispatch_with_source_hint_policy(
        &mut self,
        memory: &mut GuestMemory,
        rx_source: &mut (impl VirtioNetworkRxPacketSource + ?Sized),
        require_ready_hint: bool,
        rate_limiter: Option<&mut VirtioNetworkRateLimiter>,
        now: Instant,
    ) -> Result<VirtioNetworkRxQueueDispatch, VirtioNetworkRxQueueDispatchError> {
        let mut result = (|| {
            let mut dispatch =
                VirtioNetworkRxQueueDispatch::with_capacity(self.available.queue_size())?;
            let mut rate_limiter = rate_limiter;
            rx_source.begin_rx_dispatch();

            loop {
                if require_ready_hint && !rx_source.retry_after_tx_hint() {
                    break;
                }
                let action = {
                    let packet = match rx_source.peek_packet() {
                        Ok(Some(packet)) => packet,
                        Ok(None) => break,
                        Err(source) => {
                            dispatch.record_source_failure(source.clone());
                            dispatch.record_backend_metrics(rx_source.take_backend_metrics());
                            return Err(VirtioNetworkRxQueueDispatchError::PacketSource {
                                completed_dispatch: Box::new(dispatch),
                                source,
                            });
                        }
                    };
                    let bytes_written_to_guest = match rx_packet_used_len(packet.len()) {
                        Ok(bytes_written_to_guest) => bytes_written_to_guest,
                        Err(error) => {
                            return Err(VirtioNetworkRxQueueDispatchError::PacketTooLarge {
                                completed_dispatch: Box::new(dispatch),
                                len: error.len,
                                max: error.max,
                            });
                        }
                    };
                    let packet_len = match u64::try_from(packet.len()) {
                        Ok(packet_len) => packet_len,
                        Err(_) => {
                            return Err(VirtioNetworkRxQueueDispatchError::PacketTooLarge {
                                completed_dispatch: Box::new(dispatch),
                                len: u64::MAX,
                                max: VIRTIO_NET_MAX_BUFFER_SIZE,
                            });
                        }
                    };

                    if self.merged_rx_buffers_enabled() {
                        match self.dispatch_merged_packet(
                            memory,
                            packet,
                            packet_len,
                            bytes_written_to_guest,
                            rate_limiter.as_deref_mut(),
                            now,
                        ) {
                            Ok(VirtioNetworkMergedRxDispatch::Delivered(outcome, publication)) => {
                                VirtioNetworkRxQueueDispatchAction::Consume(outcome, publication)
                            }
                            Ok(VirtioNetworkMergedRxDispatch::NoAvailableBuffers) => {
                                dispatch.record_no_available_buffer();
                                break;
                            }
                            Ok(VirtioNetworkMergedRxDispatch::RateLimited { retry_after }) => {
                                dispatch.record_rate_limited_packet(retry_after);
                                break;
                            }
                            Err(VirtioNetworkMergedRxDispatchError::AvailableRing { source }) => {
                                return Err(VirtioNetworkRxQueueDispatchError::AvailableRing {
                                    completed_dispatch: Box::new(dispatch),
                                    source,
                                });
                            }
                            Err(VirtioNetworkMergedRxDispatchError::EmptyDescriptorChain) => {
                                return Err(
                                    VirtioNetworkRxQueueDispatchError::EmptyDescriptorChain {
                                        completed_dispatch: Box::new(dispatch),
                                    },
                                );
                            }
                            Err(VirtioNetworkMergedRxDispatchError::BufferParse {
                                descriptor_head,
                                source,
                            }) => {
                                return Err(VirtioNetworkRxQueueDispatchError::BufferParse {
                                    completed_dispatch: Box::new(dispatch),
                                    descriptor_head,
                                    source,
                                });
                            }
                            Err(VirtioNetworkMergedRxDispatchError::BufferWrite {
                                descriptor_head,
                                source,
                            }) => {
                                return Err(VirtioNetworkRxQueueDispatchError::BufferWrite {
                                    completed_dispatch: Box::new(dispatch),
                                    descriptor_head,
                                    source,
                                });
                            }
                            Err(VirtioNetworkMergedRxDispatchError::UsedRing {
                                descriptor_head,
                                source,
                            }) => {
                                return Err(VirtioNetworkRxQueueDispatchError::UsedRing {
                                    completed_dispatch: Box::new(dispatch),
                                    descriptor_head,
                                    bytes_written_to_guest,
                                    source,
                                });
                            }
                            Err(VirtioNetworkMergedRxDispatchError::MetadataAllocation {
                                source,
                            }) => {
                                return Err(
                                    VirtioNetworkRxQueueDispatchError::PacketMetadataAllocation {
                                        completed_dispatch: Some(Box::new(dispatch)),
                                        source,
                                    },
                                );
                            }
                        }
                    } else {
                        let chain = match self.available.pop_descriptor_chain(memory) {
                            Ok(Some(chain)) => chain,
                            Ok(None) => {
                                dispatch.record_no_available_buffer();
                                break;
                            }
                            Err(source) => {
                                return Err(VirtioNetworkRxQueueDispatchError::AvailableRing {
                                    completed_dispatch: Box::new(dispatch),
                                    source,
                                });
                            }
                        };
                        let descriptor_head = match descriptor_chain_head(&chain) {
                            Some(descriptor_head) => descriptor_head,
                            None => {
                                return Err(
                                    VirtioNetworkRxQueueDispatchError::EmptyDescriptorChain {
                                        completed_dispatch: Box::new(dispatch),
                                    },
                                );
                            }
                        };

                        let minimum_len = self.non_merged_rx_minimum_buffer_size();
                        match VirtioNetworkRxBuffer::parse_with_minimum(memory, &chain, minimum_len)
                        {
                            Ok(buffer) => {
                                if u64::from(bytes_written_to_guest) > buffer.len() {
                                    let notification_suppression = match self
                                        .notification_suppression(memory)
                                    {
                                        Ok(notification_suppression) => notification_suppression,
                                        Err(source) => {
                                            return Err(
                                                VirtioNetworkRxQueueDispatchError::AvailableRing {
                                                    completed_dispatch: Box::new(dispatch),
                                                    source,
                                                },
                                            );
                                        }
                                    };
                                    let publication =
                                        match self.used.publish_used_element_with_notification(
                                            memory,
                                            descriptor_head,
                                            0,
                                            notification_suppression,
                                        ) {
                                            Ok(publication) => publication,
                                            Err(source) => {
                                                return Err(
                                                    VirtioNetworkRxQueueDispatchError::UsedRing {
                                                        completed_dispatch: Box::new(dispatch),
                                                        descriptor_head,
                                                        bytes_written_to_guest: 0,
                                                        source,
                                                    },
                                                );
                                            }
                                        };
                                    VirtioNetworkRxQueueDispatchAction::Record(
                                        VirtioNetworkRxQueueDispatchOutcome::BufferTooSmall(
                                            VirtioNetworkRxBufferTooSmall {
                                                descriptor_head,
                                                len: buffer.len(),
                                                required_len: u64::from(bytes_written_to_guest),
                                            },
                                        ),
                                        publication,
                                    )
                                } else {
                                    let notification_suppression = match self
                                        .notification_suppression(memory)
                                    {
                                        Ok(notification_suppression) => notification_suppression,
                                        Err(source) => {
                                            return Err(
                                                VirtioNetworkRxQueueDispatchError::AvailableRing {
                                                    completed_dispatch: Box::new(dispatch),
                                                    source,
                                                },
                                            );
                                        }
                                    };
                                    let limiter_reservation = match rate_limiter.as_deref_mut() {
                                        Some(limiter) => match limiter
                                            .reduce_at(u64::from(bytes_written_to_guest), now)
                                        {
                                            VirtioNetworkRateLimiterReduction::Allowed(
                                                reservation,
                                            ) => Some(reservation),
                                            VirtioNetworkRateLimiterReduction::Throttled {
                                                retry_after,
                                            } => {
                                                if let Err(source) =
                                                    self.available.undo_pop_descriptor_chain()
                                                {
                                                    return Err(
                                                VirtioNetworkRxQueueDispatchError::AvailableRing {
                                                    completed_dispatch: Box::new(dispatch),
                                                    source,
                                                },
                                            );
                                                }
                                                dispatch.record_rate_limited_packet(retry_after);
                                                break;
                                            }
                                        },
                                        None => None,
                                    };
                                    if let Err(source) =
                                        write_rx_packet_to_buffer(memory, &buffer, packet)
                                    {
                                        if let (Some(limiter), Some(reservation)) =
                                            (rate_limiter.as_deref_mut(), limiter_reservation)
                                        {
                                            limiter.restore(reservation);
                                        }
                                        return Err(
                                            VirtioNetworkRxQueueDispatchError::BufferWrite {
                                                completed_dispatch: Box::new(dispatch),
                                                descriptor_head,
                                                source,
                                            },
                                        );
                                    }
                                    let publication = match self
                                        .used
                                        .publish_used_element_with_notification(
                                            memory,
                                            descriptor_head,
                                            bytes_written_to_guest,
                                            notification_suppression,
                                        ) {
                                        Ok(publication) => publication,
                                        Err(source) => {
                                            if let (Some(limiter), Some(reservation)) =
                                                (rate_limiter.as_deref_mut(), limiter_reservation)
                                            {
                                                limiter.restore(reservation);
                                            }
                                            return Err(
                                                VirtioNetworkRxQueueDispatchError::UsedRing {
                                                    completed_dispatch: Box::new(dispatch),
                                                    descriptor_head,
                                                    bytes_written_to_guest,
                                                    source,
                                                },
                                            );
                                        }
                                    };
                                    VirtioNetworkRxQueueDispatchAction::Consume(
                                        VirtioNetworkRxQueueDispatchOutcome::Delivered(
                                            VirtioNetworkRxPacketDelivery {
                                                descriptor_head,
                                                packet_len,
                                                bytes_written_to_guest,
                                                buffer_count: 1,
                                            },
                                        ),
                                        publication,
                                    )
                                }
                            }
                            Err(source) => {
                                let notification_suppression =
                                    match self.notification_suppression(memory) {
                                        Ok(notification_suppression) => notification_suppression,
                                        Err(source) => {
                                            return Err(
                                                VirtioNetworkRxQueueDispatchError::AvailableRing {
                                                    completed_dispatch: Box::new(dispatch),
                                                    source,
                                                },
                                            );
                                        }
                                    };
                                let publication = match self
                                    .used
                                    .publish_used_element_with_notification(
                                        memory,
                                        descriptor_head,
                                        0,
                                        notification_suppression,
                                    ) {
                                    Ok(publication) => publication,
                                    Err(used_source) => {
                                        return Err(VirtioNetworkRxQueueDispatchError::UsedRing {
                                            completed_dispatch: Box::new(dispatch),
                                            descriptor_head,
                                            bytes_written_to_guest: 0,
                                            source: used_source,
                                        });
                                    }
                                };
                                VirtioNetworkRxQueueDispatchAction::Record(
                                    VirtioNetworkRxQueueDispatchOutcome::BufferParseError(source),
                                    publication,
                                )
                            }
                        }
                    }
                };

                match action {
                    VirtioNetworkRxQueueDispatchAction::Record(outcome, publication) => {
                        dispatch.record(outcome, publication);
                    }
                    VirtioNetworkRxQueueDispatchAction::Consume(outcome, publication) => {
                        rx_source.consume_packet();
                        dispatch.record(outcome, publication);
                    }
                }
            }

            dispatch.record_backend_metrics(rx_source.take_backend_metrics());
            Ok(dispatch)
        })();
        if let Err(source) = &mut result {
            source.record_backend_metrics(rx_source.take_backend_metrics());
        }
        result
    }

    const fn merged_rx_buffers_enabled(&self) -> bool {
        virtio_feature_enabled(self.negotiated_features, VIRTIO_NET_F_MRG_RXBUF)
    }

    const fn non_merged_rx_minimum_buffer_size(&self) -> u64 {
        if virtio_feature_enabled(self.negotiated_features, VIRTIO_NET_F_GUEST_TSO4)
            || virtio_feature_enabled(self.negotiated_features, VIRTIO_NET_F_GUEST_TSO6)
            || virtio_feature_enabled(self.negotiated_features, VIRTIO_NET_F_GUEST_UFO)
        {
            VIRTIO_NET_RX_LARGE_BUFFER_SIZE
        } else {
            VIRTIO_NET_RX_MIN_BUFFER_SIZE
        }
    }

    fn dispatch_merged_packet(
        &mut self,
        memory: &mut GuestMemory,
        packet: VirtioNetworkRxPacket<'_>,
        packet_len: u64,
        bytes_written_to_guest: u32,
        rate_limiter: Option<&mut VirtioNetworkRateLimiter>,
        now: Instant,
    ) -> Result<VirtioNetworkMergedRxDispatch, VirtioNetworkMergedRxDispatchError> {
        let checkpoint = self.available.checkpoint();
        let mut buffers = Vec::new();
        buffers
            .try_reserve_exact(usize::from(self.available.queue_size()))
            .map_err(|source| VirtioNetworkMergedRxDispatchError::MetadataAllocation { source })?;
        let mut capacity = 0_u64;
        while capacity < u64::from(bytes_written_to_guest) {
            let chain = match self.available.pop_descriptor_chain(memory) {
                Ok(Some(chain)) => chain,
                Ok(None) => {
                    self.available.restore_checkpoint(checkpoint);
                    return Ok(VirtioNetworkMergedRxDispatch::NoAvailableBuffers);
                }
                Err(source) => {
                    self.available.restore_checkpoint(checkpoint);
                    return Err(VirtioNetworkMergedRxDispatchError::AvailableRing { source });
                }
            };
            let Some(descriptor_head) = descriptor_chain_head(&chain) else {
                self.available.restore_checkpoint(checkpoint);
                return Err(VirtioNetworkMergedRxDispatchError::EmptyDescriptorChain);
            };
            let buffer = match VirtioNetworkRxBuffer::parse_with_minimum(
                memory,
                &chain,
                u64::from(VIRTIO_NET_TX_HEADER_SIZE),
            ) {
                Ok(buffer) => buffer,
                Err(source) => {
                    self.available.restore_checkpoint(checkpoint);
                    return Err(VirtioNetworkMergedRxDispatchError::BufferParse {
                        descriptor_head,
                        source,
                    });
                }
            };
            capacity = capacity.checked_add(buffer.len()).ok_or_else(|| {
                self.available.restore_checkpoint(checkpoint);
                VirtioNetworkMergedRxDispatchError::BufferParse {
                    descriptor_head,
                    source: VirtioNetworkRxBufferParseError::BufferLengthOverflow {
                        current: capacity,
                        len: u32::try_from(buffer.len()).unwrap_or(u32::MAX),
                    },
                }
            })?;
            buffers.push(buffer);
        }

        let buffer_count = u16::try_from(buffers.len()).map_err(|_| {
            self.available.restore_checkpoint(checkpoint);
            VirtioNetworkMergedRxDispatchError::EmptyDescriptorChain
        })?;
        let Some(first_buffer) = buffers.first() else {
            self.available.restore_checkpoint(checkpoint);
            return Err(VirtioNetworkMergedRxDispatchError::EmptyDescriptorChain);
        };
        let descriptor_head = first_buffer.descriptor_head();

        let mut elements = Vec::new();
        elements
            .try_reserve_exact(buffers.len())
            .map_err(|source| {
                self.available.restore_checkpoint(checkpoint);
                VirtioNetworkMergedRxDispatchError::MetadataAllocation { source }
            })?;
        let mut remaining = u64::from(bytes_written_to_guest);
        for buffer in &buffers {
            let used_len = remaining.min(buffer.len());
            let used_len = u32::try_from(used_len).map_err(|_| {
                self.available.restore_checkpoint(checkpoint);
                VirtioNetworkMergedRxDispatchError::BufferWrite {
                    descriptor_head,
                    source: VirtioNetworkRxFrameWriteError::IncompleteFrame {
                        remaining_bytes: usize::MAX,
                    },
                }
            })?;
            elements.push((buffer.descriptor_head(), used_len));
            remaining = remaining.saturating_sub(u64::from(used_len));
        }
        let notification_suppression = match self.notification_suppression(memory) {
            Ok(notification_suppression) => notification_suppression,
            Err(source) => {
                self.available.restore_checkpoint(checkpoint);
                return Err(VirtioNetworkMergedRxDispatchError::AvailableRing { source });
            }
        };

        let mut rate_limiter = rate_limiter;
        let limiter_reservation = match rate_limiter.as_deref_mut() {
            Some(limiter) => match limiter.reduce_at(u64::from(bytes_written_to_guest), now) {
                VirtioNetworkRateLimiterReduction::Allowed(reservation) => Some(reservation),
                VirtioNetworkRateLimiterReduction::Throttled { retry_after } => {
                    self.available.restore_checkpoint(checkpoint);
                    return Ok(VirtioNetworkMergedRxDispatch::RateLimited { retry_after });
                }
            },
            None => None,
        };

        if let Err(source) = write_rx_packet_to_buffers(memory, &buffers, packet, buffer_count) {
            if let (Some(limiter), Some(reservation)) = (rate_limiter, limiter_reservation) {
                limiter.restore(reservation);
            }
            self.available.restore_checkpoint(checkpoint);
            return Err(VirtioNetworkMergedRxDispatchError::BufferWrite {
                descriptor_head,
                source,
            });
        }

        let publication = match self.used.publish_used_elements_with_notification(
            memory,
            &elements,
            notification_suppression,
        ) {
            Ok(publication) => publication,
            Err(source) => {
                if let (Some(limiter), Some(reservation)) = (rate_limiter, limiter_reservation) {
                    limiter.restore(reservation);
                }
                self.available.restore_checkpoint(checkpoint);
                return Err(VirtioNetworkMergedRxDispatchError::UsedRing {
                    descriptor_head,
                    source,
                });
            }
        };
        Ok(VirtioNetworkMergedRxDispatch::Delivered(
            VirtioNetworkRxQueueDispatchOutcome::Delivered(VirtioNetworkRxPacketDelivery {
                descriptor_head,
                packet_len,
                bytes_written_to_guest,
                buffer_count,
            }),
            publication,
        ))
    }

    fn notification_suppression(
        &self,
        memory: &GuestMemory,
    ) -> Result<VirtqueueNotificationSuppression, VirtqueueAvailableRingError> {
        if self.event_idx_enabled {
            Ok(VirtqueueNotificationSuppression::EventIdx {
                used_event: self.available.used_event(memory)?,
                avail_event: self.available.next_avail(),
            })
        } else {
            Ok(VirtqueueNotificationSuppression::Disabled)
        }
    }
}

#[derive(Debug)]
enum VirtioNetworkMergedRxDispatch {
    Delivered(
        VirtioNetworkRxQueueDispatchOutcome,
        VirtqueueUsedRingPublication,
    ),
    NoAvailableBuffers,
    RateLimited {
        retry_after: Duration,
    },
}

#[derive(Debug)]
enum VirtioNetworkMergedRxDispatchError {
    MetadataAllocation {
        source: TryReserveError,
    },
    AvailableRing {
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain,
    BufferParse {
        descriptor_head: u16,
        source: VirtioNetworkRxBufferParseError,
    },
    BufferWrite {
        descriptor_head: u16,
        source: VirtioNetworkRxFrameWriteError,
    },
    UsedRing {
        descriptor_head: u16,
        source: VirtqueueUsedRingError,
    },
}

#[derive(Debug)]
pub enum VirtioNetworkRxQueueBuildError {
    QueueNotReady,
    AvailableRing { source: VirtqueueAvailableRingError },
    UsedRing { source: VirtqueueUsedRingError },
}

impl fmt::Display for VirtioNetworkRxQueueBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("virtio-net RX queue is not ready"),
            Self::AvailableRing { source } => {
                write!(f, "failed to build virtio-net RX available ring: {source}")
            }
            Self::UsedRing { source } => {
                write!(f, "failed to build virtio-net RX used ring: {source}")
            }
        }
    }
}

impl std::error::Error for VirtioNetworkRxQueueBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source } => Some(source),
            Self::QueueNotReady => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkRxPacketDelivery {
    descriptor_head: u16,
    packet_len: u64,
    bytes_written_to_guest: u32,
    buffer_count: u16,
}

impl VirtioNetworkRxPacketDelivery {
    pub const fn descriptor_head(self) -> u16 {
        self.descriptor_head
    }

    pub const fn packet_len(self) -> u64 {
        self.packet_len
    }

    pub const fn bytes_written_to_guest(self) -> u32 {
        self.bytes_written_to_guest
    }

    pub const fn buffer_count(self) -> u16 {
        self.buffer_count
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioNetworkRxBufferTooSmall {
    descriptor_head: u16,
    len: u64,
    required_len: u64,
}

impl VirtioNetworkRxBufferTooSmall {
    pub const fn descriptor_head(self) -> u16 {
        self.descriptor_head
    }

    pub const fn len(self) -> u64 {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub const fn required_len(self) -> u64 {
        self.required_len
    }
}

#[derive(Debug)]
pub struct VirtioNetworkRxQueueDispatch {
    processed_buffers: usize,
    delivered_packets: usize,
    buffer_parse_failures: usize,
    buffer_too_small_failures: usize,
    source_failures: usize,
    no_available_buffers: usize,
    rate_limiter_throttled_packets: usize,
    rate_limiter_retry_after: Option<Duration>,
    deliveries: Vec<VirtioNetworkRxPacketDelivery>,
    first_buffer_parse_failure: Option<VirtioNetworkRxBufferParseError>,
    first_buffer_too_small: Option<VirtioNetworkRxBufferTooSmall>,
    first_source_failure: Option<VirtioNetworkRxPacketSourceError>,
    needs_queue_interrupt: bool,
    backend_metrics: VirtioNetworkBackendMetrics,
}

impl VirtioNetworkRxQueueDispatch {
    fn with_capacity(queue_size: u16) -> Result<Self, VirtioNetworkRxQueueDispatchError> {
        let mut deliveries = Vec::new();
        deliveries
            .try_reserve_exact(usize::from(queue_size))
            .map_err(
                |source| VirtioNetworkRxQueueDispatchError::PacketMetadataAllocation {
                    completed_dispatch: None,
                    source,
                },
            )?;

        Ok(Self {
            processed_buffers: 0,
            delivered_packets: 0,
            buffer_parse_failures: 0,
            buffer_too_small_failures: 0,
            source_failures: 0,
            no_available_buffers: 0,
            rate_limiter_throttled_packets: 0,
            rate_limiter_retry_after: None,
            deliveries,
            first_buffer_parse_failure: None,
            first_buffer_too_small: None,
            first_source_failure: None,
            needs_queue_interrupt: false,
            backend_metrics: VirtioNetworkBackendMetrics::default(),
        })
    }

    pub const fn processed_buffers(&self) -> usize {
        self.processed_buffers
    }

    pub const fn delivered_packets(&self) -> usize {
        self.delivered_packets
    }

    pub const fn buffer_parse_failures(&self) -> usize {
        self.buffer_parse_failures
    }

    pub const fn buffer_too_small_failures(&self) -> usize {
        self.buffer_too_small_failures
    }

    pub const fn source_failures(&self) -> usize {
        self.source_failures
    }

    pub const fn no_available_buffers(&self) -> usize {
        self.no_available_buffers
    }

    pub const fn rate_limiter_throttled_packets(&self) -> usize {
        self.rate_limiter_throttled_packets
    }

    pub const fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
    }

    pub fn deliveries(&self) -> &[VirtioNetworkRxPacketDelivery] {
        &self.deliveries
    }

    pub const fn first_buffer_parse_failure(&self) -> Option<&VirtioNetworkRxBufferParseError> {
        self.first_buffer_parse_failure.as_ref()
    }

    pub const fn first_buffer_too_small(&self) -> Option<VirtioNetworkRxBufferTooSmall> {
        self.first_buffer_too_small
    }

    pub const fn first_source_failure(&self) -> Option<&VirtioNetworkRxPacketSourceError> {
        self.first_source_failure.as_ref()
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.needs_queue_interrupt
    }

    pub const fn backend_metrics(&self) -> VirtioNetworkBackendMetrics {
        self.backend_metrics
    }

    fn record(
        &mut self,
        outcome: VirtioNetworkRxQueueDispatchOutcome,
        publication: VirtqueueUsedRingPublication,
    ) {
        self.processed_buffers += match &outcome {
            VirtioNetworkRxQueueDispatchOutcome::Delivered(delivery) => {
                usize::from(delivery.buffer_count())
            }
            VirtioNetworkRxQueueDispatchOutcome::BufferParseError(_)
            | VirtioNetworkRxQueueDispatchOutcome::BufferTooSmall(_) => 1,
        };
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        match outcome {
            VirtioNetworkRxQueueDispatchOutcome::Delivered(delivery) => {
                self.delivered_packets += 1;
                self.deliveries.push(delivery);
            }
            VirtioNetworkRxQueueDispatchOutcome::BufferParseError(source) => {
                self.buffer_parse_failures += 1;
                if self.first_buffer_parse_failure.is_none() {
                    self.first_buffer_parse_failure = Some(source);
                }
            }
            VirtioNetworkRxQueueDispatchOutcome::BufferTooSmall(failure) => {
                self.buffer_too_small_failures += 1;
                if self.first_buffer_too_small.is_none() {
                    self.first_buffer_too_small = Some(failure);
                }
            }
        }
    }

    fn record_source_failure(&mut self, source: VirtioNetworkRxPacketSourceError) {
        self.source_failures += 1;
        if self.first_source_failure.is_none() {
            self.first_source_failure = Some(source);
        }
    }

    fn record_no_available_buffer(&mut self) {
        self.no_available_buffers += 1;
    }

    fn record_backend_metrics(&mut self, metrics: VirtioNetworkBackendMetrics) {
        self.backend_metrics = self.backend_metrics.merged_with(metrics);
    }

    fn record_rate_limited_packet(&mut self, retry_after: Duration) {
        self.rate_limiter_throttled_packets += 1;
        self.rate_limiter_retry_after = Some(match self.rate_limiter_retry_after {
            Some(existing) => existing.min(retry_after),
            None => retry_after,
        });
    }
}

#[derive(Debug)]
enum VirtioNetworkRxQueueDispatchOutcome {
    Delivered(VirtioNetworkRxPacketDelivery),
    BufferParseError(VirtioNetworkRxBufferParseError),
    BufferTooSmall(VirtioNetworkRxBufferTooSmall),
}

#[derive(Debug)]
enum VirtioNetworkRxQueueDispatchAction {
    Record(
        VirtioNetworkRxQueueDispatchOutcome,
        VirtqueueUsedRingPublication,
    ),
    Consume(
        VirtioNetworkRxQueueDispatchOutcome,
        VirtqueueUsedRingPublication,
    ),
}

#[derive(Debug)]
pub enum VirtioNetworkRxFrameWriteError {
    SegmentOffsetTooLarge {
        descriptor_index: u16,
        offset: usize,
    },
    SegmentAddressOverflow {
        descriptor_index: u16,
        address: GuestAddress,
        offset: u64,
    },
    SegmentWrite {
        descriptor_index: u16,
        address: GuestAddress,
        len: usize,
        source: GuestMemoryAccessError,
    },
    IncompleteFrame {
        remaining_bytes: usize,
    },
}

impl fmt::Display for VirtioNetworkRxFrameWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SegmentOffsetTooLarge {
                descriptor_index,
                offset,
            } => {
                write!(
                    f,
                    "virtio-net RX buffer descriptor {descriptor_index} offset {offset} is too large"
                )
            }
            Self::SegmentAddressOverflow {
                descriptor_index,
                address,
                offset,
            } => {
                write!(
                    f,
                    "virtio-net RX buffer descriptor {descriptor_index} at {address} overflows when adding offset {offset}"
                )
            }
            Self::SegmentWrite {
                descriptor_index,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "failed to write {len} bytes into virtio-net RX buffer descriptor {descriptor_index} at {address}: {source}"
                )
            }
            Self::IncompleteFrame { remaining_bytes } => {
                write!(
                    f,
                    "virtio-net RX buffer write finished with {remaining_bytes} frame bytes remaining"
                )
            }
        }
    }
}

impl std::error::Error for VirtioNetworkRxFrameWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SegmentWrite { source, .. } => Some(source),
            Self::SegmentOffsetTooLarge { .. }
            | Self::SegmentAddressOverflow { .. }
            | Self::IncompleteFrame { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioNetworkRxQueueDispatchError {
    PacketMetadataAllocation {
        completed_dispatch: Option<Box<VirtioNetworkRxQueueDispatch>>,
        source: TryReserveError,
    },
    PacketSource {
        completed_dispatch: Box<VirtioNetworkRxQueueDispatch>,
        source: VirtioNetworkRxPacketSourceError,
    },
    PacketTooLarge {
        completed_dispatch: Box<VirtioNetworkRxQueueDispatch>,
        len: u64,
        max: u64,
    },
    AvailableRing {
        completed_dispatch: Box<VirtioNetworkRxQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain {
        completed_dispatch: Box<VirtioNetworkRxQueueDispatch>,
    },
    BufferParse {
        completed_dispatch: Box<VirtioNetworkRxQueueDispatch>,
        descriptor_head: u16,
        source: VirtioNetworkRxBufferParseError,
    },
    UsedRing {
        completed_dispatch: Box<VirtioNetworkRxQueueDispatch>,
        descriptor_head: u16,
        bytes_written_to_guest: u32,
        source: VirtqueueUsedRingError,
    },
    BufferWrite {
        completed_dispatch: Box<VirtioNetworkRxQueueDispatch>,
        descriptor_head: u16,
        source: VirtioNetworkRxFrameWriteError,
    },
}

impl VirtioNetworkRxQueueDispatchError {
    pub const fn completed_dispatch(&self) -> Option<&VirtioNetworkRxQueueDispatch> {
        match self {
            Self::PacketMetadataAllocation {
                completed_dispatch, ..
            } => match completed_dispatch {
                Some(completed_dispatch) => Some(completed_dispatch),
                None => None,
            },
            Self::PacketSource {
                completed_dispatch, ..
            }
            | Self::PacketTooLarge {
                completed_dispatch, ..
            }
            | Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::BufferParse {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            }
            | Self::BufferWrite {
                completed_dispatch, ..
            } => Some(completed_dispatch),
        }
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.completed_dispatch()
            .and_then(VirtioNetworkRxQueueDispatch::rate_limiter_retry_after)
    }

    fn record_backend_metrics(&mut self, metrics: VirtioNetworkBackendMetrics) {
        match self {
            Self::PacketMetadataAllocation {
                completed_dispatch: Some(completed_dispatch),
                ..
            } => completed_dispatch.record_backend_metrics(metrics),
            Self::PacketSource {
                completed_dispatch, ..
            }
            | Self::PacketTooLarge {
                completed_dispatch, ..
            }
            | Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::BufferParse {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            }
            | Self::BufferWrite {
                completed_dispatch, ..
            } => completed_dispatch.record_backend_metrics(metrics),
            Self::PacketMetadataAllocation {
                completed_dispatch: None,
                ..
            } => {}
        }
    }
}

impl fmt::Display for VirtioNetworkRxQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PacketMetadataAllocation { source, .. } => {
                write!(
                    f,
                    "failed to reserve virtio-net RX packet metadata: {source}"
                )
            }
            Self::PacketSource { source, .. } => {
                write!(f, "failed to read virtio-net RX packet source: {source}")
            }
            Self::PacketTooLarge { len, max, .. } => {
                write!(f, "virtio-net RX packet length {len} exceeds maximum {max}")
            }
            Self::AvailableRing { source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-net RX available descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain { .. } => {
                f.write_str("virtio-net RX queue produced an empty descriptor chain")
            }
            Self::BufferParse {
                descriptor_head,
                source,
                ..
            } => write!(
                f,
                "failed to validate merged virtio-net RX descriptor head {descriptor_head}: {source}"
            ),
            Self::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to publish virtio-net RX used descriptor head {descriptor_head} with length {bytes_written_to_guest}: {source}"
                )
            }
            Self::BufferWrite {
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to write virtio-net RX frame into descriptor head {descriptor_head}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioNetworkRxQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PacketMetadataAllocation { source, .. } => Some(source),
            Self::PacketSource { source, .. } => Some(source),
            Self::AvailableRing { source, .. } => Some(source),
            Self::BufferParse { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::BufferWrite { source, .. } => Some(source),
            Self::PacketTooLarge { .. } | Self::EmptyDescriptorChain { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct VirtioNetworkRxPacketLengthError {
    len: u64,
    max: u64,
}

fn rx_packet_used_len(packet_len: usize) -> Result<u32, VirtioNetworkRxPacketLengthError> {
    let packet_len = u64::try_from(packet_len).map_err(|_| VirtioNetworkRxPacketLengthError {
        len: u64::MAX,
        max: VIRTIO_NET_MAX_BUFFER_SIZE,
    })?;
    let len = u64::from(VIRTIO_NET_TX_HEADER_SIZE)
        .checked_add(packet_len)
        .ok_or(VirtioNetworkRxPacketLengthError {
            len: u64::MAX,
            max: VIRTIO_NET_MAX_BUFFER_SIZE,
        })?;
    if len > VIRTIO_NET_MAX_BUFFER_SIZE {
        return Err(VirtioNetworkRxPacketLengthError {
            len,
            max: VIRTIO_NET_MAX_BUFFER_SIZE,
        });
    }

    u32::try_from(len).map_err(|_| VirtioNetworkRxPacketLengthError {
        len,
        max: VIRTIO_NET_MAX_BUFFER_SIZE,
    })
}

fn write_rx_packet_to_buffer(
    memory: &mut GuestMemory,
    buffer: &VirtioNetworkRxBuffer,
    packet: VirtioNetworkRxPacket<'_>,
) -> Result<(), VirtioNetworkRxFrameWriteError> {
    write_rx_packet_to_buffers(memory, std::slice::from_ref(buffer), packet, 1)
}

fn write_rx_packet_to_buffers(
    memory: &mut GuestMemory,
    buffers: &[VirtioNetworkRxBuffer],
    packet: VirtioNetworkRxPacket<'_>,
    buffer_count: u16,
) -> Result<(), VirtioNetworkRxFrameWriteError> {
    let mut header = [0; VIRTIO_NET_TX_HEADER_SIZE as usize];
    let remaining_bytes = header.len().saturating_add(packet.len());
    let num_buffers_offset = header.len().saturating_sub(2);
    let num_buffers = header
        .get_mut(num_buffers_offset..)
        .ok_or(VirtioNetworkRxFrameWriteError::IncompleteFrame { remaining_bytes })?;
    num_buffers.copy_from_slice(&buffer_count.to_le_bytes());
    let mut header_remaining = header.as_slice();
    let mut payload_remaining = packet.bytes();

    for buffer in buffers {
        for segment in buffer.segments() {
            let mut segment_offset = 0;
            let mut segment_remaining = segment.len() as usize;

            if !header_remaining.is_empty() && segment_remaining != 0 {
                let write_len = header_remaining.len().min(segment_remaining);
                let (bytes, remaining) = header_remaining.split_at(write_len);
                write_rx_segment_bytes(memory, *segment, segment_offset, bytes)?;
                header_remaining = remaining;
                segment_offset += write_len;
                segment_remaining -= write_len;
            }

            if header_remaining.is_empty()
                && !payload_remaining.is_empty()
                && segment_remaining != 0
            {
                let write_len = payload_remaining.len().min(segment_remaining);
                let (bytes, remaining) = payload_remaining.split_at(write_len);
                write_rx_segment_bytes(memory, *segment, segment_offset, bytes)?;
                payload_remaining = remaining;
            }

            if header_remaining.is_empty() && payload_remaining.is_empty() {
                return Ok(());
            }
        }
    }

    Err(VirtioNetworkRxFrameWriteError::IncompleteFrame {
        remaining_bytes: header_remaining
            .len()
            .saturating_add(payload_remaining.len()),
    })
}

fn write_rx_segment_bytes(
    memory: &mut GuestMemory,
    segment: VirtioNetworkRxBufferSegment,
    offset: usize,
    bytes: &[u8],
) -> Result<(), VirtioNetworkRxFrameWriteError> {
    if bytes.is_empty() {
        return Ok(());
    }

    let offset = u64::try_from(offset).map_err(|_| {
        VirtioNetworkRxFrameWriteError::SegmentOffsetTooLarge {
            descriptor_index: segment.descriptor_index(),
            offset,
        }
    })?;
    let address = segment.address().checked_add(offset).ok_or(
        VirtioNetworkRxFrameWriteError::SegmentAddressOverflow {
            descriptor_index: segment.descriptor_index(),
            address: segment.address(),
            offset,
        },
    )?;
    memory.write_slice(bytes, address).map_err(|source| {
        VirtioNetworkRxFrameWriteError::SegmentWrite {
            descriptor_index: segment.descriptor_index(),
            address,
            len: bytes.len(),
            source,
        }
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioNetworkTxQueue {
    queue_state: VirtioMmioQueueState,
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
    event_idx_enabled: bool,
    negotiated_features: u64,
}

impl VirtioNetworkTxQueue {
    pub fn from_mmio_queue_state(
        queue: VirtioMmioQueueState,
    ) -> Result<Self, VirtioNetworkTxQueueBuildError> {
        Self::from_mmio_queue_state_with_event_idx(queue, false, false, 0)
    }

    fn from_mmio_queue_state_with_event_idx(
        queue: VirtioMmioQueueState,
        event_idx_enabled: bool,
        indirect_descriptors_enabled: bool,
        negotiated_features: u64,
    ) -> Result<Self, VirtioNetworkTxQueueBuildError> {
        if !queue.ready() {
            return Err(VirtioNetworkTxQueueBuildError::QueueNotReady);
        }

        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioNetworkTxQueueBuildError::AvailableRing { source })?;
        let available = available.with_descriptor_chain_options(
            VirtqueueDescriptorChainOptions::new()
                .with_indirect_descriptors(indirect_descriptors_enabled),
        );
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioNetworkTxQueueBuildError::UsedRing { source })?;

        Ok(Self {
            queue_state: queue,
            available,
            used,
            event_idx_enabled,
            negotiated_features,
        })
    }

    pub const fn queue_state(&self) -> VirtioMmioQueueState {
        self.queue_state
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
    }

    pub const fn event_idx_enabled(&self) -> bool {
        self.event_idx_enabled
    }

    pub const fn negotiated_features(&self) -> u64 {
        self.negotiated_features
    }

    pub fn dispatch(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioNetworkTxQueueDispatch, VirtioNetworkTxQueueDispatchError> {
        let mut sink = NoopVirtioNetworkTxPacketSink;
        self.dispatch_with_sink(memory, &mut sink)
    }

    pub fn dispatch_with_sink(
        &mut self,
        memory: &mut GuestMemory,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
    ) -> Result<VirtioNetworkTxQueueDispatch, VirtioNetworkTxQueueDispatchError> {
        self.dispatch_with_sink_at(memory, tx_sink, None, Instant::now())
    }

    fn dispatch_with_sink_at(
        &mut self,
        memory: &mut GuestMemory,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
        rate_limiter: Option<&mut VirtioNetworkRateLimiter>,
        now: Instant,
    ) -> Result<VirtioNetworkTxQueueDispatch, VirtioNetworkTxQueueDispatchError> {
        let mut result = if tx_sink.supports_staged_batch() {
            self.dispatch_with_staged_sink_at(memory, tx_sink, rate_limiter, now)
        } else {
            self.dispatch_with_single_sink_at(memory, tx_sink, rate_limiter, now)
        };
        if let Err(source) = &mut result {
            source.record_backend_metrics(tx_sink.take_backend_metrics());
        }
        result
    }

    fn dispatch_with_single_sink_at(
        &mut self,
        memory: &mut GuestMemory,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
        rate_limiter: Option<&mut VirtioNetworkRateLimiter>,
        now: Instant,
    ) -> Result<VirtioNetworkTxQueueDispatch, VirtioNetworkTxQueueDispatchError> {
        let mut dispatch =
            VirtioNetworkTxQueueDispatch::with_capacity(self.available.queue_size())?;
        let mut rate_limiter = rate_limiter;
        while let Some(chain) = match self.available.pop_descriptor_chain(memory) {
            Ok(chain) => chain,
            Err(source) => {
                return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                    completed_dispatch: Box::new(dispatch),
                    source,
                });
            }
        } {
            let remaining_requests = match self.available.available_descriptor_count(memory) {
                Ok(remaining_requests) => remaining_requests,
                Err(source) => {
                    if let Err(undo_source) = self.available.undo_pop_descriptor_chain() {
                        return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                            completed_dispatch: Box::new(dispatch),
                            source: undo_source,
                        });
                    }
                    return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            };
            dispatch.record_remaining_requests(remaining_requests);
            let descriptor_head = match descriptor_chain_head(&chain) {
                Some(descriptor_head) => descriptor_head,
                None => {
                    return Err(VirtioNetworkTxQueueDispatchError::EmptyDescriptorChain {
                        completed_dispatch: Box::new(dispatch),
                    });
                }
            };
            let preparation = match VirtioNetworkTxFrame::parse_with_features(
                memory,
                &chain,
                self.negotiated_features,
            ) {
                Ok(frame) => match frame.prepare_packet(memory) {
                    Ok(packet) => VirtioNetworkTxFramePreparation::Ready { frame, packet },
                    Err(source) => VirtioNetworkTxFramePreparation::PacketError(source),
                },
                Err(source) => VirtioNetworkTxFramePreparation::ParseError(source),
            };
            let reservation = if let VirtioNetworkTxFramePreparation::Ready { frame, .. } =
                &preparation
                && let Some(limiter) = rate_limiter.as_deref_mut()
            {
                match limiter.reduce_at(frame.frame_len(), now) {
                    VirtioNetworkRateLimiterReduction::Allowed(reservation) => Some(reservation),
                    VirtioNetworkRateLimiterReduction::Throttled { retry_after } => {
                        if let Err(source) = self.available.undo_pop_descriptor_chain() {
                            return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                                completed_dispatch: Box::new(dispatch),
                                source,
                            });
                        }
                        dispatch.record_rate_limited_frame(retry_after);
                        break;
                    }
                }
            } else {
                None
            };
            let notification_suppression = match self.notification_suppression(memory) {
                Ok(notification_suppression) => notification_suppression,
                Err(source) => {
                    if let (Some(limiter), Some(reservation)) =
                        (rate_limiter.as_deref_mut(), reservation)
                    {
                        limiter.restore(reservation);
                    }
                    return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            };
            let publication = match self.used.publish_used_element_with_notification(
                memory,
                descriptor_head,
                0,
                notification_suppression,
            ) {
                Ok(publication) => publication,
                Err(source) => {
                    if let (Some(limiter), Some(reservation)) =
                        (rate_limiter.as_deref_mut(), reservation)
                    {
                        limiter.restore(reservation);
                    }
                    return Err(VirtioNetworkTxQueueDispatchError::UsedRing {
                        completed_dispatch: Box::new(dispatch),
                        descriptor_head,
                        bytes_written_to_guest: 0,
                        source,
                    });
                }
            };
            let outcome = match preparation {
                VirtioNetworkTxFramePreparation::Ready { frame, packet } => {
                    let sink_error = match tx_sink.transmit_prepared_frame(memory, &frame, &packet)
                    {
                        Ok(VirtioNetworkTxPacketDisposition::Detoured) => {
                            if let (Some(limiter), Some(reservation)) =
                                (rate_limiter.as_deref_mut(), reservation)
                            {
                                limiter.restore(reservation);
                            }
                            None
                        }
                        Ok(VirtioNetworkTxPacketDisposition::Forwarded) => None,
                        Err(source) => Some(source),
                    };
                    VirtioNetworkTxQueueDispatchOutcome::Ok { frame, sink_error }
                }
                VirtioNetworkTxFramePreparation::PacketError(source) => {
                    VirtioNetworkTxQueueDispatchOutcome::PacketPrepareError(source)
                }
                VirtioNetworkTxFramePreparation::ParseError(source) => {
                    VirtioNetworkTxQueueDispatchOutcome::ParseError(source)
                }
            };
            dispatch.record(outcome, publication);
        }

        dispatch.record_backend_metrics(tx_sink.take_backend_metrics());
        Ok(dispatch)
    }

    fn dispatch_with_staged_sink_at(
        &mut self,
        memory: &mut GuestMemory,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
        rate_limiter: Option<&mut VirtioNetworkRateLimiter>,
        now: Instant,
    ) -> Result<VirtioNetworkTxQueueDispatch, VirtioNetworkTxQueueDispatchError> {
        let queue_size = self.available.queue_size();
        let mut dispatch = VirtioNetworkTxQueueDispatch::with_capacity(queue_size)?;
        let mut pending_frames = Vec::new();
        pending_frames
            .try_reserve_exact(usize::from(queue_size))
            .map_err(
                |source| VirtioNetworkTxQueueDispatchError::FrameMetadataAllocation { source },
            )?;
        let mut flush_results = Vec::new();
        flush_results
            .try_reserve_exact(usize::from(queue_size))
            .map_err(
                |source| VirtioNetworkTxQueueDispatchError::FrameMetadataAllocation { source },
            )?;
        let mut rate_limiter = rate_limiter;

        loop {
            let chain = match self.available.pop_descriptor_chain(memory) {
                Ok(Some(chain)) => chain,
                Ok(None) => break,
                Err(source) => {
                    flush_staged_tx_frames(
                        tx_sink,
                        &mut dispatch,
                        &mut pending_frames,
                        &mut flush_results,
                    );
                    return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            };
            let Some(descriptor_head) = descriptor_chain_head(&chain) else {
                flush_staged_tx_frames(
                    tx_sink,
                    &mut dispatch,
                    &mut pending_frames,
                    &mut flush_results,
                );
                return Err(VirtioNetworkTxQueueDispatchError::EmptyDescriptorChain {
                    completed_dispatch: Box::new(dispatch),
                });
            };
            let remaining_requests = match self.available.available_descriptor_count(memory) {
                Ok(remaining_requests) => remaining_requests,
                Err(source) => {
                    flush_staged_tx_frames(
                        tx_sink,
                        &mut dispatch,
                        &mut pending_frames,
                        &mut flush_results,
                    );
                    if let Err(undo_source) = self.available.undo_pop_descriptor_chain() {
                        return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                            completed_dispatch: Box::new(dispatch),
                            source: undo_source,
                        });
                    }
                    return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            };
            dispatch.record_remaining_requests(remaining_requests);
            let preparation = match VirtioNetworkTxFrame::parse_with_features(
                memory,
                &chain,
                self.negotiated_features,
            ) {
                Ok(frame) => match frame.prepare_packet(memory) {
                    Ok(packet) => VirtioNetworkTxFramePreparation::Ready { frame, packet },
                    Err(source) => VirtioNetworkTxFramePreparation::PacketError(source),
                },
                Err(source) => VirtioNetworkTxFramePreparation::ParseError(source),
            };
            let (frame, packet) = match preparation.into_ready() {
                Ok(ready) => ready,
                Err(outcome) => {
                    flush_staged_tx_frames(
                        tx_sink,
                        &mut dispatch,
                        &mut pending_frames,
                        &mut flush_results,
                    );
                    let notification_suppression = match self.notification_suppression(memory) {
                        Ok(notification_suppression) => notification_suppression,
                        Err(source) => {
                            return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                                completed_dispatch: Box::new(dispatch),
                                source,
                            });
                        }
                    };
                    let publication = match self.used.publish_used_element_with_notification(
                        memory,
                        descriptor_head,
                        0,
                        notification_suppression,
                    ) {
                        Ok(publication) => publication,
                        Err(publication_source) => {
                            return Err(VirtioNetworkTxQueueDispatchError::UsedRing {
                                completed_dispatch: Box::new(dispatch),
                                descriptor_head,
                                bytes_written_to_guest: 0,
                                source: publication_source,
                            });
                        }
                    };
                    dispatch.record(outcome, publication);
                    continue;
                }
            };
            let reservation = if let Some(limiter) = rate_limiter.as_deref_mut() {
                match limiter.reduce_at(frame.frame_len(), now) {
                    VirtioNetworkRateLimiterReduction::Allowed(reservation) => Some(reservation),
                    VirtioNetworkRateLimiterReduction::Throttled { retry_after } => {
                        flush_staged_tx_frames(
                            tx_sink,
                            &mut dispatch,
                            &mut pending_frames,
                            &mut flush_results,
                        );
                        if let Err(source) = self.available.undo_pop_descriptor_chain() {
                            return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                                completed_dispatch: Box::new(dispatch),
                                source,
                            });
                        }
                        dispatch.record_rate_limited_frame(retry_after);
                        break;
                    }
                }
            } else {
                None
            };

            let mut staged = false;
            let mut stage_error = None;
            let mut retried_after_flush = false;
            loop {
                match tx_sink.stage_prepared_frame(memory, &frame, &packet) {
                    Ok(VirtioNetworkTxPacketStage::Staged {
                        flush_before_commit,
                    }) => {
                        staged = true;
                        if flush_before_commit {
                            flush_staged_tx_frames(
                                tx_sink,
                                &mut dispatch,
                                &mut pending_frames,
                                &mut flush_results,
                            );
                        }
                        break;
                    }
                    Ok(VirtioNetworkTxPacketStage::FlushRequired)
                        if !pending_frames.is_empty() && !retried_after_flush =>
                    {
                        flush_staged_tx_frames(
                            tx_sink,
                            &mut dispatch,
                            &mut pending_frames,
                            &mut flush_results,
                        );
                        retried_after_flush = true;
                    }
                    Ok(VirtioNetworkTxPacketStage::FlushRequired) => {
                        stage_error = Some(VirtioNetworkTxPacketSinkError::new(
                            "virtio-net staged frame exceeds an empty batch bound",
                        ));
                        break;
                    }
                    Err(source) => {
                        flush_staged_tx_frames(
                            tx_sink,
                            &mut dispatch,
                            &mut pending_frames,
                            &mut flush_results,
                        );
                        stage_error = Some(source);
                        break;
                    }
                }
            }

            let notification_suppression = match self.notification_suppression(memory) {
                Ok(notification_suppression) => notification_suppression,
                Err(source) => {
                    if staged {
                        tx_sink.discard_staged_frame();
                    }
                    if let (Some(limiter), Some(reservation)) =
                        (rate_limiter.as_deref_mut(), reservation)
                    {
                        limiter.restore(reservation);
                    }
                    flush_staged_tx_frames(
                        tx_sink,
                        &mut dispatch,
                        &mut pending_frames,
                        &mut flush_results,
                    );
                    return Err(VirtioNetworkTxQueueDispatchError::AvailableRing {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            };
            let publication = match self.used.publish_used_element_with_notification(
                memory,
                descriptor_head,
                0,
                notification_suppression,
            ) {
                Ok(publication) => publication,
                Err(source) => {
                    if staged {
                        tx_sink.discard_staged_frame();
                    }
                    if let (Some(limiter), Some(reservation)) =
                        (rate_limiter.as_deref_mut(), reservation)
                    {
                        limiter.restore(reservation);
                    }
                    flush_staged_tx_frames(
                        tx_sink,
                        &mut dispatch,
                        &mut pending_frames,
                        &mut flush_results,
                    );
                    return Err(VirtioNetworkTxQueueDispatchError::UsedRing {
                        completed_dispatch: Box::new(dispatch),
                        descriptor_head,
                        bytes_written_to_guest: 0,
                        source,
                    });
                }
            };

            if let Some(source) = stage_error {
                dispatch.record(
                    VirtioNetworkTxQueueDispatchOutcome::Ok {
                        frame,
                        sink_error: Some(source),
                    },
                    publication,
                );
                continue;
            }

            match tx_sink.commit_staged_frame() {
                VirtioNetworkTxPacketCommit::Deferred => {
                    let frame_index = dispatch.record_deferred(frame, publication);
                    pending_frames.push(frame_index);
                }
                VirtioNetworkTxPacketCommit::Immediate(result) => {
                    if matches!(result, Ok(VirtioNetworkTxPacketDisposition::Detoured))
                        && let (Some(limiter), Some(reservation)) =
                            (rate_limiter.as_deref_mut(), reservation)
                    {
                        limiter.restore(reservation);
                    }
                    dispatch.record(
                        VirtioNetworkTxQueueDispatchOutcome::Ok {
                            frame,
                            sink_error: result.err(),
                        },
                        publication,
                    );
                }
            }
        }

        flush_staged_tx_frames(
            tx_sink,
            &mut dispatch,
            &mut pending_frames,
            &mut flush_results,
        );
        dispatch.record_backend_metrics(tx_sink.take_backend_metrics());
        Ok(dispatch)
    }

    fn notification_suppression(
        &self,
        memory: &GuestMemory,
    ) -> Result<VirtqueueNotificationSuppression, VirtqueueAvailableRingError> {
        if self.event_idx_enabled {
            Ok(VirtqueueNotificationSuppression::EventIdx {
                used_event: self.available.used_event(memory)?,
                avail_event: self.available.next_avail(),
            })
        } else {
            Ok(VirtqueueNotificationSuppression::Disabled)
        }
    }
}

fn flush_staged_tx_frames(
    tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
    dispatch: &mut VirtioNetworkTxQueueDispatch,
    pending_frames: &mut Vec<usize>,
    results: &mut Vec<Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError>>,
) {
    if pending_frames.is_empty() {
        return;
    }
    results.clear();
    tx_sink.flush_staged_frames(results);
    for (result_index, frame_index) in pending_frames.drain(..).enumerate() {
        let result = results.get(result_index).cloned().unwrap_or_else(|| {
            Err(VirtioNetworkTxPacketSinkError::new(
                "virtio-net staged sink omitted a committed frame result",
            ))
        });
        dispatch.record_deferred_sink_result(frame_index, result);
    }
    results.clear();
}

#[derive(Debug)]
pub enum VirtioNetworkTxQueueBuildError {
    QueueNotReady,
    AvailableRing { source: VirtqueueAvailableRingError },
    UsedRing { source: VirtqueueUsedRingError },
}

impl fmt::Display for VirtioNetworkTxQueueBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("virtio-net TX queue is not ready"),
            Self::AvailableRing { source } => {
                write!(f, "failed to build virtio-net TX available ring: {source}")
            }
            Self::UsedRing { source } => {
                write!(f, "failed to build virtio-net TX used ring: {source}")
            }
        }
    }
}

impl std::error::Error for VirtioNetworkTxQueueBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source } => Some(source),
            Self::QueueNotReady => None,
        }
    }
}

#[derive(Debug)]
pub struct VirtioNetworkTxQueueDispatch {
    processed_frames: usize,
    successful_frames: usize,
    parse_failures: usize,
    packet_prepare_failures: usize,
    sink_successful_frames: usize,
    sink_failures: usize,
    sink_successful_bytes: u64,
    rate_limiter_throttled_frames: usize,
    rate_limiter_retry_after: Option<Duration>,
    remaining_requests: u64,
    frames: Vec<VirtioNetworkTxFrame>,
    first_parse_failure: Option<VirtioNetworkTxFrameParseError>,
    first_packet_prepare_failure: Option<VirtioNetworkTxPacketPrepareError>,
    first_sink_failure: Option<VirtioNetworkTxPacketSinkError>,
    needs_queue_interrupt: bool,
    backend_metrics: VirtioNetworkBackendMetrics,
}

impl VirtioNetworkTxQueueDispatch {
    fn with_capacity(queue_size: u16) -> Result<Self, VirtioNetworkTxQueueDispatchError> {
        let mut frames = Vec::new();
        frames
            .try_reserve_exact(usize::from(queue_size))
            .map_err(
                |source| VirtioNetworkTxQueueDispatchError::FrameMetadataAllocation { source },
            )?;

        Ok(Self {
            processed_frames: 0,
            successful_frames: 0,
            parse_failures: 0,
            packet_prepare_failures: 0,
            sink_successful_frames: 0,
            sink_failures: 0,
            sink_successful_bytes: 0,
            rate_limiter_throttled_frames: 0,
            rate_limiter_retry_after: None,
            remaining_requests: 0,
            frames,
            first_parse_failure: None,
            first_packet_prepare_failure: None,
            first_sink_failure: None,
            needs_queue_interrupt: false,
            backend_metrics: VirtioNetworkBackendMetrics::default(),
        })
    }

    pub const fn processed_frames(&self) -> usize {
        self.processed_frames
    }

    pub const fn successful_frames(&self) -> usize {
        self.successful_frames
    }

    pub const fn parse_failures(&self) -> usize {
        self.parse_failures
    }

    pub const fn packet_prepare_failures(&self) -> usize {
        self.packet_prepare_failures
    }

    pub const fn malformed_frames(&self) -> usize {
        self.parse_failures
            .saturating_add(self.packet_prepare_failures)
    }

    pub const fn sink_successful_frames(&self) -> usize {
        self.sink_successful_frames
    }

    pub const fn sink_failures(&self) -> usize {
        self.sink_failures
    }

    pub const fn sink_successful_bytes(&self) -> u64 {
        self.sink_successful_bytes
    }

    pub const fn rate_limiter_throttled_frames(&self) -> usize {
        self.rate_limiter_throttled_frames
    }

    pub const fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
    }

    pub const fn remaining_requests(&self) -> u64 {
        self.remaining_requests
    }

    pub const fn first_parse_failure(&self) -> Option<&VirtioNetworkTxFrameParseError> {
        self.first_parse_failure.as_ref()
    }

    pub const fn first_packet_prepare_failure(&self) -> Option<&VirtioNetworkTxPacketPrepareError> {
        self.first_packet_prepare_failure.as_ref()
    }

    pub const fn first_sink_failure(&self) -> Option<&VirtioNetworkTxPacketSinkError> {
        self.first_sink_failure.as_ref()
    }

    pub fn frames(&self) -> &[VirtioNetworkTxFrame] {
        &self.frames
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.needs_queue_interrupt
    }

    pub const fn backend_metrics(&self) -> VirtioNetworkBackendMetrics {
        self.backend_metrics
    }

    fn record_backend_metrics(&mut self, metrics: VirtioNetworkBackendMetrics) {
        self.backend_metrics = self.backend_metrics.merged_with(metrics);
    }

    fn record(
        &mut self,
        outcome: VirtioNetworkTxQueueDispatchOutcome,
        publication: VirtqueueUsedRingPublication,
    ) {
        self.processed_frames += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        match outcome {
            VirtioNetworkTxQueueDispatchOutcome::Ok { frame, sink_error } => {
                self.successful_frames += 1;
                match sink_error {
                    Some(source) => {
                        self.sink_failures += 1;
                        if self.first_sink_failure.is_none() {
                            self.first_sink_failure = Some(source);
                        }
                    }
                    None => {
                        self.sink_successful_frames += 1;
                        self.sink_successful_bytes =
                            self.sink_successful_bytes.saturating_add(frame.frame_len());
                    }
                }
                self.frames.push(frame);
            }
            VirtioNetworkTxQueueDispatchOutcome::ParseError(source) => {
                self.parse_failures += 1;
                if self.first_parse_failure.is_none() {
                    self.first_parse_failure = Some(source);
                }
            }
            VirtioNetworkTxQueueDispatchOutcome::PacketPrepareError(source) => {
                self.packet_prepare_failures += 1;
                if self.first_packet_prepare_failure.is_none() {
                    self.first_packet_prepare_failure = Some(source);
                }
            }
        }
    }

    fn record_deferred(
        &mut self,
        frame: VirtioNetworkTxFrame,
        publication: VirtqueueUsedRingPublication,
    ) -> usize {
        self.processed_frames += 1;
        self.successful_frames += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        let frame_index = self.frames.len();
        self.frames.push(frame);
        frame_index
    }

    fn record_deferred_sink_result(
        &mut self,
        frame_index: usize,
        result: Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError>,
    ) {
        match result {
            Ok(_) => {
                self.sink_successful_frames += 1;
                if let Some(frame) = self.frames.get(frame_index) {
                    self.sink_successful_bytes =
                        self.sink_successful_bytes.saturating_add(frame.frame_len());
                }
            }
            Err(source) => {
                self.sink_failures += 1;
                if self.first_sink_failure.is_none() {
                    self.first_sink_failure = Some(source);
                }
            }
        }
    }

    fn record_rate_limited_frame(&mut self, retry_after: Duration) {
        self.rate_limiter_throttled_frames += 1;
        self.rate_limiter_retry_after = Some(match self.rate_limiter_retry_after {
            Some(existing) => existing.min(retry_after),
            None => retry_after,
        });
    }

    fn record_remaining_requests(&mut self, remaining_requests: u16) {
        self.remaining_requests = self
            .remaining_requests
            .saturating_add(u64::from(remaining_requests));
    }
}

#[derive(Debug)]
enum VirtioNetworkTxQueueDispatchOutcome {
    Ok {
        frame: VirtioNetworkTxFrame,
        sink_error: Option<VirtioNetworkTxPacketSinkError>,
    },
    ParseError(VirtioNetworkTxFrameParseError),
    PacketPrepareError(VirtioNetworkTxPacketPrepareError),
}

#[derive(Debug)]
enum VirtioNetworkTxFramePreparation {
    Ready {
        frame: VirtioNetworkTxFrame,
        packet: VirtioNetworkPacketPlan,
    },
    ParseError(VirtioNetworkTxFrameParseError),
    PacketError(VirtioNetworkTxPacketPrepareError),
}

impl VirtioNetworkTxFramePreparation {
    fn into_ready(
        self,
    ) -> Result<(VirtioNetworkTxFrame, VirtioNetworkPacketPlan), VirtioNetworkTxQueueDispatchOutcome>
    {
        match self {
            Self::Ready { frame, packet } => Ok((frame, packet)),
            Self::ParseError(source) => {
                Err(VirtioNetworkTxQueueDispatchOutcome::ParseError(source))
            }
            Self::PacketError(source) => Err(
                VirtioNetworkTxQueueDispatchOutcome::PacketPrepareError(source),
            ),
        }
    }
}

#[derive(Debug)]
pub enum VirtioNetworkTxQueueDispatchError {
    FrameMetadataAllocation {
        source: TryReserveError,
    },
    AvailableRing {
        completed_dispatch: Box<VirtioNetworkTxQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain {
        completed_dispatch: Box<VirtioNetworkTxQueueDispatch>,
    },
    UsedRing {
        completed_dispatch: Box<VirtioNetworkTxQueueDispatch>,
        descriptor_head: u16,
        bytes_written_to_guest: u32,
        source: VirtqueueUsedRingError,
    },
}

impl VirtioNetworkTxQueueDispatchError {
    pub const fn completed_dispatch(&self) -> Option<&VirtioNetworkTxQueueDispatch> {
        match self {
            Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            } => Some(completed_dispatch),
            Self::FrameMetadataAllocation { .. } => None,
        }
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.completed_dispatch()
            .and_then(VirtioNetworkTxQueueDispatch::rate_limiter_retry_after)
    }

    fn record_backend_metrics(&mut self, metrics: VirtioNetworkBackendMetrics) {
        match self {
            Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            } => completed_dispatch.record_backend_metrics(metrics),
            Self::FrameMetadataAllocation { .. } => {}
        }
    }
}

impl fmt::Display for VirtioNetworkTxQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameMetadataAllocation { source } => {
                write!(
                    f,
                    "failed to reserve virtio-net TX frame metadata: {source}"
                )
            }
            Self::AvailableRing { source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-net TX available descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain { .. } => {
                f.write_str("virtio-net TX queue produced an empty descriptor chain")
            }
            Self::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to publish virtio-net TX used descriptor head {descriptor_head} with length {bytes_written_to_guest}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioNetworkTxQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::FrameMetadataAllocation { source } => Some(source),
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::EmptyDescriptorChain { .. } => None,
        }
    }
}

fn descriptor_chain_head(chain: &VirtqueueDescriptorChain) -> Option<u16> {
    if chain.is_empty() {
        None
    } else {
        Some(chain.head_index())
    }
}

impl<C: VirtioMmioDeviceConfigHandler> VirtioMmioRegisterHandler<C, VirtioNetworkDevice> {
    pub fn update_network_rate_limiters(&mut self, update: &NetworkInterfaceUpdate) {
        self.activation_handler_mut().update_rate_limiters(update);
    }

    pub fn has_pending_network_queue_work(&self) -> bool {
        self.has_pending_queue_notifications()
            || self
                .activation_handler()
                .has_pending_rate_limited_queue_work()
    }

    pub fn dispatch_network_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioNetworkDeviceNotificationDispatch, VirtioNetworkDeviceNotificationError> {
        let mut sink = NoopVirtioNetworkTxPacketSink;
        self.dispatch_network_queue_notifications_with_tx_sink(memory, &mut sink)
    }

    pub fn dispatch_network_queue_notifications_with_tx_sink(
        &mut self,
        memory: &mut GuestMemory,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
    ) -> Result<VirtioNetworkDeviceNotificationDispatch, VirtioNetworkDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        let dispatch = self
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_tx_sink(
                memory,
                drained_notifications,
                tx_sink,
            );
        let (rx_interrupt, tx_interrupt) = network_queue_interrupts(&dispatch);
        if rx_interrupt && let Ok(queue_index) = u16::try_from(VIRTIO_NET_RX_QUEUE_INDEX) {
            self.mark_queue_interrupt_pending(queue_index);
        }
        if tx_interrupt && let Ok(queue_index) = u16::try_from(VIRTIO_NET_TX_QUEUE_INDEX) {
            self.mark_queue_interrupt_pending(queue_index);
        }

        dispatch
    }

    pub fn dispatch_network_queue_notifications_with_packet_io(
        &mut self,
        memory: &mut GuestMemory,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
        rx_source: &mut (impl VirtioNetworkRxPacketSource + ?Sized),
    ) -> Result<VirtioNetworkDeviceNotificationDispatch, VirtioNetworkDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        let dispatch = self
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io(
                memory,
                drained_notifications,
                tx_sink,
                rx_source,
            );
        let (rx_interrupt, tx_interrupt) = network_queue_interrupts(&dispatch);
        if rx_interrupt && let Ok(queue_index) = u16::try_from(VIRTIO_NET_RX_QUEUE_INDEX) {
            self.mark_queue_interrupt_pending(queue_index);
        }
        if tx_interrupt && let Ok(queue_index) = u16::try_from(VIRTIO_NET_TX_QUEUE_INDEX) {
            self.mark_queue_interrupt_pending(queue_index);
        }

        dispatch
    }
}

impl VirtioNetworkMmioHandler {
    pub fn capture_network_state_at(
        &self,
        config: &NetworkInterfaceConfig,
        profile: NetworkDeviceProfile,
        memory: &GuestMemory,
        provider_cached_rx_len: Option<usize>,
        now: Instant,
    ) -> Result<
        (
            VirtioNetworkMmioCaptureState,
            VirtioNetworkCaptureValidation,
        ),
        VirtioNetworkDeviceCaptureError,
    > {
        let (device, validation) =
            self.activation_handler()
                .capture_state_at(VirtioNetworkDeviceCaptureInput {
                    config,
                    profile,
                    config_space: self.device_config_handler(),
                    device_registers: self.device_registers(),
                    queue_registers: self.queue_registers(),
                    transport_activated: self.is_device_activated(),
                    memory,
                    provider_cached_rx_len,
                    now,
                })?;
        Ok((
            VirtioNetworkMmioCaptureState {
                device,
                transport: self.transport_state(),
            },
            validation,
        ))
    }

    pub fn attach_network_metrics(&mut self, metrics: SharedNetworkInterfaceMetrics) {
        self.device_config_handler_mut()
            .attach_metrics(metrics.clone());
        self.activation_handler_mut().attach_metrics(metrics);
    }

    pub fn attach_network_metrics_with_aggregate(
        &mut self,
        interface: SharedNetworkInterfaceMetrics,
        aggregate: SharedNetworkInterfaceMetrics,
    ) {
        self.device_config_handler_mut()
            .attach_metrics_with_aggregate(interface.clone(), aggregate.clone());
        self.activation_handler_mut()
            .attach_metrics_with_aggregate(interface, aggregate);
    }
}

pub fn attach_network_metrics_to_mmio_handler(
    dispatcher: &mut MmioDispatcher,
    region_id: MmioRegionId,
    interface: SharedNetworkInterfaceMetrics,
    aggregate: SharedNetworkInterfaceMetrics,
) -> Result<(), MmioHandlerLookupError> {
    dispatcher
        .handler_mut::<VirtioNetworkMmioHandler>(region_id)?
        .attach_network_metrics_with_aggregate(interface, aggregate);
    Ok(())
}

/// Returns the guest-acknowledged feature bitmap for one MMIO network device.
///
/// Signed transport conformance tests use this narrow diagnostic instead of
/// exposing the dispatcher-owned concrete handler.
pub fn network_mmio_driver_features(
    mmio_dispatcher: &mut MmioDispatcher,
    region_id: MmioRegionId,
) -> Result<u64, MmioHandlerLookupError> {
    Ok(mmio_dispatcher
        .handler_mut::<VirtioNetworkMmioHandler>(region_id)?
        .device_registers()
        .driver_features())
}

impl VirtioPciEndpoint<VirtioNetworkConfigSpace, VirtioNetworkDevice> {
    pub fn capture_network_state_at(
        &self,
        config: &NetworkInterfaceConfig,
        profile: NetworkDeviceProfile,
        memory: &GuestMemory,
        provider_cached_rx_len: Option<usize>,
        now: Instant,
    ) -> Result<
        (VirtioNetworkPciCaptureState, VirtioNetworkCaptureValidation),
        VirtioNetworkPciCaptureError,
    > {
        let (device, transport) = self
            .capture_transport_with(|registers, queues, config_space, device, activated| {
                device.capture_state_at(VirtioNetworkDeviceCaptureInput {
                    config,
                    profile,
                    config_space,
                    device_registers: registers,
                    queue_registers: queues,
                    transport_activated: activated,
                    memory,
                    provider_cached_rx_len,
                    now,
                })
            })
            .map_err(VirtioNetworkPciCaptureError::Endpoint)?;
        let (device, validation) = device.map_err(VirtioNetworkPciCaptureError::Device)?;
        Ok((
            VirtioNetworkPciCaptureState { device, transport },
            validation,
        ))
    }

    pub fn update_network_rate_limiters(
        &self,
        update: &NetworkInterfaceUpdate,
    ) -> Result<(), VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| core.activation.update_rate_limiters(update))
    }

    pub fn has_pending_network_queue_work(&self) -> Result<bool, VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| {
            !core
                .queue_notifications
                .pending_queue_notifications()
                .is_empty()
                || core.activation.has_pending_rate_limited_queue_work()
        })
    }

    pub fn dispatch_network_queue_notifications_with_packet_io(
        &self,
        memory: &mut GuestMemory,
        tx_sink: &mut (impl VirtioNetworkTxPacketSink + ?Sized),
        rx_source: &mut (impl VirtioNetworkRxPacketSource + ?Sized),
    ) -> Result<
        VirtioNetworkDeviceNotificationDispatch,
        VirtioPciDeviceOperationError<
            VirtioNetworkDeviceNotificationError,
            VirtioNetworkDeviceNotificationDispatch,
        >,
    > {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let dispatch = work
            .with_core_mut(|core| {
                let drained_notifications =
                    core.queue_notifications.take_pending_queue_notifications();
                let dispatch = core
                    .activation
                    .dispatch_drained_queue_notifications_with_packet_io(
                        memory,
                        drained_notifications,
                        tx_sink,
                        rx_source,
                    );
                let (rx_interrupt, tx_interrupt) = network_queue_interrupts(&dispatch);
                if rx_interrupt {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue {
                        queue_index: VIRTIO_NET_RX_QUEUE_INDEX as u16,
                    });
                }
                if tx_interrupt {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue {
                        queue_index: VIRTIO_NET_TX_QUEUE_INDEX as u16,
                    });
                }
                dispatch
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let endpoint = work.drain_interrupt_intents();
        VirtioPciDeviceOperationError::combine(dispatch, endpoint)
    }

    pub fn dispatch_network_queue_notifications(
        &self,
        memory: &mut GuestMemory,
    ) -> Result<
        VirtioNetworkDeviceNotificationDispatch,
        VirtioPciDeviceOperationError<
            VirtioNetworkDeviceNotificationError,
            VirtioNetworkDeviceNotificationDispatch,
        >,
    > {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let dispatch = work
            .with_core_mut(|core| {
                let drained_notifications =
                    core.queue_notifications.take_pending_queue_notifications();
                let mut sink = NoopVirtioNetworkTxPacketSink;
                let dispatch = core
                    .activation
                    .dispatch_drained_queue_notifications_with_tx_sink(
                        memory,
                        drained_notifications,
                        &mut sink,
                    );
                let (rx_interrupt, tx_interrupt) = network_queue_interrupts(&dispatch);
                if rx_interrupt {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue {
                        queue_index: VIRTIO_NET_RX_QUEUE_INDEX as u16,
                    });
                }
                if tx_interrupt {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue {
                        queue_index: VIRTIO_NET_TX_QUEUE_INDEX as u16,
                    });
                }
                dispatch
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let endpoint = work.drain_interrupt_intents();
        VirtioPciDeviceOperationError::combine(dispatch, endpoint)
    }
}

fn network_queue_interrupts(
    dispatch: &Result<
        VirtioNetworkDeviceNotificationDispatch,
        VirtioNetworkDeviceNotificationError,
    >,
) -> (bool, bool) {
    match dispatch {
        Ok(dispatch) => (
            dispatch
                .rx_queue_dispatch()
                .is_some_and(VirtioNetworkRxQueueDispatch::needs_queue_interrupt)
                || dispatch
                    .post_tx_rx_queue_dispatch()
                    .is_some_and(VirtioNetworkRxQueueDispatch::needs_queue_interrupt),
            dispatch
                .tx_queue_dispatch()
                .is_some_and(VirtioNetworkTxQueueDispatch::needs_queue_interrupt),
        ),
        Err(error) => (
            error
                .completed_initial_rx_dispatch()
                .is_some_and(VirtioNetworkRxQueueDispatch::needs_queue_interrupt)
                || error
                    .completed_rx_dispatch()
                    .is_some_and(VirtioNetworkRxQueueDispatch::needs_queue_interrupt),
            error
                .completed_tx_dispatch()
                .is_some_and(VirtioNetworkTxQueueDispatch::needs_queue_interrupt),
        ),
    }
}

impl VirtioMmioDeviceActivationHandler for VirtioNetworkDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        let result = self.activate_network(activation);
        if result.is_err()
            && let Some(metrics) = &self.metrics
        {
            metrics.record_activation_failure();
        }
        result.map_err(Into::into)
    }

    fn reset(&mut self) {
        VirtioNetworkDevice::reset(self);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct NetworkDeviceProfile {
    guest_mac: Option<GuestMacAddress>,
    mtu: Option<u16>,
    packet_envelope: VirtioNetworkPacketEnvelope,
    feature_capabilities: VirtioNetworkFeatureCapabilities,
}

impl NetworkDeviceProfile {
    pub const fn new(guest_mac: Option<GuestMacAddress>, mtu: Option<u16>) -> Self {
        Self {
            guest_mac,
            mtu,
            packet_envelope: VirtioNetworkPacketEnvelope::RawEthernet,
            feature_capabilities: VirtioNetworkFeatureCapabilities::complete_software(),
        }
    }

    pub const fn from_config(config: &NetworkInterfaceConfig) -> Self {
        Self::new(config.guest_mac(), config.mtu())
    }

    pub const fn guest_mac(&self) -> Option<GuestMacAddress> {
        self.guest_mac
    }

    pub const fn mtu(&self) -> Option<u16> {
        self.mtu
    }

    pub const fn packet_envelope(&self) -> VirtioNetworkPacketEnvelope {
        self.packet_envelope
    }

    pub const fn feature_capabilities(&self) -> VirtioNetworkFeatureCapabilities {
        self.feature_capabilities
    }

    pub const fn with_packet_envelope(
        mut self,
        packet_envelope: VirtioNetworkPacketEnvelope,
    ) -> Self {
        self.packet_envelope = packet_envelope;
        self
    }

    pub const fn with_feature_capabilities(
        mut self,
        feature_capabilities: VirtioNetworkFeatureCapabilities,
    ) -> Self {
        self.feature_capabilities = feature_capabilities;
        self
    }
}

impl fmt::Debug for NetworkDeviceProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkDeviceProfile")
            .field("guest_mac", &self.guest_mac.map(|_| "<configured>"))
            .field("mtu", &self.mtu.map(|_| "<configured>"))
            .field("packet_envelope", &self.packet_envelope)
            .field("feature_capabilities", &self.feature_capabilities)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct PreparedNetworkDevice {
    iface_id: String,
    host_dev_name: String,
    config_space: VirtioNetworkConfigSpace,
    device: VirtioNetworkDevice,
}

impl PreparedNetworkDevice {
    pub fn from_config(config: &NetworkInterfaceConfig) -> Self {
        Self::from_config_with_profile(config, NetworkDeviceProfile::from_config(config))
    }

    pub fn from_config_with_profile(
        config: &NetworkInterfaceConfig,
        profile: NetworkDeviceProfile,
    ) -> Self {
        Self {
            iface_id: config.iface_id().to_string(),
            host_dev_name: config.host_dev_name().to_string(),
            config_space: VirtioNetworkConfigSpace::with_feature_capabilities(
                profile.guest_mac(),
                profile.mtu(),
                profile.feature_capabilities(),
            ),
            device: VirtioNetworkDevice::with_rate_limiters(
                config.rx_rate_limiter(),
                config.tx_rate_limiter(),
            ),
        }
    }

    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    pub fn host_dev_name(&self) -> &str {
        &self.host_dev_name
    }

    pub fn config_space(&self) -> VirtioNetworkConfigSpace {
        self.config_space.clone()
    }

    pub const fn device(&self) -> &VirtioNetworkDevice {
        &self.device
    }

    pub fn attach_metrics(&mut self, metrics: SharedNetworkInterfaceMetrics) {
        self.config_space.attach_metrics(metrics.clone());
        self.device.attach_metrics(metrics);
    }

    pub fn attach_metrics_with_aggregate(
        &mut self,
        interface: SharedNetworkInterfaceMetrics,
        aggregate: SharedNetworkInterfaceMetrics,
    ) {
        self.config_space
            .attach_metrics_with_aggregate(interface.clone(), aggregate.clone());
        self.device
            .attach_metrics_with_aggregate(interface, aggregate);
    }

    pub fn into_parts(
        self,
    ) -> (
        String,
        String,
        VirtioNetworkConfigSpace,
        VirtioNetworkDevice,
    ) {
        (
            self.iface_id,
            self.host_dev_name,
            self.config_space,
            self.device,
        )
    }
}

impl fmt::Debug for PreparedNetworkDevice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedNetworkDevice")
            .field("iface_id", &"<redacted>")
            .field("host_dev_name", &"<redacted>")
            .field("config_space", &"<redacted>")
            .field("device", &"<owned>")
            .finish()
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PreparedNetworkDevices {
    devices: Vec<PreparedNetworkDevice>,
}

impl PreparedNetworkDevices {
    pub fn from_configs(
        configs: &NetworkInterfaceConfigs,
    ) -> Result<Self, PreparedNetworkDeviceError> {
        Self::from_config_slice(configs.as_slice())
    }

    pub(crate) fn from_config_slice(
        configs: &[NetworkInterfaceConfig],
    ) -> Result<Self, PreparedNetworkDeviceError> {
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(configs.len())
            .map_err(|source| PreparedNetworkDeviceError::AllocateDevices { source })?;

        for config in configs {
            devices.push(PreparedNetworkDevice::from_config(config));
        }

        Ok(Self { devices })
    }

    pub fn from_config_slice_with_profiles(
        configs: &[NetworkInterfaceConfig],
        mut profiles: BTreeMap<String, NetworkDeviceProfile>,
    ) -> Result<Self, PreparedNetworkDeviceError> {
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(configs.len())
            .map_err(|source| PreparedNetworkDeviceError::AllocateDevices { source })?;

        for config in configs {
            let profile = profiles
                .remove(config.iface_id())
                .ok_or(PreparedNetworkDeviceError::MissingProfile)?;
            devices.push(PreparedNetworkDevice::from_config_with_profile(
                config, profile,
            ));
        }
        if !profiles.is_empty() {
            return Err(PreparedNetworkDeviceError::UnexpectedProfile);
        }

        Ok(Self { devices })
    }

    pub fn as_slice(&self) -> &[PreparedNetworkDevice] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn into_vec(self) -> Vec<PreparedNetworkDevice> {
        self.devices
    }

    pub fn register_mmio(
        self,
        layout: NetworkMmioLayout,
    ) -> Result<NetworkMmioDevices, NetworkMmioRegistrationError> {
        NetworkMmioDevices::from_prepared(self, layout)
    }

    pub fn register_mmio_with_dispatcher(
        self,
        layout: NetworkMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<NetworkMmioDevices, NetworkMmioRegistrationError> {
        NetworkMmioDevices::from_prepared_with_dispatcher(self, layout, dispatcher)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkMmioLayout {
    base_address: GuestAddress,
    base_region_id: MmioRegionId,
    address_stride: u64,
    region_id_stride: u64,
}

impl NetworkMmioLayout {
    pub const fn new(base_address: GuestAddress, base_region_id: MmioRegionId) -> Self {
        Self {
            base_address,
            base_region_id,
            address_stride: VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            region_id_stride: 1,
        }
    }

    pub const fn base_address(self) -> GuestAddress {
        self.base_address
    }

    pub const fn base_region_id(self) -> MmioRegionId {
        self.base_region_id
    }

    pub const fn address_stride(self) -> u64 {
        self.address_stride
    }

    pub const fn region_id_stride(self) -> u64 {
        self.region_id_stride
    }

    pub const fn with_address_stride(mut self, address_stride: u64) -> Self {
        self.address_stride = address_stride;
        self
    }

    pub const fn with_region_id_stride(mut self, region_id_stride: u64) -> Self {
        self.region_id_stride = region_id_stride;
        self
    }

    fn validate(self) -> Result<(), NetworkMmioRegistrationError> {
        if self.address_stride < VIRTIO_MMIO_DEVICE_WINDOW_SIZE {
            return Err(NetworkMmioRegistrationError::AddressStrideTooSmall {
                stride: self.address_stride,
                minimum: VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            });
        }

        if self.region_id_stride == 0 {
            return Err(NetworkMmioRegistrationError::DuplicateRegionIdStride {
                region_id: self.base_region_id,
            });
        }

        Ok(())
    }

    fn placement(
        self,
        index: usize,
    ) -> Result<NetworkMmioDevicePlacement, NetworkMmioRegistrationError> {
        let device_index = u64::try_from(index)
            .map_err(|_| NetworkMmioRegistrationError::DeviceIndexTooLarge { index })?;
        let address_offset = device_index.checked_mul(self.address_stride).ok_or(
            NetworkMmioRegistrationError::AddressOffsetOverflow {
                device_index,
                stride: self.address_stride,
            },
        )?;
        let address = self.base_address.checked_add(address_offset).ok_or(
            NetworkMmioRegistrationError::AddressOverflow {
                base_address: self.base_address,
                offset: address_offset,
            },
        )?;
        let region_id_offset = device_index.checked_mul(self.region_id_stride).ok_or(
            NetworkMmioRegistrationError::RegionIdOffsetOverflow {
                device_index,
                stride: self.region_id_stride,
            },
        )?;
        let region_id = self
            .base_region_id
            .raw_value()
            .checked_add(region_id_offset)
            .map(MmioRegionId::new)
            .ok_or(NetworkMmioRegistrationError::RegionIdOverflow {
                base_region_id: self.base_region_id,
                offset: region_id_offset,
            })?;
        let region = MmioRegion::new(region_id, address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE).map_err(
            |source| NetworkMmioRegistrationError::InvalidRegion {
                region_id,
                address,
                source,
            },
        )?;

        Ok(NetworkMmioDevicePlacement {
            index,
            address,
            region_id,
            region,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NetworkMmioDevicePlacement {
    index: usize,
    address: GuestAddress,
    region_id: MmioRegionId,
    region: MmioRegion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkMmioDeviceRegistration {
    index: usize,
    iface_id: String,
    host_dev_name: String,
    region: MmioRegion,
}

impl NetworkMmioDeviceRegistration {
    pub const fn index(&self) -> usize {
        self.index
    }

    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    pub fn host_dev_name(&self) -> &str {
        &self.host_dev_name
    }

    pub const fn region(&self) -> MmioRegion {
        self.region
    }

    pub const fn region_id(&self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn address(&self) -> GuestAddress {
        self.region.range().start()
    }
}

#[derive(Debug)]
pub struct NetworkMmioDevices {
    dispatcher: MmioDispatcher,
    registrations: Vec<NetworkMmioDeviceRegistration>,
}

impl NetworkMmioDevices {
    pub fn from_prepared(
        prepared: PreparedNetworkDevices,
        layout: NetworkMmioLayout,
    ) -> Result<Self, NetworkMmioRegistrationError> {
        Self::from_prepared_with_dispatcher(prepared, layout, MmioDispatcher::new())
    }

    pub fn from_prepared_with_dispatcher(
        prepared: PreparedNetworkDevices,
        layout: NetworkMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<Self, NetworkMmioRegistrationError> {
        layout.validate()?;

        let prepared_devices = prepared.into_vec();
        let mut registrations = Vec::new();
        registrations
            .try_reserve_exact(prepared_devices.len())
            .map_err(|source| NetworkMmioRegistrationError::AllocateRegistrations { source })?;
        let mut placements = Vec::new();
        placements
            .try_reserve_exact(prepared_devices.len())
            .map_err(|source| NetworkMmioRegistrationError::AllocatePlacements { source })?;
        for index in 0..prepared_devices.len() {
            placements.push(layout.placement(index)?);
        }

        let mut dispatcher = dispatcher;
        for (prepared_device, placement) in prepared_devices.into_iter().zip(placements) {
            let (iface_id, host_dev_name, config_space, device) = prepared_device.into_parts();
            let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
                VIRTIO_NET_DEVICE_ID,
                config_space.available_features(),
                &VIRTIO_NET_QUEUE_SIZES,
                config_space,
                device,
            )
            .map_err(|source| NetworkMmioRegistrationError::BuildHandler {
                iface_id: iface_id.clone(),
                region_id: placement.region_id,
                source,
            })?;
            let region = dispatcher
                .insert_region(
                    placement.region_id,
                    placement.address,
                    VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
                )
                .map_err(|source| NetworkMmioRegistrationError::InsertRegion {
                    iface_id: iface_id.clone(),
                    region_id: placement.region_id,
                    address: placement.address,
                    source,
                })?;
            dispatcher
                .register_handler(placement.region_id, handler)
                .map_err(|source| NetworkMmioRegistrationError::RegisterHandler {
                    iface_id: iface_id.clone(),
                    region_id: placement.region_id,
                    source,
                })?;
            debug_assert_eq!(region, placement.region);
            registrations.push(NetworkMmioDeviceRegistration {
                index: placement.index,
                iface_id,
                host_dev_name,
                region,
            });
        }

        Ok(Self {
            dispatcher,
            registrations,
        })
    }

    pub fn dispatcher(&self) -> &MmioDispatcher {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut MmioDispatcher {
        &mut self.dispatcher
    }

    pub fn registrations(&self) -> &[NetworkMmioDeviceRegistration] {
        &self.registrations
    }

    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    pub fn into_parts(self) -> (MmioDispatcher, Vec<NetworkMmioDeviceRegistration>) {
        (self.dispatcher, self.registrations)
    }
}

#[derive(Debug)]
pub enum NetworkMmioRegistrationError {
    AddressStrideTooSmall {
        stride: u64,
        minimum: u64,
    },
    DuplicateRegionIdStride {
        region_id: MmioRegionId,
    },
    DeviceIndexTooLarge {
        index: usize,
    },
    AddressOffsetOverflow {
        device_index: u64,
        stride: u64,
    },
    AddressOverflow {
        base_address: GuestAddress,
        offset: u64,
    },
    RegionIdOffsetOverflow {
        device_index: u64,
        stride: u64,
    },
    RegionIdOverflow {
        base_region_id: MmioRegionId,
        offset: u64,
    },
    AllocateRegistrations {
        source: TryReserveError,
    },
    AllocatePlacements {
        source: TryReserveError,
    },
    InvalidRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: GuestMemoryError,
    },
    BuildHandler {
        iface_id: String,
        region_id: MmioRegionId,
        source: VirtioMmioRegisterHandlerError,
    },
    InsertRegion {
        iface_id: String,
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        iface_id: String,
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for NetworkMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AddressStrideTooSmall { stride, minimum } => {
                write!(
                    f,
                    "network MMIO address stride {stride} is smaller than the required device window size {minimum}"
                )
            }
            Self::DuplicateRegionIdStride { region_id } => {
                write!(
                    f,
                    "network MMIO region id stride cannot be 0 because it would duplicate region id={region_id}"
                )
            }
            Self::DeviceIndexTooLarge { index } => {
                write!(f, "network MMIO device index {index} does not fit in u64")
            }
            Self::AddressOffsetOverflow {
                device_index,
                stride,
            } => {
                write!(
                    f,
                    "network MMIO address offset overflows for device index {device_index} with stride {stride}"
                )
            }
            Self::AddressOverflow {
                base_address,
                offset,
            } => {
                write!(
                    f,
                    "network MMIO address overflows from base {base_address} with offset {offset}"
                )
            }
            Self::RegionIdOffsetOverflow {
                device_index,
                stride,
            } => {
                write!(
                    f,
                    "network MMIO region id offset overflows for device index {device_index} with stride {stride}"
                )
            }
            Self::RegionIdOverflow {
                base_region_id,
                offset,
            } => {
                write!(
                    f,
                    "network MMIO region id overflows from base id={base_region_id} with offset {offset}"
                )
            }
            Self::AllocateRegistrations { source } => {
                write!(f, "failed to allocate network MMIO registrations: {source}")
            }
            Self::AllocatePlacements { source } => {
                write!(f, "failed to allocate network MMIO placements: {source}")
            }
            Self::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "invalid network MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::BuildHandler {
                iface_id,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to build network MMIO handler for interface {iface_id} region id={region_id}: {source}"
                )
            }
            Self::InsertRegion {
                iface_id,
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "failed to insert network MMIO region for interface {iface_id} region id={region_id} at {address}: {source}"
                )
            }
            Self::RegisterHandler {
                iface_id,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to register network MMIO handler for interface {iface_id} region id={region_id}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for NetworkMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateRegistrations { source } => Some(source),
            Self::AllocatePlacements { source } => Some(source),
            Self::InvalidRegion { source, .. } => Some(source),
            Self::BuildHandler { source, .. } => Some(source),
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
            Self::AddressStrideTooSmall { .. }
            | Self::DuplicateRegionIdStride { .. }
            | Self::DeviceIndexTooLarge { .. }
            | Self::AddressOffsetOverflow { .. }
            | Self::AddressOverflow { .. }
            | Self::RegionIdOffsetOverflow { .. }
            | Self::RegionIdOverflow { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum PreparedNetworkDeviceError {
    AllocateDevices { source: TryReserveError },
    MissingProfile,
    UnexpectedProfile,
}

impl fmt::Display for PreparedNetworkDeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocateDevices { source } => {
                write!(f, "failed to allocate prepared network devices: {source}")
            }
            Self::MissingProfile => {
                f.write_str("a configured network interface has no realized device profile")
            }
            Self::UnexpectedProfile => {
                f.write_str("a realized network device profile has no configured interface")
            }
        }
    }
}

impl std::error::Error for PreparedNetworkDeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateDevices { source } => Some(source),
            Self::MissingProfile | Self::UnexpectedProfile => None,
        }
    }
}

#[derive(Debug)]
pub struct VirtioNetworkDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
    rx_queue_dispatch: Option<VirtioNetworkRxQueueDispatch>,
    tx_queue_dispatch: Option<VirtioNetworkTxQueueDispatch>,
    post_tx_rx_queue_dispatch: Option<VirtioNetworkRxQueueDispatch>,
    rx_rate_limiter_event: bool,
    tx_rate_limiter_event: bool,
}

impl VirtioNetworkDeviceNotificationDispatch {
    const fn new(
        drained_notifications: Vec<usize>,
        rx_queue_dispatch: Option<VirtioNetworkRxQueueDispatch>,
        tx_queue_dispatch: Option<VirtioNetworkTxQueueDispatch>,
        post_tx_rx_queue_dispatch: Option<VirtioNetworkRxQueueDispatch>,
    ) -> Self {
        Self {
            drained_notifications,
            rx_queue_dispatch,
            tx_queue_dispatch,
            post_tx_rx_queue_dispatch,
            rx_rate_limiter_event: false,
            tx_rate_limiter_event: false,
        }
    }

    const fn with_rate_limiter_events(
        mut self,
        rx_rate_limiter_event: bool,
        tx_rate_limiter_event: bool,
    ) -> Self {
        self.rx_rate_limiter_event = rx_rate_limiter_event;
        self.tx_rate_limiter_event = tx_rate_limiter_event;
        self
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub const fn tx_queue_dispatch(&self) -> Option<&VirtioNetworkTxQueueDispatch> {
        self.tx_queue_dispatch.as_ref()
    }

    pub const fn rx_queue_dispatch(&self) -> Option<&VirtioNetworkRxQueueDispatch> {
        self.rx_queue_dispatch.as_ref()
    }

    pub const fn post_tx_rx_queue_dispatch(&self) -> Option<&VirtioNetworkRxQueueDispatch> {
        self.post_tx_rx_queue_dispatch.as_ref()
    }

    pub const fn rx_rate_limiter_event(&self) -> bool {
        self.rx_rate_limiter_event
    }

    pub const fn tx_rate_limiter_event(&self) -> bool {
        self.tx_rate_limiter_event
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.tx_queue_dispatch
            .as_ref()
            .is_some_and(VirtioNetworkTxQueueDispatch::needs_queue_interrupt)
            || self
                .rx_queue_dispatch
                .as_ref()
                .is_some_and(VirtioNetworkRxQueueDispatch::needs_queue_interrupt)
            || self
                .post_tx_rx_queue_dispatch
                .as_ref()
                .is_some_and(VirtioNetworkRxQueueDispatch::needs_queue_interrupt)
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        [
            self.rx_queue_dispatch
                .as_ref()
                .and_then(VirtioNetworkRxQueueDispatch::rate_limiter_retry_after),
            self.tx_queue_dispatch
                .as_ref()
                .and_then(VirtioNetworkTxQueueDispatch::rate_limiter_retry_after),
            self.post_tx_rx_queue_dispatch
                .as_ref()
                .and_then(VirtioNetworkRxQueueDispatch::rate_limiter_retry_after),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

#[derive(Debug)]
pub enum VirtioNetworkDeviceNotificationError {
    Inactive {
        drained_notifications: Vec<usize>,
    },
    UnsupportedQueue {
        drained_notifications: Vec<usize>,
        queue_index: usize,
    },
    TxQueueDispatch {
        drained_notifications: Vec<usize>,
        completed_rx_dispatch: Option<Box<VirtioNetworkRxQueueDispatch>>,
        source: VirtioNetworkTxQueueDispatchError,
    },
    RxQueueDispatch {
        drained_notifications: Vec<usize>,
        completed_tx_dispatch: Option<Box<VirtioNetworkTxQueueDispatch>>,
        completed_initial_rx_dispatch: Option<Box<VirtioNetworkRxQueueDispatch>>,
        source: VirtioNetworkRxQueueDispatchError,
    },
}

impl VirtioNetworkDeviceNotificationError {
    pub fn drained_notifications(&self) -> &[usize] {
        match self {
            Self::Inactive {
                drained_notifications,
            }
            | Self::UnsupportedQueue {
                drained_notifications,
                ..
            }
            | Self::TxQueueDispatch {
                drained_notifications,
                ..
            }
            | Self::RxQueueDispatch {
                drained_notifications,
                ..
            } => drained_notifications,
        }
    }

    pub const fn completed_tx_dispatch(&self) -> Option<&VirtioNetworkTxQueueDispatch> {
        match self {
            Self::TxQueueDispatch { source, .. } => source.completed_dispatch(),
            Self::RxQueueDispatch {
                completed_tx_dispatch,
                ..
            } => match completed_tx_dispatch {
                Some(dispatch) => Some(dispatch),
                None => None,
            },
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }

    pub const fn completed_initial_rx_dispatch(&self) -> Option<&VirtioNetworkRxQueueDispatch> {
        match self {
            Self::RxQueueDispatch {
                completed_initial_rx_dispatch,
                ..
            } => match completed_initial_rx_dispatch {
                Some(dispatch) => Some(dispatch),
                None => None,
            },
            Self::Inactive { .. }
            | Self::UnsupportedQueue { .. }
            | Self::TxQueueDispatch { .. } => None,
        }
    }

    pub const fn completed_rx_dispatch(&self) -> Option<&VirtioNetworkRxQueueDispatch> {
        match self {
            Self::RxQueueDispatch { source, .. } => source.completed_dispatch(),
            Self::TxQueueDispatch {
                completed_rx_dispatch,
                ..
            } => match completed_rx_dispatch {
                Some(dispatch) => Some(dispatch),
                None => None,
            },
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        [
            self.completed_tx_dispatch()
                .and_then(VirtioNetworkTxQueueDispatch::rate_limiter_retry_after),
            self.completed_initial_rx_dispatch()
                .and_then(VirtioNetworkRxQueueDispatch::rate_limiter_retry_after),
            self.completed_rx_dispatch()
                .and_then(VirtioNetworkRxQueueDispatch::rate_limiter_retry_after),
        ]
        .into_iter()
        .flatten()
        .min()
    }
}

impl fmt::Display for VirtioNetworkDeviceNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive { .. } => {
                f.write_str("virtio-net queue notification cannot be dispatched before activation")
            }
            Self::UnsupportedQueue { queue_index, .. } => {
                write!(
                    f,
                    "virtio-net queue notification for unsupported queue {queue_index}"
                )
            }
            Self::TxQueueDispatch { source, .. } => {
                write!(f, "failed to dispatch virtio-net TX queue: {source}")
            }
            Self::RxQueueDispatch { source, .. } => {
                write!(f, "failed to dispatch virtio-net RX queue: {source}")
            }
        }
    }
}

impl std::error::Error for VirtioNetworkDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::TxQueueDispatch { source, .. } => Some(source),
            Self::RxQueueDispatch { source, .. } => Some(source),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioNetworkDeviceActivationError {
    AlreadyActive,
    QueueMetadata {
        queue_index: u32,
        source: VirtioMmioQueueRegisterError,
    },
    RxQueueBuild {
        queue_index: u32,
        source: VirtioNetworkRxQueueBuildError,
    },
    TxQueueBuild {
        queue_index: u32,
        source: VirtioNetworkTxQueueBuildError,
    },
    QueueNotReady {
        queue_index: u32,
    },
    QueueSizeNotConfigured {
        queue_index: u32,
    },
}

impl fmt::Display for VirtioNetworkDeviceActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("virtio-net device is already active"),
            Self::QueueMetadata {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to read virtio-net queue {queue_index} activation metadata: {source}"
                )
            }
            Self::RxQueueBuild {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to activate virtio-net RX queue {queue_index}: {source}"
                )
            }
            Self::TxQueueBuild {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to activate virtio-net TX queue {queue_index}: {source}"
                )
            }
            Self::QueueNotReady { queue_index } => {
                write!(f, "virtio-net queue {queue_index} is not ready")
            }
            Self::QueueSizeNotConfigured { queue_index } => {
                write!(f, "virtio-net queue {queue_index} size is not configured")
            }
        }
    }
}

impl std::error::Error for VirtioNetworkDeviceActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueMetadata { source, .. } => Some(source),
            Self::RxQueueBuild { source, .. } => Some(source),
            Self::TxQueueBuild { source, .. } => Some(source),
            Self::AlreadyActive
            | Self::QueueNotReady { .. }
            | Self::QueueSizeNotConfigured { .. } => None,
        }
    }
}

impl From<VirtioNetworkDeviceActivationError> for VirtioMmioDeviceActivationError {
    fn from(source: VirtioNetworkDeviceActivationError) -> Self {
        MmioHandlerError::new(source.to_string()).into()
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

fn read_virtio_network_config_bytes(
    mac: Option<[u8; VIRTIO_NET_CONFIG_MAC_SIZE]>,
    mtu: Option<u16>,
    access: VirtioMmioDeviceConfigAccess,
) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
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

    if let Some(mac) = mac
        && let Some(bytes) = mac.get(offset..end)
    {
        return MmioAccessBytes::new(bytes).map_err(network_config_bytes_error);
    }

    if let Some(mtu) = mtu {
        let mtu_offset = usize::try_from(VIRTIO_NET_CONFIG_MTU_OFFSET).map_err(|_| {
            VirtioMmioDeviceConfigError::UnsupportedRead {
                offset: access.offset(),
                len: access.len(),
            }
        })?;
        let Some(mtu_end) = mtu_offset.checked_add(VIRTIO_NET_CONFIG_MTU_SIZE) else {
            return Err(VirtioMmioDeviceConfigError::UnsupportedRead {
                offset: access.offset(),
                len: access.len(),
            });
        };
        if offset >= mtu_offset && end <= mtu_end {
            let relative_offset = offset - mtu_offset;
            let bytes = mtu.to_le_bytes();
            if let Some(bytes) = bytes.get(relative_offset..relative_offset + access.len()) {
                return MmioAccessBytes::new(bytes).map_err(network_config_bytes_error);
            }
        }
    }

    Err(VirtioMmioDeviceConfigError::UnsupportedRead {
        offset: access.offset(),
        len: access.len(),
    })
}

fn network_config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: MmioHandlerError::new(format!("virtio-net config access bytes failed: {source}")),
    }
}

fn network_descriptor_at(
    chain: &VirtqueueDescriptorChain,
    index: usize,
    expected: usize,
) -> Result<&VirtqueueDescriptor, VirtioNetworkTxFrameParseError> {
    chain
        .descriptors()
        .get(index)
        .ok_or(VirtioNetworkTxFrameParseError::DescriptorChainTooShort {
            expected,
            actual: chain.len(),
        })
}

fn validate_tx_header_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<(), VirtioNetworkTxFrameParseError> {
    if descriptor.is_write_only() {
        return Err(VirtioNetworkTxFrameParseError::HeaderDescriptorWriteOnly {
            index: descriptor.index(),
        });
    }

    if descriptor.len() < VIRTIO_NET_TX_HEADER_SIZE {
        return Err(VirtioNetworkTxFrameParseError::HeaderDescriptorTooSmall {
            index: descriptor.index(),
            len: descriptor.len(),
            min: VIRTIO_NET_TX_HEADER_SIZE,
        });
    }

    Ok(())
}

fn read_virtio_network_tx_header(
    memory: &GuestMemory,
    descriptor: &VirtqueueDescriptor,
) -> Result<VirtioNetworkTxHeader, VirtioNetworkTxFrameParseError> {
    let mut bytes = [0; VIRTIO_NET_TX_HEADER_SIZE as usize];
    memory
        .read_slice(&mut bytes, descriptor.address())
        .map_err(|source| VirtioNetworkTxFrameParseError::ReadHeader {
            address: descriptor.address(),
            source,
        })?;

    Ok(VirtioNetworkTxHeader {
        flags: network_header_byte(&bytes, 0)?,
        gso_type: network_header_byte(&bytes, 1)?,
        header_len: u16::from_le_bytes(network_header_field(&bytes, 2)?),
        gso_size: u16::from_le_bytes(network_header_field(&bytes, 4)?),
        checksum_start: u16::from_le_bytes(network_header_field(&bytes, 6)?),
        checksum_offset: u16::from_le_bytes(network_header_field(&bytes, 8)?),
        num_buffers: u16::from_le_bytes(network_header_field(&bytes, 10)?),
    })
}

fn network_header_byte(
    bytes: &[u8; VIRTIO_NET_TX_HEADER_SIZE as usize],
    offset: usize,
) -> Result<u8, VirtioNetworkTxFrameParseError> {
    bytes
        .get(offset)
        .copied()
        .ok_or(VirtioNetworkTxFrameParseError::InvalidHeaderLayout)
}

fn network_header_field<const N: usize>(
    bytes: &[u8; VIRTIO_NET_TX_HEADER_SIZE as usize],
    offset: usize,
) -> Result<[u8; N], VirtioNetworkTxFrameParseError> {
    let Some(end) = offset.checked_add(N) else {
        return Err(VirtioNetworkTxFrameParseError::InvalidHeaderLayout);
    };
    let Some(source) = bytes.get(offset..end) else {
        return Err(VirtioNetworkTxFrameParseError::InvalidHeaderLayout);
    };
    let mut field = [0; N];
    field.copy_from_slice(source);
    Ok(field)
}

fn header_descriptor_payload_segment(
    descriptor: &VirtqueueDescriptor,
) -> Result<Option<VirtioNetworkTxPayloadSegment>, VirtioNetworkTxFrameParseError> {
    let payload_len = descriptor
        .len()
        .checked_sub(VIRTIO_NET_TX_HEADER_SIZE)
        .ok_or(VirtioNetworkTxFrameParseError::HeaderDescriptorTooSmall {
            index: descriptor.index(),
            len: descriptor.len(),
            min: VIRTIO_NET_TX_HEADER_SIZE,
        })?;
    if payload_len == 0 {
        return Ok(None);
    }

    let address = descriptor
        .address()
        .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
        .ok_or(
            VirtioNetworkTxFrameParseError::PayloadDescriptorRangeOverflow {
                index: descriptor.index(),
                address: descriptor.address(),
                len: payload_len,
            },
        )?;
    Ok(Some(VirtioNetworkTxPayloadSegment::new(
        descriptor.index(),
        address,
        payload_len,
    )))
}

fn validate_tx_payload_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<(), VirtioNetworkTxFrameParseError> {
    if descriptor.is_write_only() {
        return Err(VirtioNetworkTxFrameParseError::PayloadDescriptorWriteOnly {
            index: descriptor.index(),
        });
    }

    if descriptor.is_empty() {
        return Err(VirtioNetworkTxFrameParseError::PayloadDescriptorEmpty {
            index: descriptor.index(),
        });
    }

    Ok(())
}

fn push_tx_payload_segment(
    memory: &GuestMemory,
    payload_segments: &mut Vec<VirtioNetworkTxPayloadSegment>,
    payload_len: u64,
    segment: VirtioNetworkTxPayloadSegment,
) -> Result<u64, VirtioNetworkTxFrameParseError> {
    let next_payload_len = payload_len + u64::from(segment.len());
    let next_frame_len = u64::from(VIRTIO_NET_TX_HEADER_SIZE) + next_payload_len;
    if next_frame_len > VIRTIO_NET_MAX_BUFFER_SIZE {
        return Err(VirtioNetworkTxFrameParseError::FrameTooLarge {
            len: next_frame_len,
            max: VIRTIO_NET_MAX_BUFFER_SIZE,
        });
    }

    validate_tx_payload_segment_range(memory, segment)?;
    payload_segments.push(segment);
    Ok(next_payload_len)
}

fn validate_tx_payload_segment_range(
    memory: &GuestMemory,
    segment: VirtioNetworkTxPayloadSegment,
) -> Result<(), VirtioNetworkTxFrameParseError> {
    let range =
        GuestMemoryRange::new(segment.address(), u64::from(segment.len())).map_err(|_| {
            VirtioNetworkTxFrameParseError::PayloadDescriptorRangeOverflow {
                index: segment.descriptor_index(),
                address: segment.address(),
                len: segment.len(),
            }
        })?;

    memory.validate_mapped_range(range).map_err(|source| {
        VirtioNetworkTxFrameParseError::PayloadDescriptorAccess {
            index: segment.descriptor_index(),
            address: segment.address(),
            len: segment.len(),
            source,
        }
    })
}

fn validate_rx_buffer_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<(), VirtioNetworkRxBufferParseError> {
    if !descriptor.is_write_only() {
        return Err(VirtioNetworkRxBufferParseError::BufferDescriptorReadOnly {
            index: descriptor.index(),
        });
    }

    if descriptor.is_empty() {
        return Err(VirtioNetworkRxBufferParseError::BufferDescriptorEmpty {
            index: descriptor.index(),
        });
    }

    Ok(())
}

fn push_rx_buffer_segment(
    memory: &GuestMemory,
    segments: &mut Vec<VirtioNetworkRxBufferSegment>,
    len: u64,
    segment: VirtioNetworkRxBufferSegment,
) -> Result<u64, VirtioNetworkRxBufferParseError> {
    let next_len = len.checked_add(u64::from(segment.len())).ok_or(
        VirtioNetworkRxBufferParseError::BufferLengthOverflow {
            current: len,
            len: segment.len(),
        },
    )?;

    validate_rx_buffer_segment_range(memory, segment)?;
    segments.push(segment);
    Ok(next_len)
}

fn validate_rx_buffer_segment_range(
    memory: &GuestMemory,
    segment: VirtioNetworkRxBufferSegment,
) -> Result<(), VirtioNetworkRxBufferParseError> {
    let range =
        GuestMemoryRange::new(segment.address(), u64::from(segment.len())).map_err(|_| {
            VirtioNetworkRxBufferParseError::BufferDescriptorRangeOverflow {
                index: segment.descriptor_index(),
                address: segment.address(),
                len: segment.len(),
            }
        })?;

    memory.validate_mapped_range(range).map_err(|source| {
        VirtioNetworkRxBufferParseError::BufferDescriptorAccess {
            index: segment.descriptor_index(),
            address: segment.address(),
            len: segment.len(),
            source,
        }
    })
}

fn active_network_queue_state(
    activation: VirtioMmioDeviceActivation<'_>,
    queue_index: u32,
) -> Result<VirtioMmioQueueState, VirtioNetworkDeviceActivationError> {
    let queue = *activation.queue(queue_index).map_err(|source| {
        VirtioNetworkDeviceActivationError::QueueMetadata {
            queue_index,
            source,
        }
    })?;

    if !queue.ready() {
        return Err(VirtioNetworkDeviceActivationError::QueueNotReady { queue_index });
    }

    if queue.size() == 0 {
        return Err(VirtioNetworkDeviceActivationError::QueueSizeNotConfigured { queue_index });
    }

    Ok(queue)
}

const fn virtio_feature_enabled(features: u64, feature: u32) -> bool {
    features & (1_u64 << feature) != 0
}

const fn usize_to_u64_saturating(value: usize) -> u64 {
    if value > u64::MAX as usize {
        u64::MAX
    } else {
        value as u64
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
    TooManyNetworkInterfaces {
        count: usize,
        max: usize,
    },
    InvalidMtu {
        mtu: u16,
    },
    /// Contained process policy does not admit system host networking.
    HostNetworkNotAuthorized,
}

/// Redacted failure while preparing or committing a post-start network
/// insertion or removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkRuntimeMutationError {
    InvalidConfig(NetworkInterfaceConfigError),
    EmptyInterfaceId,
    InvalidInterfaceId { iface_id: String },
    DuplicateInterface { iface_id: String },
    UnknownInterface { iface_id: String },
    ConfigurationAllocation,
    PciNotEnabled,
    HostNetworkNotAuthorized,
    ActiveSessionUnavailable,
    ActiveSessionCommand { message: String },
    PreparePacketIo { message: String },
    PrepareDevice { message: String },
    PublishDevice { message: String },
    TerminalInsertion { message: String },
    RemoveDevice { message: String },
    TerminalRemoval { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkInterfaceUpdateError {
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
    UnknownInterface {
        iface_id: String,
    },
    HandlerLookup {
        iface_id: String,
        region_id: MmioRegionId,
        message: String,
    },
    ActiveSessionCommand {
        message: String,
    },
    ActiveSessionUnavailable,
    MmioDispatcherUnavailable,
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
            Self::TooManyNetworkInterfaces { max, .. } => {
                write!(f, "network interface count exceeds maximum {max}")
            }
            Self::InvalidMtu { mtu } => {
                write!(
                    f,
                    "network mtu {mtu} is out of range [{VIRTIO_NET_MIN_MTU}, {VIRTIO_NET_MAX_MTU}]"
                )
            }
            Self::HostNetworkNotAuthorized => {
                f.write_str("system host networking is not authorized")
            }
        }
    }
}

impl std::error::Error for NetworkInterfaceConfigError {}

impl fmt::Display for NetworkRuntimeMutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(source) => write!(f, "{source}"),
            Self::EmptyInterfaceId => f.write_str("path iface_id must not be empty"),
            Self::InvalidInterfaceId { .. } => {
                f.write_str("path iface_id must contain only alphanumeric characters or '_'")
            }
            Self::DuplicateInterface { .. } => {
                f.write_str("network interface is already configured")
            }
            Self::UnknownInterface { .. } => f.write_str("network interface is not configured"),
            Self::ConfigurationAllocation => {
                f.write_str("failed to reserve runtime network configuration storage")
            }
            Self::PciNotEnabled => {
                f.write_str("runtime network insertion and removal require PCI transport")
            }
            Self::HostNetworkNotAuthorized => {
                f.write_str("system host networking is not authorized")
            }
            Self::ActiveSessionUnavailable => {
                f.write_str("active runtime network session is unavailable")
            }
            Self::ActiveSessionCommand { message } => {
                write!(f, "runtime network command failed: {message}")
            }
            Self::PreparePacketIo { message } => {
                write!(f, "failed to prepare runtime network packet I/O: {message}")
            }
            Self::PrepareDevice { message } => {
                write!(f, "failed to prepare runtime network device: {message}")
            }
            Self::PublishDevice { message } => {
                write!(f, "failed to publish runtime network device: {message}")
            }
            Self::TerminalInsertion { message } => write!(
                f,
                "runtime network insertion entered terminal cleanup: {message}"
            ),
            Self::RemoveDevice { message } => {
                write!(f, "failed to remove runtime network device: {message}")
            }
            Self::TerminalRemoval { message } => write!(
                f,
                "runtime network removal entered terminal cleanup: {message}"
            ),
        }
    }
}

impl std::error::Error for NetworkRuntimeMutationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidConfig(source) => Some(source),
            Self::EmptyInterfaceId
            | Self::InvalidInterfaceId { .. }
            | Self::DuplicateInterface { .. }
            | Self::UnknownInterface { .. }
            | Self::ConfigurationAllocation
            | Self::PciNotEnabled
            | Self::HostNetworkNotAuthorized
            | Self::ActiveSessionUnavailable
            | Self::ActiveSessionCommand { .. }
            | Self::PreparePacketIo { .. }
            | Self::PrepareDevice { .. }
            | Self::PublishDevice { .. }
            | Self::TerminalInsertion { .. }
            | Self::RemoveDevice { .. }
            | Self::TerminalRemoval { .. } => None,
        }
    }
}

impl fmt::Display for NetworkInterfaceUpdateError {
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
            Self::UnknownInterface { .. } => f.write_str("network interface is not configured"),
            Self::HandlerLookup {
                region_id, message, ..
            } => {
                write!(
                    f,
                    "failed to find active network interface handler for MMIO region {region_id}: {message}"
                )
            }
            Self::ActiveSessionCommand { message } => {
                write!(
                    f,
                    "active network interface update command failed: {message}"
                )
            }
            Self::ActiveSessionUnavailable => {
                f.write_str("active network interface update session is unavailable")
            }
            Self::MmioDispatcherUnavailable => {
                f.write_str("active network interface MMIO dispatcher is unavailable")
            }
        }
    }
}

impl std::error::Error for NetworkInterfaceUpdateError {}

pub fn validate_network_interface_count(count: usize) -> Result<(), NetworkInterfaceConfigError> {
    if count > MAX_NETWORK_INTERFACE_COUNT {
        return Err(NetworkInterfaceConfigError::TooManyNetworkInterfaces {
            count,
            max: MAX_NETWORK_INTERFACE_COUNT,
        });
    }

    Ok(())
}

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

fn validate_interface_update_id(
    source: InterfaceIdSource,
    iface_id: &str,
) -> Result<(), NetworkInterfaceUpdateError> {
    if iface_id.is_empty() {
        return Err(NetworkInterfaceUpdateError::EmptyInterfaceId { source });
    }

    if !iface_id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(NetworkInterfaceUpdateError::InvalidInterfaceId {
            source,
            iface_id: iface_id.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::str::FromStr;
    use std::time::{Duration, Instant};

    use crate::interrupt::DeviceInterruptKind;
    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};
    use crate::metrics::{NetworkInterfaceMetrics, SharedNetworkInterfaceMetrics};
    use crate::mmio::{
        MmioAccess, MmioAccessBytes, MmioBus, MmioDispatchOutcome, MmioDispatcher, MmioOperation,
        MmioRegionId,
    };
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_MAGIC_VALUE, VirtioMmioDeviceActivation,
        VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
        VirtioMmioDeviceRegisters, VirtioMmioQueueRegisterError, VirtioMmioQueueRegisters,
        VirtioMmioQueueState, VirtioMmioRegister, VirtioMmioRegisterHandler,
        VirtioMmioRegisterHandlerError,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_INDIRECT, VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE,
        VIRTQUEUE_DESCRIPTOR_SIZE, VirtqueueDescriptorChain, VirtqueueDescriptorChainOptions,
        read_descriptor_chain, read_descriptor_chain_with_options,
    };

    use super::{
        GuestMacAddress, InterfaceIdSource, MAX_NETWORK_INTERFACE_COUNT, NetworkDeviceProfile,
        NetworkInterfaceConfig, NetworkInterfaceConfigError, NetworkInterfaceConfigInput,
        NetworkInterfaceConfigs, NetworkInterfaceUpdateError, NetworkInterfaceUpdateInput,
        NetworkMmioDevices, NetworkMmioLayout, NetworkMmioRegistrationError,
        NetworkRateLimiterConfig, NetworkRuntimeMutationError, NetworkTokenBucketConfig,
        PreparedNetworkDeviceError, PreparedNetworkDevices, VIRTIO_FEATURE_VERSION_1,
        VIRTIO_NET_CONFIG_MAC_SIZE, VIRTIO_NET_CONFIG_MTU_OFFSET, VIRTIO_NET_CONFIG_MTU_SIZE,
        VIRTIO_NET_DEVICE_ID, VIRTIO_NET_F_CSUM, VIRTIO_NET_F_GUEST_CSUM, VIRTIO_NET_F_GUEST_TSO4,
        VIRTIO_NET_F_GUEST_TSO6, VIRTIO_NET_F_GUEST_UFO, VIRTIO_NET_F_HOST_TSO4,
        VIRTIO_NET_F_HOST_TSO6, VIRTIO_NET_F_HOST_UFO, VIRTIO_NET_F_MAC, VIRTIO_NET_F_MRG_RXBUF,
        VIRTIO_NET_F_MTU, VIRTIO_NET_MAX_BUFFER_SIZE, VIRTIO_NET_MAX_MTU, VIRTIO_NET_MIN_MTU,
        VIRTIO_NET_QUEUE_COUNT, VIRTIO_NET_QUEUE_SIZE, VIRTIO_NET_QUEUE_SIZES,
        VIRTIO_NET_RX_MIN_BUFFER_SIZE, VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_HEADER_SIZE,
        VIRTIO_NET_TX_QUEUE_INDEX, VIRTIO_RING_FEATURE_EVENT_IDX,
        VIRTIO_RING_FEATURE_INDIRECT_DESC, VirtioNetworkBackendMetrics, VirtioNetworkConfigSpace,
        VirtioNetworkDevice, VirtioNetworkDeviceActivationError, VirtioNetworkDeviceCaptureError,
        VirtioNetworkDeviceNotificationError, VirtioNetworkFeatureCapabilities,
        VirtioNetworkLatencyAggregate, VirtioNetworkMmioHandler, VirtioNetworkPacketEnvelope,
        VirtioNetworkRateLimiter, VirtioNetworkRetryCaptureState, VirtioNetworkRxBuffer,
        VirtioNetworkRxBufferParseError, VirtioNetworkRxPacket, VirtioNetworkRxPacketSource,
        VirtioNetworkRxPacketSourceError, VirtioNetworkRxQueueDispatchError, VirtioNetworkTxFrame,
        VirtioNetworkTxFrameParseError, VirtioNetworkTxPacketCommit,
        VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSink,
        VirtioNetworkTxPacketSinkError, VirtioNetworkTxPacketStage, VirtioNetworkTxQueue,
        VirtioNetworkTxQueueDispatchError,
    };

    const TEST_MMIO_BASE: GuestAddress = GuestAddress::new(0x1000);
    const TEST_RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x10_0000);
    const TEST_RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x11_0000);
    const TEST_RX_USED_RING: GuestAddress = GuestAddress::new(0x12_0000);
    const TEST_RX_BUFFER: GuestAddress = GuestAddress::new(0x13_0000);
    const TEST_RX_SECOND_BUFFER: GuestAddress = GuestAddress::new(0x14_0000);
    const TEST_RX_INDIRECT_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x15_0000);
    const TEST_TX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x20_0000);
    const TEST_TX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x21_0000);
    const TEST_TX_USED_RING: GuestAddress = GuestAddress::new(0x22_0000);
    const TEST_TX_HEADER: GuestAddress = GuestAddress::new(0x23_0000);
    const TEST_TX_SECOND_HEADER: GuestAddress = GuestAddress::new(0x23_1000);
    const TEST_TX_PAYLOAD: GuestAddress = GuestAddress::new(0x24_0000);
    const TEST_TX_SECOND_PAYLOAD: GuestAddress = GuestAddress::new(0x25_0000);
    const TEST_TX_INDIRECT_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x26_0000);
    const TEST_TX_MEMORY_SIZE: u64 = 0x30_0000;
    const TEST_QUEUE_SIZE: u16 = 8;
    const TEST_RETRY_QUEUE_SIZE: u16 = 16;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;

    fn input() -> NetworkInterfaceConfigInput {
        NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0")
    }

    fn numbered_input(index: usize) -> NetworkInterfaceConfigInput {
        let iface_id = format!("eth{index}");
        let host_dev_name = format!("tap{index}");
        NetworkInterfaceConfigInput::new(iface_id.clone(), iface_id, host_dev_name)
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

    #[test]
    fn virtio_network_rx_packet_debug_redacts_packet_bytes() {
        let protected_value = "private-rx-token-value-that-must-not-appear";
        let byte_sequence = protected_value
            .as_bytes()
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let packet = VirtioNetworkRxPacket::new(protected_value.as_bytes());
        let debug_output = format!("{packet:?}");

        assert!(!debug_output.contains(protected_value));
        assert!(!debug_output.contains(&byte_sequence));
        assert!(debug_output.contains("[REDACTED]"));
        assert!(debug_output.contains(&format!("len: {}", protected_value.len())));
    }

    #[test]
    fn empty_network_latency_aggregate_is_canonical() {
        let aggregate = VirtioNetworkLatencyAggregate::new(7, 11, 18, 0);

        assert_eq!(aggregate, VirtioNetworkLatencyAggregate::default());
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

    fn network_activation_handler() -> VirtioNetworkMmioHandler {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()), None);
        VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_NET_DEVICE_ID,
            config.available_features(),
            &VIRTIO_NET_QUEUE_SIZES,
            config,
            VirtioNetworkDevice::new(),
        )
        .expect("network activation handler should build")
    }

    fn network_activation_handler_with_rate_limiters_at(
        rx_rate_limiter: Option<NetworkRateLimiterConfig>,
        tx_rate_limiter: Option<NetworkRateLimiterConfig>,
        now: Instant,
    ) -> VirtioNetworkMmioHandler {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()), None);
        VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_NET_DEVICE_ID,
            config.available_features(),
            &VIRTIO_NET_QUEUE_SIZES,
            config,
            VirtioNetworkDevice::with_rate_limiters_at(rx_rate_limiter, tx_rate_limiter, now),
        )
        .expect("rate-limited network activation handler should build")
    }

    fn read_network_config(
        config: &VirtioNetworkConfigSpace,
        offset: u64,
        len: usize,
    ) -> Result<MmioAccessBytes, VirtioMmioRegisterHandlerError> {
        network_handler(config.clone())
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

    fn dispatch_network_mmio_read(
        devices: &mut NetworkMmioDevices,
        device_index: usize,
        offset: u64,
        len: u64,
    ) -> MmioAccessBytes {
        let address = devices.registrations()[device_index]
            .address()
            .checked_add(offset)
            .expect("test MMIO address should not overflow");
        let access = devices
            .dispatcher()
            .lookup(address, len)
            .expect("test MMIO access should resolve");
        match devices
            .dispatcher_mut()
            .dispatch(MmioOperation::read(access).expect("test read operation should be valid"))
            .expect("test MMIO read should dispatch")
        {
            MmioDispatchOutcome::Read { data } => data,
            MmioDispatchOutcome::Write => panic!("read operation should not produce write outcome"),
        }
    }

    fn dispatch_network_mmio_read_u32(
        devices: &mut NetworkMmioDevices,
        device_index: usize,
        offset: u64,
    ) -> u32 {
        let data = dispatch_network_mmio_read(devices, device_index, offset, 4);
        u32::from_le_bytes(
            data.as_slice()
                .try_into()
                .expect("test MMIO read should return 4 bytes"),
        )
    }

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test address should fit in queue low register")
    }

    fn network_device_registers() -> VirtioMmioDeviceRegisters {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()), None);
        VirtioMmioDeviceRegisters::new(VIRTIO_NET_DEVICE_ID, config.available_features())
    }

    fn configured_network_queues(
        rx_size: Option<u16>,
        rx_ready: bool,
        tx_size: Option<u16>,
        tx_ready: bool,
    ) -> VirtioMmioQueueRegisters {
        let mut queues = VirtioMmioQueueRegisters::new(&VIRTIO_NET_QUEUE_SIZES)
            .expect("network queue table should build");
        configure_network_queue_registers(
            &mut queues,
            VIRTIO_NET_RX_QUEUE_INDEX
                .try_into()
                .expect("RX queue index should fit"),
            rx_size,
            rx_ready,
        );
        configure_network_queue_registers(
            &mut queues,
            VIRTIO_NET_TX_QUEUE_INDEX
                .try_into()
                .expect("TX queue index should fit"),
            tx_size,
            tx_ready,
        );
        queues
    }

    fn configure_network_queue_registers(
        queues: &mut VirtioMmioQueueRegisters,
        queue_index: u32,
        queue_size: Option<u16>,
        ready: bool,
    ) {
        let (descriptor_table, driver_ring, device_ring) = network_queue_addresses(queue_index);
        queues
            .write_register(
                VirtioMmioRegister::QueueSel,
                queue_index,
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue should be selectable");
        if let Some(queue_size) = queue_size {
            queues
                .write_register(
                    VirtioMmioRegister::QueueNum,
                    u32::from(queue_size),
                    QUEUE_CONFIG_STATUS,
                )
                .expect("queue size should write");
        }
        queues
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(descriptor_table),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue descriptor table should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(driver_ring),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue driver ring should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                guest_address_low(device_ring),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue device ring should write");
        if ready {
            queues
                .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
                .expect("queue ready should write");
        }
    }

    fn put_network_handler_in_queue_config_state(handler: &mut VirtioNetworkMmioHandler) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("status should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("status should accept DRIVER");
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("status should accept FEATURES_OK");
    }

    fn put_network_handler_in_queue_config_state_with_event_idx(
        handler: &mut VirtioNetworkMmioHandler,
    ) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("status should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("status should accept DRIVER");
        handler
            .write_register(
                VirtioMmioRegister::DriverFeatures,
                1_u32 << VIRTIO_RING_FEATURE_EVENT_IDX,
            )
            .expect("event index feature should write");
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("status should accept FEATURES_OK");
    }

    fn configure_network_handler_queue(
        handler: &mut VirtioNetworkMmioHandler,
        queue_index: u32,
        queue_size: u16,
    ) {
        let (descriptor_table, driver_ring, device_ring) = network_queue_addresses(queue_index);
        handler
            .write_register(VirtioMmioRegister::QueueSel, queue_index)
            .expect("queue should be selectable");
        handler
            .write_register(VirtioMmioRegister::QueueNum, u32::from(queue_size))
            .expect("queue size should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(descriptor_table),
            )
            .expect("queue descriptor table should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(driver_ring),
            )
            .expect("queue driver ring should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                guest_address_low(device_ring),
            )
            .expect("queue device ring should write");
        handler
            .write_register(VirtioMmioRegister::QueueReady, 1)
            .expect("queue ready should write");
    }

    fn configure_network_handler_queues(handler: &mut VirtioNetworkMmioHandler) {
        put_network_handler_in_queue_config_state(handler);
        configure_network_handler_queue(
            handler,
            VIRTIO_NET_RX_QUEUE_INDEX
                .try_into()
                .expect("RX queue index should fit"),
            TEST_QUEUE_SIZE,
        );
        configure_network_handler_queue(
            handler,
            VIRTIO_NET_TX_QUEUE_INDEX
                .try_into()
                .expect("TX queue index should fit"),
            TEST_QUEUE_SIZE,
        );
    }

    fn configure_network_handler_queues_for_capture(handler: &mut VirtioNetworkMmioHandler) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("capture handler should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("capture handler should accept DRIVER");
        handler
            .write_register(VirtioMmioRegister::DriverFeaturesSel, 1)
            .expect("VERSION_1 feature selector should write");
        handler
            .write_register(VirtioMmioRegister::DriverFeatures, 1)
            .expect("VERSION_1 feature should negotiate");
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("capture handler should accept FEATURES_OK");
        configure_network_handler_queue(
            handler,
            VIRTIO_NET_RX_QUEUE_INDEX
                .try_into()
                .expect("RX queue index should fit"),
            TEST_QUEUE_SIZE,
        );
        configure_network_handler_queue(
            handler,
            VIRTIO_NET_TX_QUEUE_INDEX
                .try_into()
                .expect("TX queue index should fit"),
            TEST_QUEUE_SIZE,
        );
    }

    fn configure_network_handler_queues_with_features(
        handler: &mut VirtioNetworkMmioHandler,
        features: u32,
    ) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("status should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("status should accept DRIVER");
        handler
            .write_register(VirtioMmioRegister::DriverFeatures, features)
            .expect("network feature selection should write");
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("status should accept FEATURES_OK");
        configure_network_handler_queue(
            handler,
            VIRTIO_NET_RX_QUEUE_INDEX
                .try_into()
                .expect("RX queue index should fit"),
            TEST_QUEUE_SIZE,
        );
        configure_network_handler_queue(
            handler,
            VIRTIO_NET_TX_QUEUE_INDEX
                .try_into()
                .expect("TX queue index should fit"),
            TEST_QUEUE_SIZE,
        );
    }

    fn configure_network_handler_queues_with_event_idx(handler: &mut VirtioNetworkMmioHandler) {
        put_network_handler_in_queue_config_state_with_event_idx(handler);
        configure_network_handler_queue(
            handler,
            VIRTIO_NET_RX_QUEUE_INDEX
                .try_into()
                .expect("RX queue index should fit"),
            TEST_QUEUE_SIZE,
        );
        configure_network_handler_queue(
            handler,
            VIRTIO_NET_TX_QUEUE_INDEX
                .try_into()
                .expect("TX queue index should fit"),
            TEST_QUEUE_SIZE,
        );
    }

    fn activate_network_handler(handler: &mut VirtioNetworkMmioHandler) {
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("DRIVER_OK should activate network device");
    }

    fn network_queue_addresses(queue_index: u32) -> (GuestAddress, GuestAddress, GuestAddress) {
        match queue_index {
            0 => (
                TEST_RX_DESCRIPTOR_TABLE,
                TEST_RX_AVAILABLE_RING,
                TEST_RX_USED_RING,
            ),
            1 => (
                TEST_TX_DESCRIPTOR_TABLE,
                TEST_TX_AVAILABLE_RING,
                TEST_TX_USED_RING,
            ),
            other => panic!("unsupported test queue index {other}"),
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct TestDescriptor {
        address: GuestAddress,
        len: u32,
        flags: u16,
        next: u16,
    }

    impl TestDescriptor {
        const fn readable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_NEXT, index),
                None => (0, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }

        const fn writable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_WRITE | VIRTQUEUE_DESC_F_NEXT, index),
                None => (VIRTQUEUE_DESC_F_WRITE, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }

        const fn indirect(address: GuestAddress, len: u32) -> Self {
            Self {
                address,
                len,
                flags: VIRTQUEUE_DESC_F_INDIRECT,
                next: 0,
            }
        }
    }

    #[derive(Debug, Default)]
    struct RecordingTxPacketSink {
        calls: usize,
        fail_on_call: Option<usize>,
        detour_successes: bool,
        frame_heads: Vec<u16>,
        packets: Vec<Vec<u8>>,
        backend_metrics: VirtioNetworkBackendMetrics,
    }

    impl RecordingTxPacketSink {
        fn failing_on(fail_on_call: usize) -> Self {
            Self {
                calls: 0,
                fail_on_call: Some(fail_on_call),
                detour_successes: false,
                frame_heads: Vec::new(),
                packets: Vec::new(),
                backend_metrics: VirtioNetworkBackendMetrics::default(),
            }
        }

        fn detouring() -> Self {
            Self {
                calls: 0,
                fail_on_call: None,
                detour_successes: true,
                frame_heads: Vec::new(),
                packets: Vec::new(),
                backend_metrics: VirtioNetworkBackendMetrics::default(),
            }
        }
    }

    impl VirtioNetworkTxPacketSink for RecordingTxPacketSink {
        fn transmit_frame(
            &mut self,
            memory: &GuestMemory,
            frame: &VirtioNetworkTxFrame,
        ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
            self.calls += 1;
            self.frame_heads.push(frame.descriptor_head());

            if self.fail_on_call == Some(self.calls) {
                return Err(VirtioNetworkTxPacketSinkError::new(format!(
                    "test sink failure on call {}",
                    self.calls
                )));
            }

            let mut packet = Vec::new();
            packet
                .try_reserve_exact(
                    usize::try_from(frame.payload_len())
                        .expect("test payload length should fit usize"),
                )
                .expect("test packet allocation should succeed");
            for segment in frame.payload_segments() {
                let mut bytes = vec![
                    0;
                    usize::try_from(segment.len())
                        .expect("test payload segment length should fit usize")
                ];
                memory
                    .read_slice(&mut bytes, segment.address())
                    .expect("test payload segment should read");
                packet.extend_from_slice(&bytes);
            }
            self.packets.push(packet);

            if self.detour_successes {
                Ok(VirtioNetworkTxPacketDisposition::Detoured)
            } else {
                Ok(VirtioNetworkTxPacketDisposition::Forwarded)
            }
        }

        fn transmit_prepared_frame(
            &mut self,
            _memory: &GuestMemory,
            frame: &VirtioNetworkTxFrame,
            packet: &crate::network_packet::VirtioNetworkPacketPlan,
        ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
            self.calls += 1;
            self.frame_heads.push(frame.descriptor_head());
            if self.fail_on_call == Some(self.calls) {
                return Err(VirtioNetworkTxPacketSinkError::new(format!(
                    "test sink failure on call {}",
                    self.calls
                )));
            }
            let emitted = packet
                .emit(VirtioNetworkPacketEnvelope::RawEthernet)
                .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
            self.packets.push(emitted.bytes().to_vec());
            if self.detour_successes {
                Ok(VirtioNetworkTxPacketDisposition::Detoured)
            } else {
                Ok(VirtioNetworkTxPacketDisposition::Forwarded)
            }
        }

        fn take_backend_metrics(&mut self) -> VirtioNetworkBackendMetrics {
            std::mem::take(&mut self.backend_metrics)
        }
    }

    #[derive(Debug)]
    struct StagedRecordingTxFrame {
        descriptor_head: u16,
        packet: Vec<u8>,
    }

    #[derive(Debug, Default)]
    struct RecordingStagedTxPacketSink {
        staged: Option<StagedRecordingTxFrame>,
        committed: Vec<StagedRecordingTxFrame>,
        flush_before_heads: Vec<u16>,
        immediate_detour_heads: Vec<u16>,
        flush_failure_heads: Vec<u16>,
        flushed_packets: Vec<Vec<u8>>,
        events: Vec<String>,
        flush_calls: usize,
        discard_calls: usize,
    }

    impl RecordingStagedTxPacketSink {
        fn failing_flush_for(descriptor_head: u16) -> Self {
            Self {
                flush_failure_heads: vec![descriptor_head],
                ..Self::default()
            }
        }

        fn detouring_after_flush(descriptor_head: u16) -> Self {
            Self {
                flush_before_heads: vec![descriptor_head],
                immediate_detour_heads: vec![descriptor_head],
                ..Self::default()
            }
        }
    }

    impl VirtioNetworkTxPacketSink for RecordingStagedTxPacketSink {
        fn transmit_frame(
            &mut self,
            _memory: &GuestMemory,
            _frame: &VirtioNetworkTxFrame,
        ) -> Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError> {
            Err(VirtioNetworkTxPacketSinkError::new(
                "test staged sink used the single-frame path",
            ))
        }

        fn supports_staged_batch(&self) -> bool {
            true
        }

        fn stage_frame(
            &mut self,
            memory: &GuestMemory,
            frame: &VirtioNetworkTxFrame,
        ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
            assert!(self.staged.is_none(), "only one frame may be staged");
            let mut packet = Vec::new();
            packet
                .try_reserve_exact(
                    usize::try_from(frame.payload_len())
                        .expect("test payload length should fit usize"),
                )
                .expect("test staged packet allocation should succeed");
            for segment in frame.payload_segments() {
                let mut bytes = vec![
                    0;
                    usize::try_from(segment.len())
                        .expect("test payload segment length should fit usize")
                ];
                memory
                    .read_slice(&mut bytes, segment.address())
                    .expect("test staged payload segment should read");
                packet.extend_from_slice(&bytes);
            }
            let descriptor_head = frame.descriptor_head();
            self.events.push(format!("stage:{descriptor_head}"));
            self.staged = Some(StagedRecordingTxFrame {
                descriptor_head,
                packet,
            });
            Ok(VirtioNetworkTxPacketStage::Staged {
                flush_before_commit: self.flush_before_heads.contains(&descriptor_head),
            })
        }

        fn stage_prepared_frame(
            &mut self,
            _memory: &GuestMemory,
            frame: &VirtioNetworkTxFrame,
            packet: &crate::network_packet::VirtioNetworkPacketPlan,
        ) -> Result<VirtioNetworkTxPacketStage, VirtioNetworkTxPacketSinkError> {
            assert!(self.staged.is_none(), "only one frame may be staged");
            let emitted = packet
                .emit(VirtioNetworkPacketEnvelope::RawEthernet)
                .map_err(|source| VirtioNetworkTxPacketSinkError::new(source.to_string()))?;
            let descriptor_head = frame.descriptor_head();
            self.events.push(format!("stage:{descriptor_head}"));
            self.staged = Some(StagedRecordingTxFrame {
                descriptor_head,
                packet: emitted.bytes().to_vec(),
            });
            Ok(VirtioNetworkTxPacketStage::Staged {
                flush_before_commit: self.flush_before_heads.contains(&descriptor_head),
            })
        }

        fn commit_staged_frame(&mut self) -> VirtioNetworkTxPacketCommit {
            let staged = self.staged.take().expect("a test frame should be staged");
            self.events
                .push(format!("commit:{}", staged.descriptor_head));
            if self
                .immediate_detour_heads
                .contains(&staged.descriptor_head)
            {
                VirtioNetworkTxPacketCommit::Immediate(Ok(
                    VirtioNetworkTxPacketDisposition::Detoured,
                ))
            } else {
                self.committed.push(staged);
                VirtioNetworkTxPacketCommit::Deferred
            }
        }

        fn discard_staged_frame(&mut self) {
            let staged = self
                .staged
                .take()
                .expect("a test frame should exist before discard");
            self.events
                .push(format!("discard:{}", staged.descriptor_head));
            self.discard_calls += 1;
        }

        fn flush_staged_frames(
            &mut self,
            results: &mut Vec<
                Result<VirtioNetworkTxPacketDisposition, VirtioNetworkTxPacketSinkError>,
            >,
        ) {
            self.flush_calls += 1;
            let heads = self
                .committed
                .iter()
                .map(|frame| frame.descriptor_head.to_string())
                .collect::<Vec<_>>()
                .join(",");
            self.events.push(format!("flush:{heads}"));
            for frame in self.committed.drain(..) {
                self.flushed_packets.push(frame.packet);
                if self.flush_failure_heads.contains(&frame.descriptor_head) {
                    results.push(Err(VirtioNetworkTxPacketSinkError::new(format!(
                        "test staged flush failure for head {}",
                        frame.descriptor_head
                    ))));
                } else {
                    results.push(Ok(VirtioNetworkTxPacketDisposition::Forwarded));
                }
            }
        }
    }

    #[derive(Debug, Default)]
    struct RecordingRxPacketSource {
        packets: Vec<Vec<u8>>,
        next_packet: usize,
        peek_calls: usize,
        consume_calls: usize,
        fail_on_peek: Option<usize>,
        retry_after_tx_hint: bool,
        empty_peeks_before_packets: usize,
        empty_peeks_after_first_consume: usize,
        backend_metrics: VirtioNetworkBackendMetrics,
    }

    impl RecordingRxPacketSource {
        fn with_packets(packets: Vec<Vec<u8>>) -> Self {
            Self {
                packets,
                next_packet: 0,
                peek_calls: 0,
                consume_calls: 0,
                fail_on_peek: None,
                retry_after_tx_hint: false,
                empty_peeks_before_packets: 0,
                empty_peeks_after_first_consume: 0,
                backend_metrics: VirtioNetworkBackendMetrics::default(),
            }
        }

        fn failing_on_peek(fail_on_peek: usize, packets: Vec<Vec<u8>>) -> Self {
            Self {
                packets,
                next_packet: 0,
                peek_calls: 0,
                consume_calls: 0,
                fail_on_peek: Some(fail_on_peek),
                retry_after_tx_hint: false,
                empty_peeks_before_packets: 0,
                empty_peeks_after_first_consume: 0,
                backend_metrics: VirtioNetworkBackendMetrics::default(),
            }
        }

        fn remaining_packets(&self) -> usize {
            self.packets.len().saturating_sub(self.next_packet)
        }

        fn with_retry_after_tx_hint(mut self) -> Self {
            self.retry_after_tx_hint = true;
            self
        }

        fn with_empty_peeks_after_first_consume(mut self, empty_peeks: usize) -> Self {
            self.empty_peeks_after_first_consume = empty_peeks;
            self
        }
    }

    impl VirtioNetworkRxPacketSource for RecordingRxPacketSource {
        fn retry_after_tx_hint(&self) -> bool {
            self.retry_after_tx_hint && self.remaining_packets() != 0
        }

        fn peek_packet(
            &mut self,
        ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
            self.peek_calls += 1;
            if self.fail_on_peek == Some(self.peek_calls) {
                return Err(VirtioNetworkRxPacketSourceError::new(format!(
                    "test source failure on peek {}",
                    self.peek_calls
                )));
            }
            if self.empty_peeks_before_packets != 0 {
                self.empty_peeks_before_packets -= 1;
                return Ok(None);
            }

            Ok(self
                .packets
                .get(self.next_packet)
                .map(Vec::as_slice)
                .map(VirtioNetworkRxPacket::new))
        }

        fn consume_packet(&mut self) {
            self.consume_calls += 1;
            self.next_packet += 1;
            if self.next_packet == 1 && self.empty_peeks_after_first_consume != 0 {
                self.empty_peeks_before_packets = self.empty_peeks_after_first_consume;
                self.empty_peeks_after_first_consume = 0;
            }
        }

        fn take_backend_metrics(&mut self) -> VirtioNetworkBackendMetrics {
            std::mem::take(&mut self.backend_metrics)
        }
    }

    fn tx_frame_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_TX_MEMORY_SIZE)
                .expect("test range should be valid"),
        ])
        .expect("test memory layout should be valid");
        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    fn write_tx_header(memory: &mut GuestMemory, address: GuestAddress) {
        memory
            .write_slice(&[0; VIRTIO_NET_TX_HEADER_SIZE as usize], address)
            .expect("virtio-net TX header should write");
    }

    fn write_nonzero_tx_header(memory: &mut GuestMemory, address: GuestAddress) {
        let mut bytes = [0; VIRTIO_NET_TX_HEADER_SIZE as usize];
        let (flags, tail) = bytes.split_at_mut(1);
        let (gso_type, tail) = tail.split_at_mut(1);
        let (header_len, tail) = tail.split_at_mut(2);
        let (gso_size, tail) = tail.split_at_mut(2);
        let (checksum_start, tail) = tail.split_at_mut(2);
        let (checksum_offset, num_buffers) = tail.split_at_mut(2);

        flags.copy_from_slice(&[0x1]);
        gso_type.copy_from_slice(&[0x2]);
        header_len.copy_from_slice(&0x0304_u16.to_le_bytes());
        gso_size.copy_from_slice(&0x0506_u16.to_le_bytes());
        checksum_start.copy_from_slice(&0x0708_u16.to_le_bytes());
        checksum_offset.copy_from_slice(&0x090a_u16.to_le_bytes());
        num_buffers.copy_from_slice(&0x0b0c_u16.to_le_bytes());

        memory
            .write_slice(&bytes, address)
            .expect("virtio-net TX header should write");
    }

    fn tx_payload_address_after_header(header_address: GuestAddress) -> GuestAddress {
        header_address
            .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
            .expect("test TX payload address should not overflow")
    }

    fn write_tx_payload(memory: &mut GuestMemory, address: GuestAddress, bytes: &[u8]) {
        memory
            .write_slice(bytes, address)
            .expect("test TX payload should write");
    }

    fn write_two_tx_frames(memory: &mut GuestMemory) {
        write_tx_header(memory, TEST_TX_HEADER);
        write_tx_header(memory, TEST_TX_SECOND_HEADER);
        write_tx_payload(memory, TEST_TX_PAYLOAD, &[0x10, 0x11, 0x12, 0x13]);
        write_tx_payload(memory, TEST_TX_SECOND_PAYLOAD, &[0x20, 0x21, 0x22, 0x23]);
        for (index, descriptor) in [
            TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
            TestDescriptor::readable(TEST_TX_PAYLOAD, 4, None),
            TestDescriptor::readable(TEST_TX_SECOND_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(3)),
            TestDescriptor::readable(TEST_TX_SECOND_PAYLOAD, 4, None),
        ]
        .into_iter()
        .enumerate()
        {
            write_tx_descriptor(
                memory,
                u16::try_from(index).expect("test TX descriptor index should fit"),
                descriptor,
            );
        }
        write_tx_available_heads(memory, &[0, 2]);
    }

    fn write_tx_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        write_descriptor_at(memory, TEST_TX_DESCRIPTOR_TABLE, index, descriptor);
    }

    fn write_rx_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        write_descriptor_at(memory, TEST_RX_DESCRIPTOR_TABLE, index, descriptor);
    }

    fn write_descriptor_at(
        memory: &mut GuestMemory,
        descriptor_table: GuestAddress,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = descriptor_table
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("descriptor should write");
    }

    fn tx_descriptor_chain(
        memory: &mut GuestMemory,
        descriptors: &[TestDescriptor],
    ) -> VirtqueueDescriptorChain {
        for (index, descriptor) in descriptors.iter().copied().enumerate() {
            write_tx_descriptor(
                memory,
                u16::try_from(index).expect("test descriptor index should fit"),
                descriptor,
            );
        }

        read_descriptor_chain(memory, TEST_TX_DESCRIPTOR_TABLE, TEST_QUEUE_SIZE, 0)
            .expect("TX descriptor chain should read")
    }

    fn tx_indirect_descriptor_chain(
        memory: &mut GuestMemory,
        outer_head: u16,
        descriptors: &[TestDescriptor],
    ) -> VirtqueueDescriptorChain {
        write_indirect_descriptor_chain(
            memory,
            TEST_TX_DESCRIPTOR_TABLE,
            TEST_TX_INDIRECT_DESCRIPTOR_TABLE,
            outer_head,
            descriptors,
        )
    }

    fn rx_indirect_descriptor_chain(
        memory: &mut GuestMemory,
        outer_head: u16,
        descriptors: &[TestDescriptor],
    ) -> VirtqueueDescriptorChain {
        write_indirect_descriptor_chain(
            memory,
            TEST_RX_DESCRIPTOR_TABLE,
            TEST_RX_INDIRECT_DESCRIPTOR_TABLE,
            outer_head,
            descriptors,
        )
    }

    fn write_indirect_descriptor_chain(
        memory: &mut GuestMemory,
        descriptor_table: GuestAddress,
        indirect_table: GuestAddress,
        outer_head: u16,
        descriptors: &[TestDescriptor],
    ) -> VirtqueueDescriptorChain {
        let indirect_table_len = u32::try_from(
            descriptors
                .len()
                .checked_mul(VIRTQUEUE_DESCRIPTOR_SIZE)
                .expect("indirect descriptor table len should not overflow"),
        )
        .expect("indirect descriptor table len should fit in u32");
        write_descriptor_at(
            memory,
            descriptor_table,
            outer_head,
            TestDescriptor::indirect(indirect_table, indirect_table_len),
        );
        for (index, descriptor) in descriptors.iter().copied().enumerate() {
            write_descriptor_at(
                memory,
                indirect_table,
                u16::try_from(index).expect("test indirect descriptor index should fit"),
                descriptor,
            );
        }

        read_descriptor_chain_with_options(
            memory,
            descriptor_table,
            TEST_QUEUE_SIZE,
            outer_head,
            VirtqueueDescriptorChainOptions::new().with_indirect_descriptors(true),
        )
        .expect("indirect descriptor chain should read")
    }

    fn write_rx_descriptors(memory: &mut GuestMemory, descriptors: &[TestDescriptor]) {
        for (index, descriptor) in descriptors.iter().copied().enumerate() {
            write_rx_descriptor(
                memory,
                u16::try_from(index).expect("test RX descriptor index should fit"),
                descriptor,
            );
        }
    }

    fn write_guest_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("test u16 should write");
    }

    fn read_guest_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("test u16 should read");
        u16::from_le_bytes(bytes)
    }

    fn read_guest_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("test u32 should read");
        u32::from_le_bytes(bytes)
    }

    fn read_guest_bytes(memory: &GuestMemory, address: GuestAddress, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        memory
            .read_slice(&mut bytes, address)
            .expect("test bytes should read");
        bytes
    }

    fn rx_available_ring_idx_address() -> GuestAddress {
        TEST_RX_AVAILABLE_RING
            .checked_add(2)
            .expect("RX available ring idx address should not overflow")
    }

    fn rx_available_ring_entry_address(index: usize) -> GuestAddress {
        TEST_RX_AVAILABLE_RING
            .checked_add(
                4 + u64::try_from(index).expect("test index should fit") * u64::from(2_u16),
            )
            .expect("RX available ring entry address should not overflow")
    }

    fn rx_available_ring_used_event_address() -> GuestAddress {
        TEST_RX_AVAILABLE_RING
            .checked_add(4 + u64::from(TEST_QUEUE_SIZE) * 2)
            .expect("RX available ring used_event address should not overflow")
    }

    fn write_rx_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(memory, rx_available_ring_entry_address(index), head);
        }
        write_guest_u16(
            memory,
            rx_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test RX available head count should fit"),
        );
    }

    fn write_rx_available_used_event(memory: &mut GuestMemory, used_event: u16) {
        write_guest_u16(memory, rx_available_ring_used_event_address(), used_event);
    }

    fn rx_used_ring_idx_address() -> GuestAddress {
        TEST_RX_USED_RING
            .checked_add(2)
            .expect("RX used ring idx address should not overflow")
    }

    fn rx_used_ring_entry_address(index: usize) -> GuestAddress {
        TEST_RX_USED_RING
            .checked_add(4 + u64::try_from(index).expect("test index should fit") * 8)
            .expect("RX used ring entry address should not overflow")
    }

    fn read_rx_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, rx_used_ring_idx_address())
    }

    fn read_rx_used_element(memory: &GuestMemory, index: usize) -> (u32, u32) {
        let address = rx_used_ring_entry_address(index);
        let descriptor_head = read_guest_u32(memory, address);
        let len = read_guest_u32(
            memory,
            address
                .checked_add(4)
                .expect("RX used ring len address should not overflow"),
        );
        (descriptor_head, len)
    }

    fn rx_used_len(packet_len: usize) -> u32 {
        u32::try_from(
            u64::from(VIRTIO_NET_TX_HEADER_SIZE)
                + u64::try_from(packet_len).expect("test packet len should fit u64"),
        )
        .expect("test RX used len should fit u32")
    }

    fn tx_available_ring_idx_address() -> GuestAddress {
        TEST_TX_AVAILABLE_RING
            .checked_add(2)
            .expect("available ring idx address should not overflow")
    }

    fn tx_available_ring_entry_address(index: usize) -> GuestAddress {
        TEST_TX_AVAILABLE_RING
            .checked_add(
                4 + u64::try_from(index).expect("test index should fit") * u64::from(2_u16),
            )
            .expect("available ring entry address should not overflow")
    }

    fn tx_available_ring_used_event_address() -> GuestAddress {
        TEST_TX_AVAILABLE_RING
            .checked_add(4 + u64::from(TEST_QUEUE_SIZE) * 2)
            .expect("TX available ring used_event address should not overflow")
    }

    fn write_tx_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(memory, tx_available_ring_entry_address(index), head);
        }
        write_guest_u16(
            memory,
            tx_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available head count should fit"),
        );
    }

    fn write_tx_available_used_event(memory: &mut GuestMemory, used_event: u16) {
        write_guest_u16(memory, tx_available_ring_used_event_address(), used_event);
    }

    fn tx_used_ring_idx_address() -> GuestAddress {
        TEST_TX_USED_RING
            .checked_add(2)
            .expect("used ring idx address should not overflow")
    }

    fn tx_used_ring_entry_address(index: usize) -> GuestAddress {
        TEST_TX_USED_RING
            .checked_add(4 + u64::try_from(index).expect("test index should fit") * 8)
            .expect("used ring entry address should not overflow")
    }

    fn read_tx_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, tx_used_ring_idx_address())
    }

    fn read_tx_used_element(memory: &GuestMemory, index: usize) -> (u32, u32) {
        let address = tx_used_ring_entry_address(index);
        let descriptor_head = read_guest_u32(memory, address);
        let len = read_guest_u32(
            memory,
            address
                .checked_add(4)
                .expect("used ring len address should not overflow"),
        );
        (descriptor_head, len)
    }

    fn read_interrupt_status(handler: &VirtioNetworkMmioHandler) -> u32 {
        handler
            .read_register(VirtioMmioRegister::InterruptStatus)
            .expect("interrupt status should read")
    }

    fn acknowledge_queue_interrupt(handler: &mut VirtioNetworkMmioHandler) {
        handler
            .write_register(
                VirtioMmioRegister::InterruptAck,
                DeviceInterruptKind::Queue.status().bits(),
            )
            .expect("queue interrupt should acknowledge");
    }

    fn parse_tx_frame(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<VirtioNetworkTxFrame, VirtioNetworkTxFrameParseError> {
        VirtioNetworkTxFrame::parse(memory, chain)
    }

    fn parse_rx_buffer(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<VirtioNetworkRxBuffer, VirtioNetworkRxBufferParseError> {
        VirtioNetworkRxBuffer::parse(memory, chain)
    }

    #[test]
    fn accepts_minimal_network_interface_config() {
        let config = validate(input()).expect("minimal network config should be valid");

        assert_eq!(config.iface_id(), "eth0");
        assert_eq!(config.host_dev_name(), "tap0");
        assert_eq!(config.guest_mac(), None);
        assert_eq!(config.mtu(), None);
    }

    #[test]
    fn accepts_network_interface_config_with_guest_mac() {
        let config = validate(input().with_guest_mac("12:34:56:78:9a:BC"))
            .expect("network config with guest MAC should be valid");

        let guest_mac = config.guest_mac().expect("guest MAC should be stored");
        assert_eq!(guest_mac.octets(), [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        assert_eq!(guest_mac.to_string(), "12:34:56:78:9a:bc");
        assert_eq!(config.mtu(), None);
    }

    #[test]
    fn accepts_network_interface_config_with_mtu_bounds() {
        let min_config = validate(input().with_mtu(VIRTIO_NET_MIN_MTU))
            .expect("minimum network MTU should be valid");
        let max_config = validate(input().with_mtu(VIRTIO_NET_MAX_MTU))
            .expect("maximum network MTU should be valid");

        assert_eq!(min_config.mtu(), Some(VIRTIO_NET_MIN_MTU));
        assert_eq!(max_config.mtu(), Some(VIRTIO_NET_MAX_MTU));
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
    fn rejects_invalid_network_mtu() {
        assert_eq!(
            validate(input().with_mtu(VIRTIO_NET_MIN_MTU - 1)),
            Err(NetworkInterfaceConfigError::InvalidMtu {
                mtu: VIRTIO_NET_MIN_MTU - 1,
            })
        );
    }

    #[test]
    fn accepts_configured_network_rate_limiters() {
        let rx_rate_limiter = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(1024, Some(2048), 100)),
            None,
        );
        let tx_rate_limiter = NetworkRateLimiterConfig::new(
            None,
            Some(NetworkTokenBucketConfig::new(10, None, 1000)),
        );

        let config = validate(
            input()
                .with_rx_rate_limiter(rx_rate_limiter)
                .with_tx_rate_limiter(tx_rate_limiter),
        )
        .expect("configured network rate limiters should validate");

        assert_eq!(config.rx_rate_limiter(), Some(rx_rate_limiter));
        assert_eq!(config.tx_rate_limiter(), Some(tx_rate_limiter));
    }

    #[test]
    fn normalizes_disabled_network_rate_limiter_buckets() {
        let config = validate(input().with_rx_rate_limiter(NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(0, Some(2048), 100)),
            Some(NetworkTokenBucketConfig::new(10, None, 0)),
        )))
        .expect("disabled network rate limiter buckets should validate");

        assert_eq!(config.rx_rate_limiter(), None);
    }

    #[test]
    fn network_rate_limiter_enforces_ops_and_refills_at_injected_time() {
        let now = Instant::now();
        let config =
            NetworkRateLimiterConfig::new(None, Some(NetworkTokenBucketConfig::new(1, None, 100)));
        let mut limiter = VirtioNetworkRateLimiter::new_at(config, now)
            .expect("enabled ops limiter should build");

        assert!(matches!(
            limiter.reduce_at(4096, now),
            super::VirtioNetworkRateLimiterReduction::Allowed(_)
        ));
        assert_eq!(
            limiter.reduce_at(1, now),
            super::VirtioNetworkRateLimiterReduction::Throttled {
                retry_after: Duration::from_millis(100),
            }
        );
        let throttled = limiter.clone();
        assert_eq!(
            limiter.reduce_at(1, now + Duration::from_millis(99)),
            super::VirtioNetworkRateLimiterReduction::Throttled {
                retry_after: Duration::from_millis(1),
            }
        );
        assert_eq!(limiter, throttled);
        assert!(matches!(
            limiter.reduce_at(1, now + Duration::from_millis(100)),
            super::VirtioNetworkRateLimiterReduction::Allowed(_)
        ));
    }

    #[test]
    fn network_rate_limiter_rolls_back_ops_when_bandwidth_throttles() {
        let now = Instant::now();
        let config = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(16, None, 100)),
            Some(NetworkTokenBucketConfig::new(2, None, 100)),
        );
        let mut limiter = VirtioNetworkRateLimiter::new_at(config, now)
            .expect("enabled dual limiter should build");

        assert!(matches!(
            limiter.reduce_at(16, now),
            super::VirtioNetworkRateLimiterReduction::Allowed(_)
        ));
        let before_throttle = limiter.clone();

        assert_eq!(
            limiter.reduce_at(1, now),
            super::VirtioNetworkRateLimiterReduction::Throttled {
                retry_after: Duration::from_micros(6_250),
            }
        );
        assert_eq!(limiter, before_throttle);
    }

    #[test]
    fn network_rate_limiter_consumes_one_time_burst_before_steady_budget() {
        let now = Instant::now();
        let config = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(16, Some(16), 100)),
            None,
        );
        let mut limiter = VirtioNetworkRateLimiter::new_at(config, now)
            .expect("enabled bandwidth limiter should build");

        for _ in 0..2 {
            assert!(matches!(
                limiter.reduce_at(16, now),
                super::VirtioNetworkRateLimiterReduction::Allowed(_)
            ));
        }
        assert_eq!(
            limiter.reduce_at(1, now),
            super::VirtioNetworkRateLimiterReduction::Throttled {
                retry_after: Duration::from_micros(6_250),
            }
        );
    }

    #[test]
    fn network_rate_limiter_allows_one_oversized_frame_from_full_bucket() {
        let now = Instant::now();
        let config =
            NetworkRateLimiterConfig::new(Some(NetworkTokenBucketConfig::new(16, None, 100)), None);
        let mut limiter = VirtioNetworkRateLimiter::new_at(config, now)
            .expect("enabled bandwidth limiter should build");

        assert!(matches!(
            limiter.reduce_at(32, now),
            super::VirtioNetworkRateLimiterReduction::Allowed(_)
        ));
        assert_eq!(
            limiter.reduce_at(32, now),
            super::VirtioNetworkRateLimiterReduction::Throttled {
                retry_after: Duration::from_millis(100),
            }
        );
        assert!(matches!(
            limiter.reduce_at(32, now + Duration::from_millis(100)),
            super::VirtioNetworkRateLimiterReduction::Allowed(_)
        ));
    }

    #[test]
    fn network_rate_limiter_ignores_disabled_and_overflowing_buckets() {
        let now = Instant::now();
        for bucket in [
            NetworkTokenBucketConfig::new(0, Some(10), 100),
            NetworkTokenBucketConfig::new(10, Some(10), 0),
            NetworkTokenBucketConfig::new(10, Some(10), u64::MAX),
        ] {
            assert_eq!(
                VirtioNetworkRateLimiter::new_at(
                    NetworkRateLimiterConfig::new(Some(bucket), None),
                    now,
                ),
                None,
                "{bucket:?}"
            );
        }
    }

    #[test]
    fn network_dispatches_preserve_earliest_rate_limiter_retry_after() {
        let mut initial_rx = super::VirtioNetworkRxQueueDispatch::with_capacity(0)
            .expect("empty RX dispatch should allocate");
        initial_rx.record_rate_limited_packet(Duration::from_millis(80));

        let mut tx = super::VirtioNetworkTxQueueDispatch::with_capacity(0)
            .expect("empty TX dispatch should allocate");
        tx.record_rate_limited_frame(Duration::from_millis(60));
        tx.record_rate_limited_frame(Duration::from_millis(40));

        let mut post_tx_rx = super::VirtioNetworkRxQueueDispatch::with_capacity(0)
            .expect("empty post-TX RX dispatch should allocate");
        post_tx_rx.record_rate_limited_packet(Duration::from_millis(50));

        assert_eq!(tx.rate_limiter_throttled_frames(), 2);
        assert_eq!(
            tx.rate_limiter_retry_after(),
            Some(Duration::from_millis(40))
        );

        let dispatch = super::VirtioNetworkDeviceNotificationDispatch::new(
            Vec::new(),
            Some(initial_rx),
            Some(tx),
            Some(post_tx_rx),
        );
        assert_eq!(
            dispatch.rate_limiter_retry_after(),
            Some(Duration::from_millis(40))
        );

        let mut completed_rx = super::VirtioNetworkRxQueueDispatch::with_capacity(0)
            .expect("completed RX dispatch should allocate");
        completed_rx.record_rate_limited_packet(Duration::from_millis(80));
        let mut completed_tx = super::VirtioNetworkTxQueueDispatch::with_capacity(0)
            .expect("completed TX dispatch should allocate");
        completed_tx.record_rate_limited_frame(Duration::from_millis(40));
        let tx_source = VirtioNetworkTxQueueDispatchError::EmptyDescriptorChain {
            completed_dispatch: Box::new(completed_tx),
        };
        assert_eq!(
            tx_source.rate_limiter_retry_after(),
            Some(Duration::from_millis(40))
        );
        let tx_error = VirtioNetworkDeviceNotificationError::TxQueueDispatch {
            drained_notifications: Vec::new(),
            completed_rx_dispatch: Some(Box::new(completed_rx)),
            source: tx_source,
        };
        assert_eq!(
            tx_error.rate_limiter_retry_after(),
            Some(Duration::from_millis(40))
        );

        let mut completed_initial_rx = super::VirtioNetworkRxQueueDispatch::with_capacity(0)
            .expect("completed initial RX dispatch should allocate");
        completed_initial_rx.record_rate_limited_packet(Duration::from_millis(70));
        let mut completed_tx = super::VirtioNetworkTxQueueDispatch::with_capacity(0)
            .expect("completed TX dispatch should allocate");
        completed_tx.record_rate_limited_frame(Duration::from_millis(50));
        let mut completed_rx = super::VirtioNetworkRxQueueDispatch::with_capacity(0)
            .expect("completed current RX dispatch should allocate");
        completed_rx.record_rate_limited_packet(Duration::from_millis(30));
        let rx_source = VirtioNetworkRxQueueDispatchError::EmptyDescriptorChain {
            completed_dispatch: Box::new(completed_rx),
        };
        assert_eq!(
            rx_source.rate_limiter_retry_after(),
            Some(Duration::from_millis(30))
        );
        let rx_error = VirtioNetworkDeviceNotificationError::RxQueueDispatch {
            drained_notifications: Vec::new(),
            completed_tx_dispatch: Some(Box::new(completed_tx)),
            completed_initial_rx_dispatch: Some(Box::new(completed_initial_rx)),
            source: rx_source,
        };
        assert_eq!(
            rx_error.rate_limiter_retry_after(),
            Some(Duration::from_millis(30))
        );
    }

    #[test]
    fn network_device_reports_earliest_directional_retry_at_injected_time() {
        let now = Instant::now();
        let rx_rate_limiter =
            NetworkRateLimiterConfig::new(Some(NetworkTokenBucketConfig::new(16, None, 200)), None);
        let tx_rate_limiter =
            NetworkRateLimiterConfig::new(Some(NetworkTokenBucketConfig::new(16, None, 100)), None);
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler_with_rate_limiters_at(
            Some(rx_rate_limiter),
            Some(tx_rate_limiter),
            now,
        );
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x20, 0x21, 0x22, 0x23]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE).expect("RX minimum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0]);
        memory
            .write_slice(&[0xa5; 16], TEST_RX_BUFFER)
            .expect("RX buffer sentinel should write");
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, TEST_TX_PAYLOAD, &[0x10, 0x11, 0x12, 0x13]);
        for (index, descriptor) in [
            TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
            TestDescriptor::readable(TEST_TX_PAYLOAD, 4, None),
        ]
        .into_iter()
        .enumerate()
        {
            write_tx_descriptor(
                &mut memory,
                u16::try_from(index).expect("test TX descriptor index should fit"),
                descriptor,
            );
        }
        write_tx_available_heads(&mut memory, &[0]);

        {
            let limiter = handler
                .activation_handler_mut()
                .rx_rate_limiter
                .as_mut()
                .expect("RX limiter should exist");
            assert!(matches!(
                limiter.reduce_at(16, now),
                super::VirtioNetworkRateLimiterReduction::Allowed(_)
            ));
        }
        {
            let limiter = handler
                .activation_handler_mut()
                .tx_rate_limiter
                .as_mut()
                .expect("TX limiter should exist");
            assert!(matches!(
                limiter.reduce_at(16, now),
                super::VirtioNetworkRateLimiterReduction::Allowed(_)
            ));
        }
        let exhausted_rx = handler
            .activation_handler()
            .rx_rate_limiter()
            .expect("RX limiter should exist")
            .clone();
        let exhausted_tx = handler
            .activation_handler()
            .tx_rate_limiter()
            .expect("TX limiter should exist")
            .clone();

        let first = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                vec![VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_QUEUE_INDEX],
                &mut sink,
                &mut source,
                now,
            )
            .expect("both throttled directions should dispatch");
        assert_eq!(
            first
                .rx_queue_dispatch()
                .expect("RX dispatch should be present")
                .rate_limiter_retry_after(),
            Some(Duration::from_millis(200))
        );
        assert_eq!(
            first
                .tx_queue_dispatch()
                .expect("TX dispatch should be present")
                .rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            first.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(source.consume_calls, 0);
        assert_eq!(sink.calls, 0);
        assert_eq!(read_rx_used_index(&memory), 0);
        assert_eq!(read_tx_used_index(&memory), 0);
        assert_eq!(read_guest_bytes(&memory, TEST_RX_BUFFER, 16), [0xa5; 16]);
        assert_eq!(
            handler
                .activation_handler()
                .rx_rate_limiter()
                .expect("RX limiter should exist"),
            &exhausted_rx
        );
        assert_eq!(
            handler
                .activation_handler()
                .tx_rate_limiter()
                .expect("TX limiter should exist"),
            &exhausted_tx
        );

        let repeated = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                Vec::new(),
                &mut sink,
                &mut source,
                now + Duration::from_millis(99),
            )
            .expect("both directions should remain retryable before the first boundary");
        assert_eq!(
            repeated
                .rx_queue_dispatch()
                .expect("RX retry dispatch should be present")
                .rate_limiter_retry_after(),
            Some(Duration::from_millis(101))
        );
        assert_eq!(
            repeated
                .tx_queue_dispatch()
                .expect("TX retry dispatch should be present")
                .rate_limiter_retry_after(),
            Some(Duration::from_millis(1))
        );
        assert_eq!(
            repeated.rate_limiter_retry_after(),
            Some(Duration::from_millis(1))
        );
        assert_eq!(source.consume_calls, 0);
        assert_eq!(sink.calls, 0);
        assert_eq!(read_rx_used_index(&memory), 0);
        assert_eq!(read_tx_used_index(&memory), 0);
        assert_eq!(
            handler
                .activation_handler()
                .rx_rate_limiter()
                .expect("RX limiter should exist"),
            &exhausted_rx
        );
        assert_eq!(
            handler
                .activation_handler()
                .tx_rate_limiter()
                .expect("TX limiter should exist"),
            &exhausted_tx
        );

        let tx_boundary = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                Vec::new(),
                &mut sink,
                &mut source,
                now + Duration::from_millis(100),
            )
            .expect("TX should progress at its exact refill boundary");
        assert_eq!(
            tx_boundary
                .rx_queue_dispatch()
                .expect("RX boundary dispatch should be present")
                .rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            tx_boundary
                .tx_queue_dispatch()
                .expect("TX boundary dispatch should be present")
                .rate_limiter_retry_after(),
            None
        );
        assert_eq!(
            tx_boundary.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(source.consume_calls, 0);
        assert_eq!(sink.calls, 1);
        assert_eq!(read_rx_used_index(&memory), 0);
        assert_eq!(read_tx_used_index(&memory), 1);

        let rx_boundary = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                Vec::new(),
                &mut sink,
                &mut source,
                now + Duration::from_millis(200),
            )
            .expect("RX should progress at its exact refill boundary");
        assert_eq!(rx_boundary.rate_limiter_retry_after(), None);
        assert_eq!(source.consume_calls, 1);
        assert_eq!(sink.calls, 1);
        assert_eq!(read_rx_used_index(&memory), 1);
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_BUFFER
                    .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                    .expect("RX payload address should not overflow"),
                4,
            ),
            [0x20, 0x21, 0x22, 0x23]
        );
        assert!(!handler.has_pending_network_queue_work());
    }

    #[test]
    fn network_interface_config_input_exposes_firecracker_shape() {
        let rx_rate_limiter = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(1024, Some(2048), 100)),
            None,
        );
        let tx_rate_limiter = NetworkRateLimiterConfig::new(
            None,
            Some(NetworkTokenBucketConfig::new(10, None, 1000)),
        );
        let input = input()
            .with_guest_mac("12:34:56:78:9a:bc")
            .with_mtu(1500)
            .with_rx_rate_limiter(rx_rate_limiter)
            .with_tx_rate_limiter(tx_rate_limiter);

        assert_eq!(input.path_iface_id(), "eth0");
        assert_eq!(input.body_iface_id(), "eth0");
        assert_eq!(input.host_dev_name(), "tap0");
        assert_eq!(input.guest_mac(), Some("12:34:56:78:9a:bc"));
        assert_eq!(input.mtu(), Some(1500));
        assert_eq!(input.rx_rate_limiter(), Some(rx_rate_limiter));
        assert_eq!(input.tx_rate_limiter(), Some(tx_rate_limiter));
    }

    #[test]
    fn network_interface_update_input_exposes_firecracker_shape() {
        let rx_rate_limiter = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(1024, Some(2048), 100)),
            None,
        );
        let tx_rate_limiter = NetworkRateLimiterConfig::new(
            None,
            Some(NetworkTokenBucketConfig::new(10, None, 1000)),
        );
        let input = NetworkInterfaceUpdateInput::new("eth0", "eth0")
            .with_rx_rate_limiter(rx_rate_limiter)
            .with_tx_rate_limiter(tx_rate_limiter);

        assert_eq!(input.path_iface_id(), "eth0");
        assert_eq!(input.body_iface_id(), "eth0");
        assert_eq!(input.rx_rate_limiter(), Some(rx_rate_limiter));
        assert_eq!(input.tx_rate_limiter(), Some(tx_rate_limiter));
    }

    #[test]
    fn network_interface_update_validates_ids_and_rate_limiters() {
        let update = NetworkInterfaceUpdateInput::new("eth0", "eth0")
            .validate()
            .expect("matching no-op update should validate");
        assert_eq!(update.iface_id(), "eth0");
        assert!(update.is_noop());
        let empty_update = NetworkInterfaceUpdateInput::new("eth0", "eth0")
            .with_rx_rate_limiter(NetworkRateLimiterConfig::new(None, None))
            .with_tx_rate_limiter(NetworkRateLimiterConfig::new(None, None))
            .validate()
            .expect("empty limiter objects should validate as a no-op");
        assert!(empty_update.is_noop());

        assert_eq!(
            NetworkInterfaceUpdateInput::new("", "").validate(),
            Err(NetworkInterfaceUpdateError::EmptyInterfaceId {
                source: InterfaceIdSource::Path,
            })
        );
        assert_eq!(
            NetworkInterfaceUpdateInput::new("eth-0", "eth-0").validate(),
            Err(NetworkInterfaceUpdateError::InvalidInterfaceId {
                source: InterfaceIdSource::Path,
                iface_id: "eth-0".to_string(),
            })
        );
        assert_eq!(
            NetworkInterfaceUpdateInput::new("eth0", "eth1").validate(),
            Err(NetworkInterfaceUpdateError::MismatchedInterfaceId {
                path_iface_id: "eth0".to_string(),
                body_iface_id: "eth1".to_string(),
            })
        );
        let rx_rate_limiter = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(1024, None, 100)),
            None,
        );
        let update = NetworkInterfaceUpdateInput::new("eth0", "eth0")
            .with_rx_rate_limiter(rx_rate_limiter)
            .validate()
            .expect("configured runtime limiter update should validate");
        assert_eq!(update.rx_rate_limiter(), Some(rx_rate_limiter));
        assert_eq!(update.tx_rate_limiter(), None);
        assert!(!update.is_noop());
    }

    #[test]
    fn network_interface_config_errors_display_without_sources() {
        let err = NetworkInterfaceConfigError::EmptyHostDeviceName;

        assert_eq!(err.to_string(), "network host_dev_name must not be empty");
        assert!(std::error::Error::source(&err).is_none());

        let err = NetworkInterfaceConfigError::HostNetworkNotAuthorized;
        assert_eq!(err.to_string(), "system host networking is not authorized");
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn network_interface_update_errors_display_without_sources() {
        let err = NetworkInterfaceUpdateError::ActiveSessionCommand {
            message: "boot run loop command queue is full".to_string(),
        };

        assert_eq!(
            err.to_string(),
            "active network interface update command failed: boot run loop command queue is full"
        );
        assert!(std::error::Error::source(&err).is_none());

        let err = NetworkInterfaceUpdateError::UnknownInterface {
            iface_id: "eth9".to_string(),
        };
        assert_eq!(err.to_string(), "network interface is not configured");
        assert!(std::error::Error::source(&err).is_none());
        assert!(!err.to_string().contains("eth9"));
    }

    #[test]
    fn network_interface_config_errors_display_mtu_bounds() {
        let err = NetworkInterfaceConfigError::InvalidMtu { mtu: 67 };

        assert_eq!(
            err.to_string(),
            "network mtu 67 is out of range [68, 65535]"
        );
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
    fn runtime_network_insert_prepares_without_mutation_and_commits_once_live() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input().with_guest_mac("12:34:56:78:9a:bc"))
            .expect("initial interface should be stored");

        let prepared = configs
            .prepare_runtime_insert(
                NetworkInterfaceConfigInput::new("eth1", "eth1", "vmnet:shared")
                    .with_guest_mac("12:34:56:78:9a:bd"),
            )
            .expect("runtime interface should prepare");
        assert_eq!(prepared.config().iface_id(), "eth1");
        assert_eq!(configs.as_slice().len(), 1);

        configs.commit_runtime_insert(prepared);
        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].iface_id(), "eth0");
        assert_eq!(configs.as_slice()[1].iface_id(), "eth1");
    }

    #[test]
    fn runtime_network_insert_rejects_duplicate_identity_without_mutation() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input().with_guest_mac("12:34:56:78:9a:bc"))
            .expect("initial interface should be stored");

        assert!(matches!(
            configs.prepare_runtime_insert(NetworkInterfaceConfigInput::new(
                "eth0",
                "eth0",
                "vmnet:host",
            )),
            Err(NetworkRuntimeMutationError::DuplicateInterface { iface_id })
                if iface_id == "eth0"
        ));
        assert!(matches!(
            configs.prepare_runtime_insert(
                NetworkInterfaceConfigInput::new("eth1", "eth1", "vmnet:host")
                    .with_guest_mac("12:34:56:78:9a:bc"),
            ),
            Err(NetworkRuntimeMutationError::InvalidConfig(
                NetworkInterfaceConfigError::GuestMacAddressInUse { .. }
            ))
        ));
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].host_dev_name(), "tap0");
    }

    #[test]
    fn runtime_network_insert_enforces_capacity_without_mutation() {
        let mut configs = NetworkInterfaceConfigs::new();
        for index in 0..MAX_NETWORK_INTERFACE_COUNT {
            configs
                .insert(NetworkInterfaceConfigInput::new(
                    format!("eth{index}"),
                    format!("eth{index}"),
                    "vmnet:shared",
                ))
                .expect("interface within the limit should insert");
        }

        assert!(matches!(
            configs.prepare_runtime_insert(NetworkInterfaceConfigInput::new(
                "overflow",
                "overflow",
                "vmnet:shared",
            )),
            Err(NetworkRuntimeMutationError::InvalidConfig(
                NetworkInterfaceConfigError::TooManyNetworkInterfaces { count, max }
            )) if count == MAX_NETWORK_INTERFACE_COUNT + 1 && max == MAX_NETWORK_INTERFACE_COUNT
        ));
        assert_eq!(configs.as_slice().len(), MAX_NETWORK_INTERFACE_COUNT);
    }

    #[test]
    fn runtime_network_removal_prepares_without_mutation_and_preserves_peer_order() {
        let mut configs = NetworkInterfaceConfigs::new();
        for iface_id in ["eth0", "eth1", "eth2"] {
            configs
                .insert(NetworkInterfaceConfigInput::new(
                    iface_id,
                    iface_id,
                    "vmnet:shared",
                ))
                .expect("interface should insert");
        }

        let prepared = configs
            .prepare_runtime_removal("eth1")
            .expect("existing interface should prepare for removal");
        assert_eq!(prepared.iface_id(), "eth1");
        assert_eq!(configs.as_slice().len(), 3);

        configs.commit_runtime_removal(prepared);
        assert_eq!(
            configs
                .as_slice()
                .iter()
                .map(NetworkInterfaceConfig::iface_id)
                .collect::<Vec<_>>(),
            ["eth0", "eth2"]
        );
        assert!(matches!(
            configs.prepare_runtime_removal("missing"),
            Err(NetworkRuntimeMutationError::UnknownInterface { iface_id })
                if iface_id == "missing"
        ));
        assert!(matches!(
            configs.prepare_runtime_removal("eth-2"),
            Err(NetworkRuntimeMutationError::InvalidInterfaceId { iface_id })
                if iface_id == "eth-2"
        ));
    }

    #[test]
    fn runtime_network_errors_redact_identity_and_host_details() {
        for error in [
            NetworkRuntimeMutationError::DuplicateInterface {
                iface_id: "private_iface".to_string(),
            },
            NetworkRuntimeMutationError::UnknownInterface {
                iface_id: "private_missing".to_string(),
            },
            NetworkRuntimeMutationError::HostNetworkNotAuthorized,
        ] {
            let message = error.to_string();
            assert!(!message.contains("private_"));
            assert!(std::error::Error::source(&error).is_none());
        }
    }

    #[test]
    fn network_interface_configs_validate_runtime_update_existence() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input())
            .expect("interface config should be stored");

        let update = configs
            .validate_update(NetworkInterfaceUpdateInput::new("eth0", "eth0"))
            .expect("existing no-op update should validate");
        assert_eq!(update.iface_id(), "eth0");

        assert_eq!(
            configs.validate_update(NetworkInterfaceUpdateInput::new("eth9", "eth9")),
            Err(NetworkInterfaceUpdateError::UnknownInterface {
                iface_id: "eth9".to_string(),
            })
        );
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].iface_id(), "eth0");
    }

    #[test]
    fn network_interface_configs_prepare_partial_limiter_update_without_mutating() {
        let rx_bandwidth = NetworkTokenBucketConfig::new(1024, Some(2048), 100);
        let rx_ops = NetworkTokenBucketConfig::new(10, None, 1000);
        let tx_bandwidth = NetworkTokenBucketConfig::new(4096, None, 200);
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(
                input()
                    .with_rx_rate_limiter(NetworkRateLimiterConfig::new(
                        Some(rx_bandwidth),
                        Some(rx_ops),
                    ))
                    .with_tx_rate_limiter(NetworkRateLimiterConfig::new(Some(tx_bandwidth), None)),
            )
            .expect("initial rate-limited interface should be stored");
        configs
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1"))
            .expect("second interface should be stored");
        let updated_ops = NetworkTokenBucketConfig::new(20, Some(30), 2000);

        let (update, candidate) = configs
            .prepare_update(
                NetworkInterfaceUpdateInput::new("eth0", "eth0")
                    .with_rx_rate_limiter(NetworkRateLimiterConfig::new(None, Some(updated_ops))),
            )
            .expect("partial network limiter update should prepare");

        assert_eq!(update.iface_id(), "eth0");
        assert_eq!(
            candidate.rx_rate_limiter(),
            Some(NetworkRateLimiterConfig::new(
                Some(rx_bandwidth),
                Some(updated_ops),
            ))
        );
        assert_eq!(
            candidate.tx_rate_limiter(),
            Some(NetworkRateLimiterConfig::new(Some(tx_bandwidth), None,))
        );
        assert_eq!(
            configs.as_slice()[0].rx_rate_limiter(),
            Some(NetworkRateLimiterConfig::new(
                Some(rx_bandwidth),
                Some(rx_ops),
            )),
            "candidate preparation must not mutate stored config"
        );

        configs
            .commit_update(candidate)
            .expect("prepared network update should commit");
        assert_eq!(configs.as_slice()[0].iface_id(), "eth0");
        assert_eq!(configs.as_slice()[1].iface_id(), "eth1");
        assert_eq!(
            configs.as_slice()[0].rx_rate_limiter(),
            Some(NetworkRateLimiterConfig::new(
                Some(rx_bandwidth),
                Some(updated_ops),
            ))
        );
    }

    #[test]
    fn network_interface_configs_clear_only_explicitly_disabled_bucket() {
        let bandwidth = NetworkTokenBucketConfig::new(1024, Some(2048), 100);
        let ops = NetworkTokenBucketConfig::new(10, None, 1000);
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(
                input().with_rx_rate_limiter(NetworkRateLimiterConfig::new(
                    Some(bandwidth),
                    Some(ops),
                )),
            )
            .expect("initial rate-limited interface should be stored");

        let (_, candidate) = configs
            .prepare_update(
                NetworkInterfaceUpdateInput::new("eth0", "eth0").with_rx_rate_limiter(
                    NetworkRateLimiterConfig::new(
                        Some(NetworkTokenBucketConfig::new(0, None, 100)),
                        None,
                    ),
                ),
            )
            .expect("disabled bucket update should prepare");

        assert_eq!(
            candidate.rx_rate_limiter(),
            Some(NetworkRateLimiterConfig::new(None, Some(ops)))
        );
        assert_eq!(
            configs.as_slice()[0].rx_rate_limiter(),
            Some(NetworkRateLimiterConfig::new(Some(bandwidth), Some(ops),))
        );
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
    fn network_interface_configs_accept_exact_interface_count_limit() {
        let mut configs = NetworkInterfaceConfigs::new();

        for index in 0..MAX_NETWORK_INTERFACE_COUNT {
            configs
                .insert(numbered_input(index))
                .expect("interface within limit should insert");
        }

        assert_eq!(configs.as_slice().len(), MAX_NETWORK_INTERFACE_COUNT);
    }

    #[test]
    fn network_interface_configs_reject_one_over_limit_without_mutating() {
        let mut configs = NetworkInterfaceConfigs::new();

        for index in 0..MAX_NETWORK_INTERFACE_COUNT {
            configs
                .insert(numbered_input(index))
                .expect("interface within limit should insert");
        }

        let err = configs
            .insert(numbered_input(MAX_NETWORK_INTERFACE_COUNT))
            .expect_err("one-over interface should fail");

        assert_eq!(
            err,
            NetworkInterfaceConfigError::TooManyNetworkInterfaces {
                count: MAX_NETWORK_INTERFACE_COUNT + 1,
                max: MAX_NETWORK_INTERFACE_COUNT,
            }
        );
        assert_eq!(
            err.to_string(),
            format!("network interface count exceeds maximum {MAX_NETWORK_INTERFACE_COUNT}")
        );
        assert_eq!(configs.as_slice().len(), MAX_NETWORK_INTERFACE_COUNT);
        assert_eq!(
            configs.as_slice()[MAX_NETWORK_INTERFACE_COUNT - 1].iface_id(),
            format!("eth{}", MAX_NETWORK_INTERFACE_COUNT - 1)
        );
    }

    #[test]
    fn network_interface_configs_reject_invalid_mtu_without_mutating() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input().with_mtu(1500))
            .expect("initial network config should be stored");

        let err = configs
            .insert(input().with_mtu(VIRTIO_NET_MIN_MTU - 1))
            .expect_err("invalid replacement MTU should fail");

        assert_eq!(
            err,
            NetworkInterfaceConfigError::InvalidMtu {
                mtu: VIRTIO_NET_MIN_MTU - 1,
            }
        );
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].iface_id(), "eth0");
        assert_eq!(configs.as_slice()[0].mtu(), Some(1500));
    }

    #[test]
    fn network_interface_configs_replace_existing_interface_at_limit() {
        let mut configs = NetworkInterfaceConfigs::new();

        for index in 0..MAX_NETWORK_INTERFACE_COUNT {
            configs
                .insert(numbered_input(index))
                .expect("interface within limit should insert");
        }

        configs
            .insert(NetworkInterfaceConfigInput::new(
                "eth0",
                "eth0",
                "replacement",
            ))
            .expect("replacement at limit should insert");

        assert_eq!(configs.as_slice().len(), MAX_NETWORK_INTERFACE_COUNT);
        assert_eq!(
            configs.as_slice()[MAX_NETWORK_INTERFACE_COUNT - 1].iface_id(),
            "eth0"
        );
        assert_eq!(
            configs.as_slice()[MAX_NETWORK_INTERFACE_COUNT - 1].host_dev_name(),
            "replacement"
        );
    }

    #[test]
    fn prepared_network_devices_accept_empty_configs() {
        let configs = NetworkInterfaceConfigs::new();
        let devices =
            PreparedNetworkDevices::from_configs(&configs).expect("empty configs should prepare");

        assert!(devices.is_empty());
        assert_eq!(devices.len(), 0);
        assert!(devices.as_slice().is_empty());
        assert!(devices.into_vec().is_empty());
    }

    #[test]
    fn prepared_network_devices_prepare_interface_without_guest_mac() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input())
            .expect("network config should be stored");

        let devices =
            PreparedNetworkDevices::from_configs(&configs).expect("network device should prepare");
        let device = devices
            .as_slice()
            .first()
            .expect("prepared network device should exist");
        let base_features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_INDIRECT_DESC)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX)
            | VirtioNetworkFeatureCapabilities::complete_software().feature_bits();

        assert_eq!(devices.len(), 1);
        assert_eq!(device.iface_id(), "eth0");
        assert_eq!(device.host_dev_name(), "tap0");
        assert_eq!(device.config_space().guest_mac(), None);
        assert_eq!(device.config_space().mtu(), None);
        assert_eq!(device.config_space().available_features(), base_features);
        assert!(!device.device().is_activated());
    }

    #[test]
    fn prepared_network_devices_prepare_interface_with_guest_mac() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input().with_guest_mac("12:34:56:78:9a:bc"))
            .expect("network config should be stored");

        let devices =
            PreparedNetworkDevices::from_configs(&configs).expect("network device should prepare");
        let device = devices
            .as_slice()
            .first()
            .expect("prepared network device should exist");

        assert_eq!(device.config_space().guest_mac(), Some(test_guest_mac()));
        assert_eq!(
            device.config_space().available_features(),
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
                | virtio_feature_bit(VIRTIO_RING_FEATURE_INDIRECT_DESC)
                | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX)
                | VirtioNetworkFeatureCapabilities::complete_software().feature_bits()
                | virtio_feature_bit(VIRTIO_NET_F_MAC)
        );
        assert!(!device.device().is_activated());
    }

    #[test]
    fn prepared_network_devices_prepare_interface_with_mtu() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input().with_mtu(1500))
            .expect("network config should be stored");

        let devices =
            PreparedNetworkDevices::from_configs(&configs).expect("network device should prepare");
        let device = devices
            .as_slice()
            .first()
            .expect("prepared network device should exist");

        assert_eq!(device.config_space().guest_mac(), None);
        assert_eq!(device.config_space().mtu(), Some(1500));
        assert_eq!(
            device.config_space().available_features(),
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
                | virtio_feature_bit(VIRTIO_RING_FEATURE_INDIRECT_DESC)
                | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX)
                | VirtioNetworkFeatureCapabilities::complete_software().feature_bits()
                | virtio_feature_bit(VIRTIO_NET_F_MTU)
        );
        assert!(!device.device().is_activated());
    }

    #[test]
    fn prepared_network_device_profile_keeps_requested_config_immutable() {
        let requested_mac = test_guest_mac();
        let realized_mac = GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 0x88]);
        let config = NetworkInterfaceConfigInput::new("eth0", "eth0", "vmnet:shared")
            .with_guest_mac(requested_mac.to_string())
            .with_mtu(1500)
            .validate()
            .expect("requested network config should validate");
        let profile = NetworkDeviceProfile::new(Some(realized_mac), Some(1500));

        let device = super::PreparedNetworkDevice::from_config_with_profile(&config, profile);

        assert_eq!(config.guest_mac(), Some(requested_mac));
        assert_eq!(config.mtu(), Some(1500));
        assert_eq!(device.config_space().guest_mac(), Some(realized_mac));
        assert_eq!(device.config_space().mtu(), Some(1500));
        assert_eq!(profile.guest_mac(), Some(realized_mac));
        assert_eq!(profile.mtu(), Some(1500));
        let debug = format!("{profile:?} {device:?}");
        assert!(debug.contains("<configured>"));
        assert!(!debug.contains(&requested_mac.to_string()));
        assert!(!debug.contains(&realized_mac.to_string()));
        assert!(!debug.contains("vmnet:shared"));
    }

    #[test]
    fn prepared_network_devices_consume_exact_profiles_in_configuration_order() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new(
                "eth0",
                "eth0",
                "vmnet:shared",
            ))
            .expect("first network config should validate");
        configs
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "vmnet:host").with_mtu(1400))
            .expect("second network config should validate");
        let first_mac = GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 1]);
        let second_mac = GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 2]);
        let profiles = BTreeMap::from([
            (
                "eth1".to_owned(),
                NetworkDeviceProfile::new(Some(second_mac), Some(1400)),
            ),
            (
                "eth0".to_owned(),
                NetworkDeviceProfile::new(Some(first_mac), None),
            ),
        ]);

        let devices =
            PreparedNetworkDevices::from_config_slice_with_profiles(configs.as_slice(), profiles)
                .expect("exact profile map should prepare");

        assert_eq!(devices.as_slice()[0].iface_id(), "eth0");
        assert_eq!(
            devices.as_slice()[0].config_space().guest_mac(),
            Some(first_mac)
        );
        assert_eq!(devices.as_slice()[0].config_space().mtu(), None);
        assert_eq!(devices.as_slice()[1].iface_id(), "eth1");
        assert_eq!(
            devices.as_slice()[1].config_space().guest_mac(),
            Some(second_mac)
        );
        assert_eq!(devices.as_slice()[1].config_space().mtu(), Some(1400));
    }

    #[test]
    fn prepared_network_devices_reject_missing_and_unexpected_profiles() {
        let config = NetworkInterfaceConfigInput::new("eth0", "eth0", "vmnet:shared")
            .validate()
            .expect("network config should validate");
        let configs = [config];

        let missing =
            PreparedNetworkDevices::from_config_slice_with_profiles(&configs, BTreeMap::new())
                .expect_err("missing realized profile should fail");
        assert!(matches!(
            missing,
            PreparedNetworkDeviceError::MissingProfile
        ));

        let profiles = BTreeMap::from([
            ("eth0".to_owned(), NetworkDeviceProfile::new(None, None)),
            ("eth1".to_owned(), NetworkDeviceProfile::new(None, None)),
        ]);
        let unexpected =
            PreparedNetworkDevices::from_config_slice_with_profiles(&configs, profiles)
                .expect_err("unexpected realized profile should fail");
        assert!(matches!(
            unexpected,
            PreparedNetworkDeviceError::UnexpectedProfile
        ));
    }

    #[test]
    fn prepared_network_devices_build_independent_directional_rate_limiters() {
        let rx_rate_limiter = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(1024, Some(2048), 100)),
            None,
        );
        let tx_rate_limiter = NetworkRateLimiterConfig::new(
            None,
            Some(NetworkTokenBucketConfig::new(10, None, 1000)),
        );
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(
                input()
                    .with_rx_rate_limiter(rx_rate_limiter)
                    .with_tx_rate_limiter(tx_rate_limiter),
            )
            .expect("rate-limited network config should be stored");

        let devices =
            PreparedNetworkDevices::from_configs(&configs).expect("network device should prepare");
        let device = devices
            .as_slice()
            .first()
            .expect("prepared network device should exist")
            .device();

        assert!(device.rx_rate_limiter().is_some());
        assert!(device.tx_rate_limiter().is_some());
        assert_ne!(device.rx_rate_limiter(), device.tx_rate_limiter());
    }

    #[test]
    fn network_rate_limiter_state_is_isolated_by_direction_and_device() {
        let now = Instant::now();
        let config =
            NetworkRateLimiterConfig::new(None, Some(NetworkTokenBucketConfig::new(1, None, 100)));
        let mut first = VirtioNetworkDevice::with_rate_limiters_at(Some(config), Some(config), now);
        let second = VirtioNetworkDevice::with_rate_limiters_at(Some(config), Some(config), now);
        let initial = second
            .tx_rate_limiter()
            .expect("second TX limiter should exist")
            .clone();

        assert!(matches!(
            first
                .tx_rate_limiter
                .as_mut()
                .expect("first TX limiter should exist")
                .reduce_at(1, now),
            super::VirtioNetworkRateLimiterReduction::Allowed(_)
        ));

        assert_eq!(
            first
                .rx_rate_limiter()
                .expect("first RX limiter should exist"),
            &initial
        );
        assert_eq!(
            second
                .tx_rate_limiter()
                .expect("second TX limiter should exist"),
            &initial
        );
    }

    #[test]
    fn virtio_network_feature_capabilities_support_independent_safe_downgrades() {
        let complete = VirtioNetworkFeatureCapabilities::complete_software();
        let downgraded = [
            (
                VIRTIO_NET_F_CSUM,
                complete
                    .with_checksum(false)
                    .with_host_tso4(false)
                    .with_host_tso6(false)
                    .with_host_ufo(false),
            ),
            (
                VIRTIO_NET_F_GUEST_CSUM,
                complete
                    .with_guest_checksum(false)
                    .with_guest_tso4(false)
                    .with_guest_tso6(false)
                    .with_guest_ufo(false),
            ),
            (VIRTIO_NET_F_GUEST_TSO4, complete.with_guest_tso4(false)),
            (VIRTIO_NET_F_GUEST_TSO6, complete.with_guest_tso6(false)),
            (VIRTIO_NET_F_GUEST_UFO, complete.with_guest_ufo(false)),
            (VIRTIO_NET_F_HOST_TSO4, complete.with_host_tso4(false)),
            (VIRTIO_NET_F_HOST_TSO6, complete.with_host_tso6(false)),
            (VIRTIO_NET_F_HOST_UFO, complete.with_host_ufo(false)),
            (
                VIRTIO_NET_F_MRG_RXBUF,
                complete.with_merged_rx_buffers(false),
            ),
        ];

        for (disabled_feature, capabilities) in downgraded {
            assert!(capabilities.is_dependency_complete());
            assert!(!capabilities.supports(disabled_feature));
            let config =
                VirtioNetworkConfigSpace::with_feature_capabilities(None, None, capabilities);
            assert_eq!(
                config.available_features() & virtio_feature_bit(disabled_feature),
                0
            );
        }
        assert!(complete.checksum());
        assert!(complete.guest_checksum());
        assert!(complete.guest_tso4());
        assert!(complete.guest_tso6());
        assert!(complete.guest_ufo());
        assert!(complete.host_tso4());
        assert!(complete.host_tso6());
        assert!(complete.host_ufo());
        assert!(complete.merged_rx_buffers());
    }

    #[test]
    fn virtio_network_feature_capabilities_safely_downgrade_incomplete_dependencies() {
        let missing_checksum = VirtioNetworkFeatureCapabilities::none().with_host_tso4(true);
        let missing_guest_checksum = VirtioNetworkFeatureCapabilities::none().with_guest_ufo(true);

        assert!(!missing_checksum.is_dependency_complete());
        assert!(!missing_guest_checksum.is_dependency_complete());
        assert_eq!(
            VirtioNetworkConfigSpace::with_feature_capabilities(None, None, missing_checksum)
                .available_features()
                & virtio_feature_bit(VIRTIO_NET_F_HOST_TSO4),
            0
        );
        let profile = NetworkDeviceProfile::new(Some(test_guest_mac()), None)
            .with_feature_capabilities(missing_guest_checksum);
        assert_eq!(
            VirtioNetworkConfigSpace::with_feature_capabilities(
                profile.guest_mac(),
                profile.mtu(),
                profile.feature_capabilities(),
            )
            .available_features()
                & virtio_feature_bit(VIRTIO_NET_F_GUEST_UFO),
            0
        );
    }

    #[test]
    fn network_device_profile_freezes_envelope_and_feature_matrix() {
        let capabilities = VirtioNetworkFeatureCapabilities::complete_software()
            .with_host_ufo(false)
            .with_guest_ufo(false);
        let profile = NetworkDeviceProfile::new(Some(test_guest_mac()), None)
            .with_packet_envelope(VirtioNetworkPacketEnvelope::DirectVirtioHeader)
            .with_feature_capabilities(capabilities);

        assert_eq!(
            profile.packet_envelope(),
            VirtioNetworkPacketEnvelope::DirectVirtioHeader
        );
        assert_eq!(profile.feature_capabilities(), capabilities);
        assert!(
            !profile
                .feature_capabilities()
                .supports(VIRTIO_NET_F_HOST_UFO)
        );
        assert!(
            !profile
                .feature_capabilities()
                .supports(VIRTIO_NET_F_GUEST_UFO)
        );
    }

    #[test]
    fn network_rate_limiter_update_preserves_omitted_live_budget_and_queue_state() {
        let initial_time = Instant::now();
        let update_time = initial_time + Duration::from_millis(25);
        let rx_bandwidth = NetworkTokenBucketConfig::new(64, Some(16), 100);
        let rx_ops = NetworkTokenBucketConfig::new(4, None, 100);
        let tx_bandwidth = NetworkTokenBucketConfig::new(128, None, 200);
        let replacement_rx_ops = NetworkTokenBucketConfig::new(8, Some(2), 250);
        let mut device = VirtioNetworkDevice::with_rate_limiters_at(
            Some(NetworkRateLimiterConfig::new(
                Some(rx_bandwidth),
                Some(rx_ops),
            )),
            Some(NetworkRateLimiterConfig::new(Some(tx_bandwidth), None)),
            initial_time,
        );
        let registers = network_device_registers();
        let queues =
            configured_network_queues(Some(TEST_QUEUE_SIZE), true, Some(TEST_QUEUE_SIZE), true);
        device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect("network device should activate");
        assert!(matches!(
            device
                .rx_rate_limiter
                .as_mut()
                .expect("RX limiter should exist")
                .reduce_at(32, initial_time),
            super::VirtioNetworkRateLimiterReduction::Allowed(_)
        ));
        device.pending_rate_limited_rx_queue = true;
        device.pending_rate_limited_tx_queue = true;
        let rx_queue_before = device.active_rx_queue();
        let tx_queue_before = device.active_tx_queue();
        let rx_bandwidth_before = device
            .rx_rate_limiter()
            .expect("RX limiter should exist")
            .bandwidth
            .clone();
        let tx_limiter_before = device.tx_rate_limiter().cloned();
        let update = NetworkInterfaceUpdateInput::new("eth0", "eth0")
            .with_rx_rate_limiter(NetworkRateLimiterConfig::new(
                None,
                Some(replacement_rx_ops),
            ))
            .validate()
            .expect("network limiter update should validate");

        device.update_rate_limiters_at(&update, update_time);

        let updated_rx = device
            .rx_rate_limiter()
            .expect("updated RX limiter should exist");
        assert_eq!(
            updated_rx.bandwidth, rx_bandwidth_before,
            "omitted bucket must preserve its exact live budget"
        );
        let expected_replacement = VirtioNetworkRateLimiter::new_at(
            NetworkRateLimiterConfig::new(None, Some(replacement_rx_ops)),
            update_time,
        )
        .expect("replacement ops limiter should build");
        assert_eq!(updated_rx.ops, expected_replacement.ops);
        assert_eq!(device.tx_rate_limiter(), tx_limiter_before.as_ref());
        assert_eq!(device.active_rx_queue(), rx_queue_before);
        assert_eq!(device.active_tx_queue(), tx_queue_before);
        assert!(device.has_pending_rate_limited_rx_queue());
        assert!(device.has_pending_rate_limited_tx_queue());
    }

    #[test]
    fn network_rate_limiter_update_clears_only_explicitly_disabled_bucket() {
        let now = Instant::now();
        let bandwidth = NetworkTokenBucketConfig::new(64, Some(16), 100);
        let ops = NetworkTokenBucketConfig::new(4, None, 100);
        let mut device = VirtioNetworkDevice::with_rate_limiters_at(
            Some(NetworkRateLimiterConfig::new(Some(bandwidth), Some(ops))),
            None,
            now,
        );
        let original_ops = device
            .rx_rate_limiter()
            .expect("RX limiter should exist")
            .ops
            .clone();
        let clear_bandwidth = NetworkInterfaceUpdateInput::new("eth0", "eth0")
            .with_rx_rate_limiter(NetworkRateLimiterConfig::new(
                Some(NetworkTokenBucketConfig::new(0, Some(999), 100)),
                None,
            ))
            .validate()
            .expect("disabled bandwidth update should validate");

        device.update_rate_limiters_at(&clear_bandwidth, now);

        let rx = device
            .rx_rate_limiter()
            .expect("ops bucket should keep direction configured");
        assert!(rx.bandwidth.is_none());
        assert_eq!(rx.ops, original_ops);

        let clear_ops = NetworkInterfaceUpdateInput::new("eth0", "eth0")
            .with_rx_rate_limiter(NetworkRateLimiterConfig::new(
                None,
                Some(NetworkTokenBucketConfig::new(4, None, 0)),
            ))
            .validate()
            .expect("disabled ops update should validate");
        device.update_rate_limiters_at(&clear_ops, now);

        assert!(device.rx_rate_limiter().is_none());
    }

    #[test]
    fn network_mmio_rate_limiter_update_does_not_signal_config_change() {
        let now = Instant::now();
        let mut handler = network_activation_handler_with_rate_limiters_at(
            Some(NetworkRateLimiterConfig::new(
                Some(NetworkTokenBucketConfig::new(64, None, 100)),
                None,
            )),
            None,
            now,
        );
        let update = NetworkInterfaceUpdateInput::new("eth0", "eth0")
            .with_tx_rate_limiter(NetworkRateLimiterConfig::new(
                None,
                Some(NetworkTokenBucketConfig::new(8, None, 250)),
            ))
            .validate()
            .expect("network limiter update should validate");
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );
        assert_eq!(read_interrupt_status(&handler), 0);

        handler.update_network_rate_limiters(&update);

        assert!(handler.activation_handler().rx_rate_limiter().is_some());
        assert!(handler.activation_handler().tx_rate_limiter().is_some());
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );
        assert_eq!(read_interrupt_status(&handler), 0);
    }

    #[test]
    fn network_device_reset_clears_pending_flags_but_retains_limiter_budget() {
        let now = Instant::now();
        let config =
            NetworkRateLimiterConfig::new(None, Some(NetworkTokenBucketConfig::new(1, None, 100)));
        let mut device = VirtioNetworkDevice::with_rate_limiters_at(None, Some(config), now);
        assert!(matches!(
            device
                .tx_rate_limiter
                .as_mut()
                .expect("TX limiter should exist")
                .reduce_at(1, now),
            super::VirtioNetworkRateLimiterReduction::Allowed(_)
        ));
        device.pending_rate_limited_rx_queue = true;
        device.pending_rate_limited_tx_queue = true;
        let spent_limiter = device
            .tx_rate_limiter()
            .expect("TX limiter should exist")
            .clone();

        device.reset();

        assert!(!device.has_pending_rate_limited_queue_work());
        assert_eq!(device.tx_rate_limiter(), Some(&spent_limiter));
    }

    #[test]
    fn prepared_network_devices_preserve_interface_order() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("first network config should be stored");
        configs
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1"))
            .expect("second network config should be stored");

        let devices =
            PreparedNetworkDevices::from_configs(&configs).expect("network devices should prepare");

        assert_eq!(devices.as_slice()[0].iface_id(), "eth0");
        assert_eq!(devices.as_slice()[0].host_dev_name(), "tap0");
        assert_eq!(devices.as_slice()[1].iface_id(), "eth1");
        assert_eq!(devices.as_slice()[1].host_dev_name(), "tap1");
    }

    #[test]
    fn prepared_network_devices_do_not_touch_host_device_name() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new(
                "eth0",
                "eth0",
                "/definitely/missing/bangbang-tap",
            ))
            .expect("network config should be stored");

        let devices = PreparedNetworkDevices::from_configs(&configs)
            .expect("network preparation should not open host devices");

        assert_eq!(
            devices.as_slice()[0].host_dev_name(),
            "/definitely/missing/bangbang-tap"
        );
    }

    #[test]
    fn prepared_network_device_into_parts_consumes_owned_resource() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input().with_guest_mac("12:34:56:78:9a:bc"))
            .expect("network config should be stored");

        let mut devices = PreparedNetworkDevices::from_configs(&configs)
            .expect("network device should prepare")
            .into_vec();
        let device = devices
            .pop()
            .expect("prepared network device should be returned");
        let (iface_id, host_dev_name, config_space, device) = device.into_parts();

        assert!(devices.is_empty());
        assert_eq!(iface_id, "eth0");
        assert_eq!(host_dev_name, "tap0");
        assert_eq!(config_space.guest_mac(), Some(test_guest_mac()));
        assert!(!device.is_activated());
    }

    #[test]
    fn network_mmio_devices_accept_empty_prepared_devices() {
        let configs = NetworkInterfaceConfigs::new();
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("empty configs should prepare");

        let devices = prepared
            .register_mmio(NetworkMmioLayout::new(
                TEST_MMIO_BASE,
                MmioRegionId::new(10),
            ))
            .expect("empty prepared network devices should register");

        assert!(devices.is_empty());
        assert_eq!(devices.len(), 0);
        assert!(devices.registrations().is_empty());
        assert!(devices.dispatcher().regions().is_empty());
    }

    #[test]
    fn network_mmio_devices_register_into_existing_dispatcher() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("network config should be stored");
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network device should prepare");
        let mut dispatcher = MmioDispatcher::new();
        let existing_region = dispatcher
            .insert_region(MmioRegionId::new(1), GuestAddress::new(0x3000_0000), 0x1000)
            .expect("existing MMIO region should insert");

        let devices = prepared
            .register_mmio_with_dispatcher(
                NetworkMmioLayout::new(TEST_MMIO_BASE, MmioRegionId::new(10)),
                dispatcher,
            )
            .expect("network MMIO device should register");

        assert_eq!(devices.registrations().len(), 1);
        assert_eq!(devices.dispatcher().regions().len(), 2);
        assert!(devices.dispatcher().regions().contains(&existing_region));
        assert!(
            devices
                .dispatcher()
                .regions()
                .contains(&devices.registrations()[0].region())
        );
    }

    #[test]
    fn network_mmio_devices_register_one_prepared_device() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(input().with_guest_mac("12:34:56:78:9a:bc"))
            .expect("network config should be stored");
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network device should prepare");

        let mut devices = prepared
            .register_mmio(NetworkMmioLayout::new(
                TEST_MMIO_BASE,
                MmioRegionId::new(10),
            ))
            .expect("network MMIO device should register");

        assert_eq!(devices.len(), 1);
        let registration = &devices.registrations()[0];
        assert_eq!(registration.index(), 0);
        assert_eq!(registration.iface_id(), "eth0");
        assert_eq!(registration.host_dev_name(), "tap0");
        assert_eq!(registration.region_id(), MmioRegionId::new(10));
        assert_eq!(registration.address(), TEST_MMIO_BASE);
        assert_eq!(
            registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(devices.dispatcher().regions().len(), 1);
        assert_eq!(devices.dispatcher().regions()[0], registration.region());
        assert_eq!(
            dispatch_network_mmio_read_u32(
                &mut devices,
                0,
                VirtioMmioRegister::MagicValue.offset(),
            ),
            VIRTIO_MMIO_MAGIC_VALUE,
        );
        assert_eq!(
            dispatch_network_mmio_read_u32(&mut devices, 0, VirtioMmioRegister::DeviceId.offset()),
            VIRTIO_NET_DEVICE_ID,
        );
        assert_eq!(
            dispatch_network_mmio_read(&mut devices, 0, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 4)
                .as_slice(),
            &[0x12, 0x34, 0x56, 0x78],
        );
    }

    #[test]
    fn network_mmio_devices_preserve_prepared_interface_order_and_layout() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("first network config should be stored");
        configs
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1"))
            .expect("second network config should be stored");
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network devices should prepare");

        let devices = prepared
            .register_mmio(
                NetworkMmioLayout::new(TEST_MMIO_BASE, MmioRegionId::new(20))
                    .with_address_stride(VIRTIO_MMIO_DEVICE_WINDOW_SIZE * 2)
                    .with_region_id_stride(3),
            )
            .expect("network MMIO devices should register");

        assert_eq!(devices.registrations()[0].iface_id(), "eth0");
        assert_eq!(devices.registrations()[0].host_dev_name(), "tap0");
        assert_eq!(devices.registrations()[0].index(), 0);
        assert_eq!(
            devices.registrations()[0].region_id(),
            MmioRegionId::new(20)
        );
        assert_eq!(devices.registrations()[0].address(), TEST_MMIO_BASE);
        assert_eq!(devices.registrations()[1].iface_id(), "eth1");
        assert_eq!(devices.registrations()[1].host_dev_name(), "tap1");
        assert_eq!(devices.registrations()[1].index(), 1);
        assert_eq!(
            devices.registrations()[1].region_id(),
            MmioRegionId::new(23)
        );
        assert_eq!(
            devices.registrations()[1].address(),
            TEST_MMIO_BASE
                .checked_add(VIRTIO_MMIO_DEVICE_WINDOW_SIZE * 2)
                .expect("test address should not overflow"),
        );
    }

    #[test]
    fn network_mmio_devices_reject_overlapping_address_stride() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("first network config should be stored");
        configs
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1"))
            .expect("second network config should be stored");
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network devices should prepare");

        let err = prepared
            .register_mmio(
                NetworkMmioLayout::new(TEST_MMIO_BASE, MmioRegionId::new(30))
                    .with_address_stride(VIRTIO_MMIO_DEVICE_WINDOW_SIZE - 1),
            )
            .expect_err("overlapping network MMIO layout should fail");

        assert!(matches!(
            err,
            NetworkMmioRegistrationError::AddressStrideTooSmall { .. },
        ));
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn network_mmio_devices_reject_duplicate_region_id_stride() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("first network config should be stored");
        configs
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1"))
            .expect("second network config should be stored");
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network devices should prepare");

        let err = prepared
            .register_mmio(
                NetworkMmioLayout::new(TEST_MMIO_BASE, MmioRegionId::new(40))
                    .with_region_id_stride(0),
            )
            .expect_err("duplicate network MMIO region id layout should fail");

        assert!(matches!(
            err,
            NetworkMmioRegistrationError::DuplicateRegionIdStride { .. },
        ));
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn network_mmio_devices_reject_address_overflow_without_returning_bundle() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("first network config should be stored");
        configs
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1"))
            .expect("second network config should be stored");
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network devices should prepare");

        let err = prepared
            .register_mmio(
                NetworkMmioLayout::new(TEST_MMIO_BASE, MmioRegionId::new(50))
                    .with_address_stride(u64::MAX),
            )
            .expect_err("overflowing network MMIO layout should fail");

        assert!(matches!(
            err,
            NetworkMmioRegistrationError::AddressOverflow { .. },
        ));
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn network_mmio_devices_reject_region_range_overflow_without_returning_bundle() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("network config should be stored");
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network device should prepare");

        let err = prepared
            .register_mmio(NetworkMmioLayout::new(
                GuestAddress::new(u64::MAX),
                MmioRegionId::new(60),
            ))
            .expect_err("overflowing network MMIO region range should fail");

        assert!(matches!(
            err,
            NetworkMmioRegistrationError::InvalidRegion { .. },
        ));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn network_mmio_devices_reject_region_id_overflow_without_returning_bundle() {
        let mut configs = NetworkInterfaceConfigs::new();
        configs
            .insert(NetworkInterfaceConfigInput::new("eth0", "eth0", "tap0"))
            .expect("first network config should be stored");
        configs
            .insert(NetworkInterfaceConfigInput::new("eth1", "eth1", "tap1"))
            .expect("second network config should be stored");
        let prepared =
            PreparedNetworkDevices::from_configs(&configs).expect("network devices should prepare");

        let err = prepared
            .register_mmio(NetworkMmioLayout::new(
                TEST_MMIO_BASE,
                MmioRegionId::new(u64::MAX),
            ))
            .expect_err("overflowing network MMIO region id should fail");

        assert!(matches!(
            err,
            NetworkMmioRegistrationError::RegionIdOverflow { .. },
        ));
        assert!(std::error::Error::source(&err).is_none());
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
        assert_eq!(VIRTIO_RING_FEATURE_INDIRECT_DESC, 28);
        assert_eq!(VIRTIO_RING_FEATURE_EVENT_IDX, 29);
        assert_eq!(VIRTIO_FEATURE_VERSION_1, 32);
        assert_eq!(VIRTIO_NET_TX_HEADER_SIZE, 12);
        assert_eq!(VIRTIO_NET_MAX_BUFFER_SIZE, 65_562);
        assert_eq!(VIRTIO_NET_RX_MIN_BUFFER_SIZE, 1_526);
    }

    #[test]
    fn virtio_network_tx_frame_parser_accepts_single_descriptor_frame() {
        let mut memory = tx_frame_memory();
        write_nonzero_tx_header(&mut memory, TEST_TX_HEADER);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 4,
                None,
            )],
        );

        let frame = parse_tx_frame(&memory, &chain).expect("single-descriptor frame should parse");

        assert_eq!(frame.descriptor_head(), 0);
        assert_eq!(frame.header().flags(), 0x1);
        assert_eq!(frame.header().gso_type(), 0x2);
        assert_eq!(frame.header().header_len(), 0x0304);
        assert_eq!(frame.header().gso_size(), 0x0506);
        assert_eq!(frame.header().checksum_start(), 0x0708);
        assert_eq!(frame.header().checksum_offset(), 0x090a);
        assert_eq!(frame.header().num_buffers(), 0x0b0c);
        assert_eq!(frame.payload_len(), 4);
        assert_eq!(frame.frame_len(), 16);

        let segment = frame
            .payload_segments()
            .first()
            .expect("payload segment should exist");
        assert_eq!(frame.payload_segments().len(), 1);
        assert_eq!(segment.descriptor_index(), 0);
        assert_eq!(
            segment.address(),
            TEST_TX_HEADER
                .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                .expect("payload address should not overflow")
        );
        assert_eq!(segment.len(), 4);
    }

    #[test]
    fn virtio_network_tx_frame_parser_accepts_split_payload_descriptors() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 4, Some(2)),
                TestDescriptor::readable(TEST_TX_SECOND_PAYLOAD, 5, None),
            ],
        );

        let frame = parse_tx_frame(&memory, &chain).expect("split TX frame should parse");

        assert_eq!(frame.payload_len(), 9);
        assert_eq!(frame.frame_len(), 21);
        assert_eq!(frame.payload_segments().len(), 2);
        let first = frame
            .payload_segments()
            .first()
            .expect("first payload segment should exist");
        let second = frame
            .payload_segments()
            .get(1)
            .expect("second payload segment should exist");
        assert_eq!(first.descriptor_index(), 1);
        assert_eq!(first.address(), TEST_TX_PAYLOAD);
        assert_eq!(first.len(), 4);
        assert_eq!(second.descriptor_index(), 2);
        assert_eq!(second.address(), TEST_TX_SECOND_PAYLOAD);
        assert_eq!(second.len(), 5);
    }

    #[test]
    fn virtio_network_tx_frame_parser_indirect_chain_uses_outer_descriptor_head() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let outer_head = 4;
        let chain = tx_indirect_descriptor_chain(
            &mut memory,
            outer_head,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 4, None),
            ],
        );

        let frame = parse_tx_frame(&memory, &chain).expect("indirect TX frame should parse");

        assert_eq!(frame.descriptor_head(), outer_head);
        assert_eq!(frame.payload_len(), 4);
        let segment = frame
            .payload_segments()
            .first()
            .expect("indirect TX payload segment should exist");
        assert_eq!(segment.descriptor_index(), 1);
        assert_eq!(segment.address(), TEST_TX_PAYLOAD);
        assert_eq!(segment.len(), 4);
    }

    #[test]
    fn virtio_network_tx_frame_parser_accepts_header_remainder_and_split_payload() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE + 3, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 4, None),
            ],
        );

        let frame =
            parse_tx_frame(&memory, &chain).expect("header remainder plus payload should parse");

        assert_eq!(frame.payload_len(), 7);
        assert_eq!(frame.payload_segments().len(), 2);
        let first = frame
            .payload_segments()
            .first()
            .expect("header remainder segment should exist");
        let second = frame
            .payload_segments()
            .get(1)
            .expect("following payload segment should exist");
        assert_eq!(first.descriptor_index(), 0);
        assert_eq!(
            first.address(),
            TEST_TX_HEADER
                .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                .expect("header remainder address should not overflow")
        );
        assert_eq!(first.len(), 3);
        assert_eq!(second.descriptor_index(), 1);
        assert_eq!(second.address(), TEST_TX_PAYLOAD);
        assert_eq!(second.len(), 4);
    }

    #[test]
    fn virtio_network_tx_frame_parser_rejects_write_only_header() {
        let mut memory = tx_frame_memory();
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 1,
                None,
            )],
        );

        assert!(matches!(
            parse_tx_frame(&memory, &chain),
            Err(VirtioNetworkTxFrameParseError::HeaderDescriptorWriteOnly { index: 0 })
        ));
    }

    #[test]
    fn virtio_network_tx_frame_parser_rejects_small_header() {
        let mut memory = tx_frame_memory();
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE - 1,
                None,
            )],
        );

        assert!(matches!(
            parse_tx_frame(&memory, &chain),
            Err(VirtioNetworkTxFrameParseError::HeaderDescriptorTooSmall {
                index: 0,
                len: 11,
                min: 12,
            })
        ));
    }

    #[test]
    fn virtio_network_tx_frame_parser_rejects_unmapped_header() {
        let mut memory = tx_frame_memory();
        let unmapped_header = GuestAddress::new(TEST_TX_MEMORY_SIZE + 0x1000);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                unmapped_header,
                VIRTIO_NET_TX_HEADER_SIZE + 1,
                None,
            )],
        );

        let error = parse_tx_frame(&memory, &chain).expect_err("unmapped header should fail");

        match &error {
            VirtioNetworkTxFrameParseError::ReadHeader { address, .. } => {
                assert_eq!(*address, unmapped_header);
            }
            other => panic!("expected header read error, got {other:?}"),
        }
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn virtio_network_tx_frame_parser_rejects_missing_payload() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE,
                None,
            )],
        );

        assert!(matches!(
            parse_tx_frame(&memory, &chain),
            Err(VirtioNetworkTxFrameParseError::MissingPayload { descriptor_head: 0 })
        ));
    }

    #[test]
    fn virtio_network_tx_frame_parser_indirect_missing_payload_uses_outer_descriptor_head() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let outer_head = 5;
        let chain = tx_indirect_descriptor_chain(
            &mut memory,
            outer_head,
            &[TestDescriptor::readable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE,
                None,
            )],
        );

        assert!(matches!(
            parse_tx_frame(&memory, &chain),
            Err(VirtioNetworkTxFrameParseError::MissingPayload { descriptor_head })
                if descriptor_head == outer_head
        ));
    }

    #[test]
    fn virtio_network_tx_frame_parser_rejects_write_only_payload() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(TEST_TX_PAYLOAD, 4, None),
            ],
        );

        assert!(matches!(
            parse_tx_frame(&memory, &chain),
            Err(VirtioNetworkTxFrameParseError::PayloadDescriptorWriteOnly { index: 1 })
        ));
    }

    #[test]
    fn virtio_network_tx_frame_parser_rejects_empty_payload() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 0, None),
            ],
        );

        assert!(matches!(
            parse_tx_frame(&memory, &chain),
            Err(VirtioNetworkTxFrameParseError::PayloadDescriptorEmpty { index: 1 })
        ));
    }

    #[test]
    fn virtio_network_tx_frame_parser_rejects_unmapped_payload() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let unmapped_payload = GuestAddress::new(TEST_TX_MEMORY_SIZE + 0x1000);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(unmapped_payload, 4, None),
            ],
        );

        let error = parse_tx_frame(&memory, &chain).expect_err("unmapped payload should fail");

        match &error {
            VirtioNetworkTxFrameParseError::PayloadDescriptorAccess {
                index,
                address,
                len,
                ..
            } => {
                assert_eq!(*index, 1);
                assert_eq!(*address, unmapped_payload);
                assert_eq!(*len, 4);
            }
            other => panic!("expected payload access error, got {other:?}"),
        }
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn virtio_network_tx_frame_parser_rejects_payload_range_overflow() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let overflowing_payload = GuestAddress::new(u64::MAX - 1);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(overflowing_payload, 4, None),
            ],
        );

        assert!(matches!(
            parse_tx_frame(&memory, &chain),
            Err(VirtioNetworkTxFrameParseError::PayloadDescriptorRangeOverflow {
                index: 1,
                address,
                len: 4,
            }) if address == overflowing_payload
        ));
    }

    #[test]
    fn virtio_network_tx_frame_parser_rejects_oversized_frame_before_mapping_payload() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
        let too_large_payload_len =
            u32::try_from(VIRTIO_NET_MAX_BUFFER_SIZE).expect("max buffer should fit in u32");
        let chain = tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, too_large_payload_len, None),
            ],
        );

        assert!(matches!(
            parse_tx_frame(&memory, &chain),
            Err(VirtioNetworkTxFrameParseError::FrameTooLarge { len, max })
                if len == VIRTIO_NET_MAX_BUFFER_SIZE + u64::from(VIRTIO_NET_TX_HEADER_SIZE)
                    && max == VIRTIO_NET_MAX_BUFFER_SIZE
        ));
    }

    #[test]
    fn virtio_network_rx_buffer_parser_accepts_single_descriptor_buffer() {
        let mut memory = tx_frame_memory();
        let len = u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
            .expect("RX minimum should fit in descriptor len");
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::writable(TEST_TX_PAYLOAD, len, None)],
        );

        let buffer = parse_rx_buffer(&memory, &chain).expect("single RX buffer should parse");

        assert_eq!(buffer.descriptor_head(), 0);
        assert_eq!(buffer.len(), VIRTIO_NET_RX_MIN_BUFFER_SIZE);
        assert!(!buffer.is_empty());
        assert_eq!(buffer.segments().len(), 1);
        let segment = buffer
            .segments()
            .first()
            .expect("RX buffer segment should exist");
        assert_eq!(segment.descriptor_index(), 0);
        assert_eq!(segment.address(), TEST_TX_PAYLOAD);
        assert_eq!(segment.len(), len);
        assert!(!segment.is_empty());
    }

    #[test]
    fn virtio_network_rx_buffer_parser_indirect_chain_uses_outer_descriptor_head() {
        let mut memory = tx_frame_memory();
        let len = u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
            .expect("RX minimum should fit in descriptor len");
        let outer_head = 6;
        let chain = rx_indirect_descriptor_chain(
            &mut memory,
            outer_head,
            &[TestDescriptor::writable(TEST_RX_BUFFER, len, None)],
        );

        let buffer = parse_rx_buffer(&memory, &chain).expect("indirect RX buffer should parse");

        assert_eq!(buffer.descriptor_head(), outer_head);
        assert_eq!(buffer.len(), VIRTIO_NET_RX_MIN_BUFFER_SIZE);
        let segment = buffer
            .segments()
            .first()
            .expect("indirect RX buffer segment should exist");
        assert_eq!(segment.descriptor_index(), 0);
        assert_eq!(segment.address(), TEST_RX_BUFFER);
        assert_eq!(segment.len(), len);
    }

    #[test]
    fn virtio_network_rx_buffer_parser_accepts_split_buffer() {
        let mut memory = tx_frame_memory();
        let chain = tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::writable(TEST_TX_PAYLOAD, 1_000, Some(1)),
                TestDescriptor::writable(TEST_TX_SECOND_PAYLOAD, 526, None),
            ],
        );

        let buffer = parse_rx_buffer(&memory, &chain).expect("split RX buffer should parse");

        assert_eq!(buffer.len(), VIRTIO_NET_RX_MIN_BUFFER_SIZE);
        assert_eq!(buffer.segments().len(), 2);
        let first = buffer
            .segments()
            .first()
            .expect("first RX buffer segment should exist");
        let second = buffer
            .segments()
            .get(1)
            .expect("second RX buffer segment should exist");
        assert_eq!(first.descriptor_index(), 0);
        assert_eq!(first.address(), TEST_TX_PAYLOAD);
        assert_eq!(first.len(), 1_000);
        assert_eq!(second.descriptor_index(), 1);
        assert_eq!(second.address(), TEST_TX_SECOND_PAYLOAD);
        assert_eq!(second.len(), 526);
    }

    #[test]
    fn virtio_network_rx_buffer_parser_rejects_read_only_descriptor() {
        let mut memory = tx_frame_memory();
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(TEST_TX_PAYLOAD, 1_526, None)],
        );

        assert!(matches!(
            parse_rx_buffer(&memory, &chain),
            Err(VirtioNetworkRxBufferParseError::BufferDescriptorReadOnly { index: 0 })
        ));
    }

    #[test]
    fn virtio_network_rx_buffer_parser_rejects_empty_descriptor() {
        let mut memory = tx_frame_memory();
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::writable(TEST_TX_PAYLOAD, 0, None)],
        );

        assert!(matches!(
            parse_rx_buffer(&memory, &chain),
            Err(VirtioNetworkRxBufferParseError::BufferDescriptorEmpty { index: 0 })
        ));
    }

    #[test]
    fn virtio_network_rx_buffer_parser_rejects_unmapped_descriptor() {
        let mut memory = tx_frame_memory();
        let unmapped_buffer = GuestAddress::new(TEST_TX_MEMORY_SIZE + 0x1000);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::writable(unmapped_buffer, 1_526, None)],
        );

        let error = parse_rx_buffer(&memory, &chain).expect_err("unmapped RX buffer should fail");

        match &error {
            VirtioNetworkRxBufferParseError::BufferDescriptorAccess {
                index,
                address,
                len,
                ..
            } => {
                assert_eq!(*index, 0);
                assert_eq!(*address, unmapped_buffer);
                assert_eq!(*len, 1_526);
            }
            other => panic!("expected RX buffer access error, got {other:?}"),
        }
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn virtio_network_rx_buffer_parser_rejects_descriptor_range_overflow() {
        let mut memory = tx_frame_memory();
        let overflowing_buffer = GuestAddress::new(u64::MAX - 1);
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::writable(overflowing_buffer, 1_526, None)],
        );

        assert!(matches!(
            parse_rx_buffer(&memory, &chain),
            Err(VirtioNetworkRxBufferParseError::BufferDescriptorRangeOverflow {
                index: 0,
                address,
                len: 1_526,
            }) if address == overflowing_buffer
        ));
    }

    #[test]
    fn virtio_network_rx_buffer_parser_rejects_length_overflow_without_stale_segments() {
        let memory = tx_frame_memory();
        let segment = super::VirtioNetworkRxBufferSegment::new(0, TEST_TX_PAYLOAD, 1);
        let mut segments = Vec::new();

        let error = super::push_rx_buffer_segment(&memory, &mut segments, u64::MAX, segment)
            .expect_err("overflowing RX buffer length should fail");

        assert!(matches!(
            error,
            VirtioNetworkRxBufferParseError::BufferLengthOverflow {
                current: u64::MAX,
                len: 1,
            }
        ));
        assert!(segments.is_empty());
    }

    #[test]
    fn virtio_network_rx_buffer_parser_rejects_small_buffer() {
        let mut memory = tx_frame_memory();
        let chain = tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::writable(TEST_TX_PAYLOAD, 1_525, None)],
        );

        assert!(matches!(
            parse_rx_buffer(&memory, &chain),
            Err(VirtioNetworkRxBufferParseError::BufferTooSmall {
                len: 1_525,
                min,
            }) if min == VIRTIO_NET_RX_MIN_BUFFER_SIZE
        ));
    }

    #[test]
    fn virtio_network_config_space_tracks_configured_features() {
        let base_features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_INDIRECT_DESC)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX)
            | VirtioNetworkFeatureCapabilities::complete_software().feature_bits();
        let without_mac = VirtioNetworkConfigSpace::new(None, None);
        let with_mac = VirtioNetworkConfigSpace::new(Some(test_guest_mac()), None);
        let with_mtu = VirtioNetworkConfigSpace::new(None, Some(1500));
        let with_mac_and_mtu = VirtioNetworkConfigSpace::new(Some(test_guest_mac()), Some(1500));

        assert_eq!(without_mac.guest_mac(), None);
        assert_eq!(without_mac.mtu(), None);
        assert_eq!(without_mac.available_features(), base_features);
        assert_eq!(with_mac.guest_mac(), Some(test_guest_mac()));
        assert_eq!(
            with_mac.available_features(),
            base_features | virtio_feature_bit(VIRTIO_NET_F_MAC)
        );
        assert_eq!(with_mtu.guest_mac(), None);
        assert_eq!(with_mtu.mtu(), Some(1500));
        assert_eq!(
            with_mtu.available_features(),
            base_features | virtio_feature_bit(VIRTIO_NET_F_MTU)
        );
        assert_eq!(
            with_mac_and_mtu.available_features(),
            base_features
                | virtio_feature_bit(VIRTIO_NET_F_MAC)
                | virtio_feature_bit(VIRTIO_NET_F_MTU)
        );
    }

    #[test]
    fn virtio_network_config_space_reads_mac_bytes() {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()), None);

        assert_eq!(
            read_network_config(&config, 0, 4)
                .expect("low MAC config word should read")
                .as_slice(),
            &[0x12, 0x34, 0x56, 0x78]
        );
        assert_eq!(
            read_network_config(&config, 4, 2)
                .expect("high MAC config halfword should read")
                .as_slice(),
            &[0x9a, 0xbc]
        );
        assert_eq!(
            read_network_config(&config, 1, 2)
                .expect("partial MAC config read should succeed")
                .as_slice(),
            &[0x34, 0x56]
        );
        assert_eq!(
            read_network_config(&config, 5, 1)
                .expect("last MAC byte should read")
                .as_slice(),
            &[0xbc]
        );
        assert_eq!(
            read_network_config(&config, 2, 4)
                .expect("read ending at MAC boundary should succeed")
                .as_slice(),
            &[0x56, 0x78, 0x9a, 0xbc]
        );
    }

    #[test]
    fn virtio_network_config_space_reads_mtu_bytes() {
        let config = VirtioNetworkConfigSpace::new(None, Some(1500));
        let mtu_bytes = 1500_u16.to_le_bytes();

        assert_eq!(
            read_network_config(
                &config,
                VIRTIO_NET_CONFIG_MTU_OFFSET,
                VIRTIO_NET_CONFIG_MTU_SIZE
            )
            .expect("MTU config halfword should read")
            .as_slice(),
            &mtu_bytes
        );
        assert_eq!(
            read_network_config(&config, VIRTIO_NET_CONFIG_MTU_OFFSET, 1)
                .expect("low MTU byte should read")
                .as_slice(),
            &mtu_bytes[0..1]
        );
        assert_eq!(
            read_network_config(&config, VIRTIO_NET_CONFIG_MTU_OFFSET + 1, 1)
                .expect("high MTU byte should read")
                .as_slice(),
            &mtu_bytes[1..2]
        );
    }

    #[test]
    fn virtio_network_config_space_rejects_unsupported_reads() {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()), None);

        assert_eq!(
            read_network_config(&VirtioNetworkConfigSpace::new(None, None), 0, 1),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 0, len: 1 })
        );
        assert_eq!(
            read_network_config(&config, 6, 1),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 6, len: 1 })
        );
        assert_eq!(
            read_network_config(&config, 5, 2),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 5, len: 2 })
        );
        assert_eq!(
            read_network_config(
                &config,
                VIRTIO_NET_CONFIG_MTU_OFFSET,
                VIRTIO_NET_CONFIG_MTU_SIZE
            ),
            Err(
                VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead {
                    offset: VIRTIO_NET_CONFIG_MTU_OFFSET,
                    len: VIRTIO_NET_CONFIG_MTU_SIZE,
                }
            )
        );
        assert_eq!(
            read_network_config(
                &VirtioNetworkConfigSpace::new(None, Some(1500)),
                VIRTIO_NET_CONFIG_MTU_OFFSET - 1,
                VIRTIO_NET_CONFIG_MTU_SIZE,
            ),
            Err(
                VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead {
                    offset: VIRTIO_NET_CONFIG_MTU_OFFSET - 1,
                    len: VIRTIO_NET_CONFIG_MTU_SIZE,
                }
            )
        );
        assert_eq!(
            read_network_config(
                &VirtioNetworkConfigSpace::new(None, Some(1500)),
                VIRTIO_NET_CONFIG_MTU_OFFSET,
                VIRTIO_NET_CONFIG_MTU_SIZE + 2,
            ),
            Err(
                VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead {
                    offset: VIRTIO_NET_CONFIG_MTU_OFFSET,
                    len: VIRTIO_NET_CONFIG_MTU_SIZE + 2,
                }
            )
        );
    }

    #[test]
    fn virtio_network_config_space_rejects_writes_after_driver_status() {
        assert_eq!(
            write_network_config_after_driver(
                VirtioNetworkConfigSpace::new(Some(test_guest_mac()), None),
                0,
                &[1, 2, 3, 4],
            ),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite { offset: 0, len: 4 })
        );
    }

    #[test]
    fn virtio_network_transport_records_config_and_activation_failures_at_source() {
        let metrics = SharedNetworkInterfaceMetrics::default();
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()), None);
        let mut handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_NET_DEVICE_ID,
            config.available_features(),
            &VIRTIO_NET_QUEUE_SIZES,
            config,
            VirtioNetworkDevice::new(),
        )
        .expect("instrumented network handler should build");
        handler.attach_network_metrics(metrics.clone());

        assert!(
            handler
                .read_access(mmio_access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 6, 1))
                .is_err()
        );
        put_network_handler_in_queue_config_state(&mut handler);
        assert!(
            handler
                .write_access(
                    mmio_access(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, 1),
                    MmioAccessBytes::new(&[1]).expect("one config byte should encode"),
                )
                .is_err()
        );
        assert!(
            handler
                .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
                .is_err(),
            "missing queue configuration should fail activation"
        );

        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_cfg_fails(2)
                .with_activate_fails(1)
        );
    }

    #[test]
    fn virtio_network_config_space_runs_through_mmio_register_handler() {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()), None);
        let mut handler = network_handler(config.clone());

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

    #[test]
    fn virtio_network_device_activation_stores_rx_and_tx_queues() {
        let mut device = VirtioNetworkDevice::new();
        let registers = network_device_registers();
        let queues = configured_network_queues(
            Some(TEST_QUEUE_SIZE),
            true,
            Some(TEST_RETRY_QUEUE_SIZE),
            true,
        );

        device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect("network device should activate");

        assert!(device.is_activated());
        assert_eq!(
            device
                .active_rx_queue()
                .expect("RX queue should be active")
                .size(),
            TEST_QUEUE_SIZE
        );
        assert_eq!(
            device
                .active_tx_queue()
                .expect("TX queue should be active")
                .size(),
            TEST_RETRY_QUEUE_SIZE
        );
    }

    #[test]
    fn virtio_network_device_activation_rejects_not_ready_queue_without_stale_state() {
        let mut device = VirtioNetworkDevice::new();
        let registers = network_device_registers();
        let queues =
            configured_network_queues(Some(TEST_QUEUE_SIZE), true, Some(TEST_QUEUE_SIZE), false);

        let error = device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("not-ready TX queue should not activate");

        assert!(matches!(
            error,
            VirtioNetworkDeviceActivationError::QueueNotReady { queue_index: 1 }
        ));
        assert!(!device.is_activated());
        assert!(device.active_rx_queue().is_none());
        assert!(device.active_tx_queue().is_none());
    }

    #[test]
    fn virtio_network_device_activation_rejects_zero_size_queue_without_stale_state() {
        let mut device = VirtioNetworkDevice::new();
        let registers = network_device_registers();
        let queues = configured_network_queues(Some(TEST_QUEUE_SIZE), true, None, true);

        let error = device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("ready zero-size TX queue should not activate");

        assert!(matches!(
            error,
            VirtioNetworkDeviceActivationError::QueueSizeNotConfigured { queue_index: 1 }
        ));
        assert!(!device.is_activated());
        assert!(device.active_rx_queue().is_none());
        assert!(device.active_tx_queue().is_none());
    }

    #[test]
    fn virtio_network_device_activation_rejects_duplicate_without_replacing_queues() {
        let mut device = VirtioNetworkDevice::new();
        let registers = network_device_registers();
        let first_queues =
            configured_network_queues(Some(TEST_QUEUE_SIZE), true, Some(TEST_QUEUE_SIZE), true);
        let second_queues = configured_network_queues(
            Some(TEST_RETRY_QUEUE_SIZE),
            true,
            Some(TEST_RETRY_QUEUE_SIZE),
            true,
        );

        device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &first_queues))
            .expect("first activation should succeed");

        let error = device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &second_queues))
            .expect_err("duplicate activation should fail");

        assert!(matches!(
            error,
            VirtioNetworkDeviceActivationError::AlreadyActive
        ));
        assert_eq!(
            device
                .active_rx_queue()
                .expect("original RX queue should remain active")
                .size(),
            TEST_QUEUE_SIZE
        );
        assert_eq!(
            device
                .active_tx_queue()
                .expect("original TX queue should remain active")
                .size(),
            TEST_QUEUE_SIZE
        );
    }

    #[test]
    fn virtio_network_device_activation_reset_clears_state_and_allows_retry() {
        let mut device = VirtioNetworkDevice::new();
        let registers = network_device_registers();
        let first_queues =
            configured_network_queues(Some(TEST_QUEUE_SIZE), true, Some(TEST_QUEUE_SIZE), true);
        let second_queues = configured_network_queues(
            Some(TEST_RETRY_QUEUE_SIZE),
            true,
            Some(TEST_RETRY_QUEUE_SIZE),
            true,
        );

        device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &first_queues))
            .expect("first activation should succeed");

        VirtioMmioDeviceActivationHandler::reset(&mut device);

        assert!(!device.is_activated());
        assert!(device.active_rx_queue().is_none());
        assert!(device.active_tx_queue().is_none());

        device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &second_queues))
            .expect("second activation should succeed after reset");

        assert_eq!(
            device
                .active_rx_queue()
                .expect("RX queue should be active after retry")
                .size(),
            TEST_RETRY_QUEUE_SIZE
        );
    }

    #[test]
    fn virtio_network_device_activation_trait_error_is_generic_handler_error() {
        let mut device = VirtioNetworkDevice::new();
        let registers = network_device_registers();
        let queues =
            configured_network_queues(Some(TEST_QUEUE_SIZE), false, Some(TEST_QUEUE_SIZE), true);

        let error = VirtioMmioDeviceActivationHandler::activate(
            &mut device,
            VirtioMmioDeviceActivation::new(&registers, &queues),
        )
        .expect_err("trait activation should fail with generic handler error");

        match &error {
            VirtioMmioDeviceActivationError::Handler { source } => {
                assert_eq!(source.to_string(), "virtio-net queue 0 is not ready");
            }
        }
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn virtio_network_device_activation_reports_queue_metadata_errors() {
        let mut device = VirtioNetworkDevice::new();
        let registers = network_device_registers();
        let mut queues = VirtioMmioQueueRegisters::new(&[VIRTIO_NET_QUEUE_SIZE])
            .expect("one queue table should build");
        configure_network_queue_registers(
            &mut queues,
            VIRTIO_NET_RX_QUEUE_INDEX
                .try_into()
                .expect("RX queue index should fit"),
            Some(TEST_QUEUE_SIZE),
            true,
        );

        let error = device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("missing TX queue should fail activation");

        match &error {
            VirtioNetworkDeviceActivationError::QueueMetadata {
                queue_index,
                source:
                    VirtioMmioQueueRegisterError::InvalidQueueIndex {
                        queue_index: source_queue_index,
                        queue_count,
                    },
            } => {
                assert_eq!(*queue_index, 1);
                assert_eq!(*source_queue_index, 1);
                assert_eq!(*queue_count, 1);
            }
            other => panic!("expected queue metadata error, got {other:?}"),
        }
    }

    #[test]
    fn virtio_network_device_activation_runs_through_mmio_register_handler_and_reset() {
        let mut handler = network_activation_handler();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);

        assert!(handler.is_device_activated());
        assert!(handler.activation_handler().is_activated());
        assert_eq!(
            handler
                .activation_handler()
                .active_rx_queue()
                .expect("RX queue should be active")
                .device_ring(),
            TEST_RX_USED_RING
        );
        assert_eq!(
            handler
                .activation_handler()
                .active_tx_queue()
                .expect("TX queue should be active")
                .device_ring(),
            TEST_TX_USED_RING
        );

        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_INIT)
            .expect("INIT status should reset network activation state");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
    }

    #[test]
    fn virtio_network_capture_is_stable_owned_and_rejects_profile_drift() {
        let handler = network_activation_handler();
        let memory = tx_frame_memory();
        let config = input()
            .with_guest_mac(test_guest_mac().to_string())
            .validate()
            .expect("capture network config should validate");
        let profile = NetworkDeviceProfile::from_config(&config);
        let now = Instant::now();

        let (first, first_validation) = handler
            .capture_network_state_at(&config, profile, &memory, None, now)
            .expect("inactive MMIO network should be capture-ready");
        let (second, second_validation) = handler
            .capture_network_state_at(&config, profile, &memory, None, now)
            .expect("repeated capture should remain stable");

        assert_eq!(first, second);
        assert_eq!(first_validation, second_validation);
        assert_eq!(first_validation.source_rx_retry(), None);
        assert_eq!(first.device().profile(), profile);
        assert!(first.device().active_rx_queue().is_none());
        assert!(first.device().active_tx_queue().is_none());
        assert!(!first.device().rx_rate_limiter().is_configured());
        assert!(!first.device().tx_rate_limiter().is_configured());
        assert!(!first.device().source_rx_cache_normalized());
        assert!(!first.device().source_rx_retry_normalized());
        assert_eq!(
            first.device().tx_retry(),
            VirtioNetworkRetryCaptureState::None
        );
        assert!(!first.transport().is_device_activated());
        let debug = format!("{first:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(&test_guest_mac().to_string()));
        assert!(!debug.contains(config.host_dev_name()));

        assert!(matches!(
            handler.capture_network_state_at(
                &config,
                NetworkDeviceProfile::new(None, None),
                &memory,
                None,
                now,
            ),
            Err(VirtioNetworkDeviceCaptureError::RequestedProfileMismatch)
        ));

        let (with_cached_rx, cached_validation) = handler
            .capture_network_state_at(&config, profile, &memory, Some(4), now)
            .expect("bounded cached RX should normalize without retaining bytes");
        assert!(with_cached_rx.device().source_rx_cache_normalized());
        assert!(!with_cached_rx.device().source_rx_retry_normalized());
        assert_eq!(cached_validation.source_rx_retry(), None);
        assert_ne!(first, with_cached_rx);
        for invalid_len in [0, usize::MAX] {
            assert!(matches!(
                handler
                    .capture_network_state_at(&config, profile, &memory, Some(invalid_len), now,),
                Err(VirtioNetworkDeviceCaptureError::CachedRxPacketInvalid)
            ));
        }
    }

    #[test]
    fn virtio_network_notifications_without_pending_work_are_noop() {
        let mut memory = tx_frame_memory();
        let mut device = VirtioNetworkDevice::new();
        let mut sink = RecordingTxPacketSink::default();

        let dispatch = device
            .dispatch_drained_queue_notifications_with_tx_sink(&mut memory, Vec::new(), &mut sink)
            .expect("empty notification drain should be a no-op");

        assert!(dispatch.drained_notifications().is_empty());
        assert!(dispatch.rx_queue_dispatch().is_none());
        assert!(dispatch.tx_queue_dispatch().is_none());
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(sink.calls, 0);
    }

    #[test]
    fn virtio_network_notifications_reject_inactive_device_with_drained_metadata() {
        let mut memory = tx_frame_memory();
        let mut device = VirtioNetworkDevice::new();
        let mut sink = RecordingTxPacketSink::default();

        let error = device
            .dispatch_drained_queue_notifications_with_tx_sink(
                &mut memory,
                vec![VIRTIO_NET_RX_QUEUE_INDEX],
                &mut sink,
            )
            .expect_err("notification before activation should fail");

        assert!(matches!(
            error,
            VirtioNetworkDeviceNotificationError::Inactive { .. }
        ));
        assert_eq!(error.drained_notifications(), &[VIRTIO_NET_RX_QUEUE_INDEX]);
        assert_eq!(
            error.to_string(),
            "virtio-net queue notification cannot be dispatched before activation"
        );
        assert!(error.completed_tx_dispatch().is_none());
        assert!(std::error::Error::source(&error).is_none());
        assert_eq!(sink.calls, 0);
    }

    #[test]
    fn virtio_network_notifications_with_empty_rx_source_are_noop_and_drain() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications(&mut memory)
            .expect("empty RX source should make notification dispatch a no-op");

        assert_eq!(
            notification.drained_notifications(),
            [VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_QUEUE_INDEX]
        );
        let rx_dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx_dispatch.processed_buffers(), 0);
        assert_eq!(rx_dispatch.delivered_packets(), 0);
        assert!(!rx_dispatch.needs_queue_interrupt());
        let tx_dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx_dispatch.processed_frames(), 0);
        assert!(!tx_dispatch.needs_queue_interrupt());
        assert!(!notification.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn virtio_network_notifications_deliver_rx_packet_to_single_buffer() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0xde, 0xad, 0xbe, 0xef]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE).expect("RX minimum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("RX queue notification should dispatch");

        assert_eq!(
            notification.drained_notifications(),
            [VIRTIO_NET_RX_QUEUE_INDEX]
        );
        let dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_packets(), 1);
        assert_eq!(dispatch.buffer_parse_failures(), 0);
        assert_eq!(dispatch.buffer_too_small_failures(), 0);
        assert_eq!(dispatch.source_failures(), 0);
        assert!(dispatch.first_buffer_parse_failure().is_none());
        assert!(dispatch.first_buffer_too_small().is_none());
        assert!(dispatch.first_source_failure().is_none());
        assert!(dispatch.needs_queue_interrupt());
        let delivery = dispatch
            .deliveries()
            .first()
            .expect("RX delivery should be recorded");
        assert_eq!(delivery.descriptor_head(), 0);
        assert_eq!(delivery.packet_len(), 4);
        assert_eq!(delivery.bytes_written_to_guest(), rx_used_len(4));
        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics.record_notification_dispatch(&notification);
        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_rx_queue_event_count(1)
                .with_rx_bytes_count(u64::from(rx_used_len(4)))
                .with_rx_packets_count(1)
                .with_rx_count(1)
        );
        assert_eq!(source.consume_calls, 1);
        assert_eq!(source.remaining_packets(), 0);
        assert_eq!(read_rx_used_index(&memory), 1);
        assert_eq!(read_rx_used_element(&memory, 0), (0, rx_used_len(4)));
        assert_eq!(
            read_guest_bytes(&memory, TEST_RX_BUFFER, VIRTIO_NET_TX_HEADER_SIZE as usize),
            vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0]
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_BUFFER
                    .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                    .expect("payload address should not overflow"),
                4,
            ),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_merged_rx_publishes_complete_chain_set_with_exact_header() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let packet = (0_u8..20).collect::<Vec<_>>();
        let mut source = RecordingRxPacketSource::with_packets(vec![packet.clone()]);

        configure_network_handler_queues_with_features(
            &mut handler,
            (1_u32 << VIRTIO_NET_F_MRG_RXBUF) | (1_u32 << VIRTIO_RING_FEATURE_EVENT_IDX),
        );
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[
                TestDescriptor::writable(TEST_RX_BUFFER, 16, None),
                TestDescriptor::writable(TEST_RX_SECOND_BUFFER, 16, None),
            ],
        );
        write_rx_available_heads(&mut memory, &[0, 1]);
        write_rx_available_used_event(&mut memory, 1);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("merged RX packet should dispatch");
        let dispatch = notification
            .rx_queue_dispatch()
            .expect("merged RX dispatch should be present");

        assert_eq!(dispatch.processed_buffers(), 2);
        assert_eq!(dispatch.delivered_packets(), 1);
        let delivery = dispatch
            .deliveries()
            .first()
            .expect("merged RX delivery should be recorded");
        assert_eq!(delivery.buffer_count(), 2);
        assert_eq!(delivery.bytes_written_to_guest(), 32);
        assert_eq!(source.consume_calls, 1);
        assert_eq!(read_rx_used_index(&memory), 2);
        assert_eq!(read_rx_used_element(&memory, 0), (0, 16));
        assert_eq!(read_rx_used_element(&memory, 1), (1, 16));
        assert_eq!(
            read_guest_bytes(&memory, TEST_RX_BUFFER, 12),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0]
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_BUFFER
                    .checked_add(12)
                    .expect("first merged payload address should fit"),
                4,
            ),
            packet
                .get(..4)
                .expect("first merged packet prefix should exist")
        );
        assert_eq!(
            read_guest_bytes(&memory, TEST_RX_SECOND_BUFFER, 16),
            packet
                .get(4..)
                .expect("second merged packet suffix should exist")
        );
        assert!(dispatch.needs_queue_interrupt());
        assert!(notification.needs_queue_interrupt());
    }

    #[test]
    fn virtio_network_merged_rx_delivers_maximum_packet_through_indirect_event_idx_chains() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let packet_len =
            usize::try_from(VIRTIO_NET_MAX_BUFFER_SIZE - u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                .expect("maximum merged RX payload should fit usize");
        let packet = vec![0xa5; packet_len];
        let mut source = RecordingRxPacketSource::with_packets(vec![packet]);
        let first_len = 32_781_u32;
        let second_len = 32_781_u32;

        configure_network_handler_queues_with_features(
            &mut handler,
            (1_u32 << VIRTIO_NET_F_MRG_RXBUF)
                | (1_u32 << VIRTIO_RING_FEATURE_EVENT_IDX)
                | (1_u32 << VIRTIO_RING_FEATURE_INDIRECT_DESC),
        );
        activate_network_handler(&mut handler);
        write_indirect_descriptor_chain(
            &mut memory,
            TEST_RX_DESCRIPTOR_TABLE,
            TEST_RX_INDIRECT_DESCRIPTOR_TABLE,
            0,
            &[TestDescriptor::writable(TEST_RX_BUFFER, first_len, None)],
        );
        write_rx_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(TEST_RX_SECOND_BUFFER, second_len, None),
        );
        write_rx_available_heads(&mut memory, &[0, 1]);
        write_rx_available_used_event(&mut memory, 1);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("merged RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("maximum indirect merged RX packet should dispatch");
        let dispatch = notification
            .rx_queue_dispatch()
            .expect("maximum merged RX dispatch should be present");

        assert_eq!(dispatch.processed_buffers(), 2);
        assert_eq!(dispatch.delivered_packets(), 1);
        assert_eq!(dispatch.deliveries()[0].buffer_count(), 2);
        assert_eq!(
            dispatch.deliveries()[0].bytes_written_to_guest(),
            u32::try_from(VIRTIO_NET_MAX_BUFFER_SIZE).expect("RX maximum should fit u32")
        );
        assert_eq!(source.consume_calls, 1);
        assert_eq!(read_rx_used_index(&memory), 2);
        assert_eq!(read_rx_used_element(&memory, 0), (0, first_len));
        assert_eq!(read_rx_used_element(&memory, 1), (1, second_len));
        assert_eq!(
            read_guest_bytes(&memory, TEST_RX_BUFFER, 12),
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0]
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_SECOND_BUFFER
                    .checked_add(u64::from(second_len - 4))
                    .expect("last merged RX bytes should fit"),
                4,
            ),
            [0xa5; 4]
        );
        assert!(dispatch.needs_queue_interrupt());
        assert!(notification.needs_queue_interrupt());
    }

    #[test]
    fn virtio_network_merged_rx_missing_capacity_restores_all_pops_and_retains_source() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x5a; 20]]);

        configure_network_handler_queues_with_features(
            &mut handler,
            1_u32 << VIRTIO_NET_F_MRG_RXBUF,
        );
        activate_network_handler(&mut handler);
        memory
            .write_slice(&[0xa5; 16], TEST_RX_BUFFER)
            .expect("RX sentinel should write");
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(TEST_RX_BUFFER, 16, None)],
        );
        write_rx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("missing merged RX capacity should remain retryable");
        let dispatch = notification
            .rx_queue_dispatch()
            .expect("merged RX dispatch should be present");

        assert_eq!(dispatch.processed_buffers(), 0);
        assert_eq!(dispatch.delivered_packets(), 0);
        assert_eq!(dispatch.no_available_buffers(), 1);
        assert_eq!(source.consume_calls, 0);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 0);
        assert_eq!(read_guest_bytes(&memory, TEST_RX_BUFFER, 16), [0xa5; 16]);
        assert_eq!(
            handler
                .activation_handler()
                .active_rx_dispatch_queue()
                .expect("RX queue should remain active")
                .available_ring()
                .next_avail(),
            0
        );
    }

    #[test]
    fn virtio_network_merged_rx_malformed_later_chain_rolls_back_before_guest_write() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x5a; 20]]);

        configure_network_handler_queues_with_features(
            &mut handler,
            1_u32 << VIRTIO_NET_F_MRG_RXBUF,
        );
        activate_network_handler(&mut handler);
        memory
            .write_slice(&[0xa5; 16], TEST_RX_BUFFER)
            .expect("first RX sentinel should write");
        memory
            .write_slice(&[0xb5; 8], TEST_RX_SECOND_BUFFER)
            .expect("second RX sentinel should write");
        write_rx_descriptors(
            &mut memory,
            &[
                TestDescriptor::writable(TEST_RX_BUFFER, 16, None),
                TestDescriptor::writable(TEST_RX_SECOND_BUFFER, 8, None),
            ],
        );
        write_rx_available_heads(&mut memory, &[0, 1]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let error = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect_err("malformed later merged chain should fail");

        assert!(matches!(
            &error,
            VirtioNetworkDeviceNotificationError::RxQueueDispatch {
                source: VirtioNetworkRxQueueDispatchError::BufferParse {
                    descriptor_head: 1,
                    source: VirtioNetworkRxBufferParseError::BufferTooSmall { len: 8, min: 12 },
                    ..
                },
                ..
            }
        ));
        assert_eq!(source.consume_calls, 0);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 0);
        assert_eq!(read_guest_bytes(&memory, TEST_RX_BUFFER, 16), [0xa5; 16]);
        assert_eq!(
            read_guest_bytes(&memory, TEST_RX_SECOND_BUFFER, 8),
            [0xb5; 8]
        );
        assert_eq!(
            handler
                .activation_handler()
                .active_rx_dispatch_queue()
                .expect("RX queue should remain active")
                .available_ring()
                .next_avail(),
            0
        );
    }

    #[test]
    fn virtio_network_notifications_deliver_rx_packet_to_split_buffer() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0xa1, 0xa2, 0xa3]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[
                TestDescriptor::writable(TEST_RX_BUFFER, 8, Some(1)),
                TestDescriptor::writable(TEST_RX_SECOND_BUFFER, 1_518, None),
            ],
        );
        write_rx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("split RX queue notification should dispatch");

        let dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_packets(), 1);
        assert_eq!(read_rx_used_element(&memory, 0), (0, rx_used_len(3)));
        assert_eq!(read_guest_bytes(&memory, TEST_RX_BUFFER, 8), vec![0; 8]);
        assert_eq!(
            read_guest_bytes(&memory, TEST_RX_SECOND_BUFFER, 7),
            vec![0, 0, 1, 0, 0xa1, 0xa2, 0xa3]
        );
        assert!(notification.needs_queue_interrupt());
    }

    #[test]
    fn virtio_network_non_merged_guest_offloads_require_large_rx_buffer() {
        for guest_feature in [
            VIRTIO_NET_F_GUEST_TSO4,
            VIRTIO_NET_F_GUEST_TSO6,
            VIRTIO_NET_F_GUEST_UFO,
        ] {
            let mut memory = tx_frame_memory();
            let mut handler = network_activation_handler();
            let mut sink = RecordingTxPacketSink::default();
            let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x5a; 4]]);

            configure_network_handler_queues_with_features(
                &mut handler,
                (1_u32 << VIRTIO_NET_F_GUEST_CSUM) | (1_u32 << guest_feature),
            );
            activate_network_handler(&mut handler);
            write_rx_descriptors(
                &mut memory,
                &[TestDescriptor::writable(
                    TEST_RX_BUFFER,
                    u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                        .expect("legacy RX minimum should fit u32"),
                    None,
                )],
            );
            write_rx_available_heads(&mut memory, &[0]);
            handler
                .write_register(
                    VirtioMmioRegister::QueueNotify,
                    VIRTIO_NET_RX_QUEUE_INDEX
                        .try_into()
                        .expect("RX queue index should fit"),
                )
                .expect("large-minimum RX notification should write");

            let notification = handler
                .dispatch_network_queue_notifications_with_packet_io(
                    &mut memory,
                    &mut sink,
                    &mut source,
                )
                .expect("undersized non-merged buffer should be recorded");
            let dispatch = notification
                .rx_queue_dispatch()
                .expect("large-minimum RX dispatch should be present");

            assert_eq!(dispatch.delivered_packets(), 0, "feature {guest_feature}");
            assert_eq!(
                dispatch.buffer_parse_failures(),
                1,
                "feature {guest_feature}"
            );
            assert!(matches!(
                dispatch.first_buffer_parse_failure(),
                Some(VirtioNetworkRxBufferParseError::BufferTooSmall {
                    len: VIRTIO_NET_RX_MIN_BUFFER_SIZE,
                    min: VIRTIO_NET_MAX_BUFFER_SIZE,
                })
            ));
            assert_eq!(source.consume_calls, 0, "feature {guest_feature}");
            assert_eq!(source.remaining_packets(), 1, "feature {guest_feature}");
            assert_eq!(read_rx_used_index(&memory), 1, "feature {guest_feature}");
            assert_eq!(read_rx_used_element(&memory, 0), (0, 0));
        }
    }

    #[test]
    fn virtio_network_notifications_deliver_maximum_rx_packet() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let packet_len =
            usize::try_from(VIRTIO_NET_MAX_BUFFER_SIZE - u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                .expect("maximum RX payload should fit usize");
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0xa5; packet_len]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_MAX_BUFFER_SIZE).expect("RX maximum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("maximum RX packet should dispatch");

        let dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_packets(), 1);
        assert_eq!(
            dispatch
                .deliveries()
                .first()
                .expect("maximum RX delivery should be recorded")
                .bytes_written_to_guest(),
            u32::try_from(VIRTIO_NET_MAX_BUFFER_SIZE).expect("RX maximum should fit u32")
        );
        assert_eq!(
            read_rx_used_element(&memory, 0),
            (
                0,
                u32::try_from(VIRTIO_NET_MAX_BUFFER_SIZE).expect("RX maximum should fit u32"),
            )
        );
        assert_eq!(
            read_guest_bytes(&memory, TEST_RX_BUFFER, VIRTIO_NET_TX_HEADER_SIZE as usize),
            vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0]
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_BUFFER
                    .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                    .expect("first payload address should not overflow"),
                1,
            ),
            vec![0xa5]
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_BUFFER
                    .checked_add(VIRTIO_NET_MAX_BUFFER_SIZE - 1)
                    .expect("last payload address should not overflow"),
                1,
            ),
            vec![0xa5]
        );
        assert_eq!(source.consume_calls, 1);
        assert_eq!(source.remaining_packets(), 0);
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_notifications_deliver_multiple_rx_packets() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source =
            RecordingRxPacketSource::with_packets(vec![vec![0x11, 0x12], vec![0x21, 0x22, 0x23]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[
                TestDescriptor::writable(
                    TEST_RX_BUFFER,
                    u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                        .expect("RX minimum should fit u32"),
                    None,
                ),
                TestDescriptor::writable(
                    TEST_RX_SECOND_BUFFER,
                    u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                        .expect("RX minimum should fit u32"),
                    None,
                ),
            ],
        );
        write_rx_available_heads(&mut memory, &[0, 1]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("multiple RX packets should dispatch");

        let dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(dispatch.processed_buffers(), 2);
        assert_eq!(dispatch.delivered_packets(), 2);
        assert_eq!(dispatch.deliveries().len(), 2);
        assert_eq!(source.consume_calls, 2);
        assert_eq!(source.remaining_packets(), 0);
        assert_eq!(read_rx_used_index(&memory), 2);
        assert_eq!(read_rx_used_element(&memory, 0), (0, rx_used_len(2)));
        assert_eq!(read_rx_used_element(&memory, 1), (1, rx_used_len(3)));
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_BUFFER
                    .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                    .expect("first RX payload address should not overflow"),
                2,
            ),
            vec![0x11, 0x12]
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_SECOND_BUFFER
                    .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                    .expect("second RX payload address should not overflow"),
                3,
            ),
            vec![0x21, 0x22, 0x23]
        );
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_notifications_empty_rx_source_keeps_available_buffer() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::default();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE).expect("RX minimum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("empty RX source should dispatch as no-op");

        let dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(dispatch.processed_buffers(), 0);
        assert_eq!(source.peek_calls, 1);
        assert_eq!(source.consume_calls, 0);
        assert!(!notification.needs_queue_interrupt());
        assert_eq!(read_rx_used_index(&memory), 0);
        let active_rx_queue = handler
            .activation_handler()
            .active_rx_dispatch_queue()
            .expect("RX queue should remain active");
        assert_eq!(active_rx_queue.available_ring().next_avail(), 0);
        assert_eq!(active_rx_queue.used_ring().next_used(), 0);
    }

    #[test]
    fn virtio_network_notifications_empty_rx_queue_keeps_source_packet() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x55]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("empty RX queue should dispatch as no-op");

        let dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(dispatch.processed_buffers(), 0);
        assert_eq!(source.peek_calls, 1);
        assert_eq!(source.consume_calls, 0);
        assert_eq!(source.remaining_packets(), 1);
        assert!(!notification.needs_queue_interrupt());
        assert_eq!(read_rx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_network_notifications_malformed_rx_buffer_completes_without_consuming_packet() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x60, 0x61]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE).expect("RX minimum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("malformed RX buffer should complete with zero length");

        let dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_packets(), 0);
        assert_eq!(dispatch.buffer_parse_failures(), 1);
        assert!(matches!(
            dispatch.first_buffer_parse_failure(),
            Some(VirtioNetworkRxBufferParseError::BufferDescriptorReadOnly { index: 0 })
        ));
        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics.record_notification_dispatch(&notification);
        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_rx_queue_event_count(1)
                .with_no_rx_avail_buffer(1)
                .with_rx_fails(1)
        );
        assert_eq!(source.consume_calls, 0);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 1);
        assert_eq!(read_rx_used_element(&memory, 0), (0, 0));
        assert!(notification.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_notifications_rx_buffer_too_small_for_packet_retains_packet() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let packet = vec![0x77; 2_000];
        let mut source = RecordingRxPacketSource::with_packets(vec![packet]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE).expect("RX minimum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("too-small RX buffer should complete with zero length");

        let dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(dispatch.processed_buffers(), 1);
        assert_eq!(dispatch.delivered_packets(), 0);
        assert_eq!(dispatch.buffer_too_small_failures(), 1);
        let failure = dispatch
            .first_buffer_too_small()
            .expect("too-small metadata should be present");
        assert_eq!(failure.descriptor_head(), 0);
        assert_eq!(failure.len(), VIRTIO_NET_RX_MIN_BUFFER_SIZE);
        assert_eq!(failure.required_len(), u64::from(rx_used_len(2_000)));
        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics.record_notification_dispatch(&notification);
        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_rx_queue_event_count(1)
                .with_no_rx_avail_buffer(1)
                .with_rx_fails(1)
        );
        assert_eq!(source.consume_calls, 0);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 1);
        assert_eq!(read_rx_used_element(&memory, 0), (0, 0));
        assert!(notification.needs_queue_interrupt());
    }

    #[test]
    fn virtio_network_notifications_oversized_rx_packet_keeps_buffer_and_source() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let oversized_packet_len =
            usize::try_from(VIRTIO_NET_MAX_BUFFER_SIZE).expect("max packet size should fit usize");
        let mut source =
            RecordingRxPacketSource::with_packets(vec![vec![0x99; oversized_packet_len]]);
        let mut expected_backend_metrics = VirtioNetworkBackendMetrics::default();
        expected_backend_metrics.record_vmnet_read(1, Ok(1), Duration::from_micros(11));
        source.backend_metrics = expected_backend_metrics;

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE).expect("RX minimum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let error = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect_err("oversized RX packet should fail before consuming queue state");

        match &error {
            VirtioNetworkDeviceNotificationError::RxQueueDispatch {
                source: VirtioNetworkRxQueueDispatchError::PacketTooLarge { len, max, .. },
                ..
            } => {
                assert_eq!(
                    *len,
                    VIRTIO_NET_MAX_BUFFER_SIZE + u64::from(VIRTIO_NET_TX_HEADER_SIZE)
                );
                assert_eq!(*max, VIRTIO_NET_MAX_BUFFER_SIZE);
            }
            other => panic!("expected RX packet-too-large error, got {other:?}"),
        }
        let completed = error
            .completed_rx_dispatch()
            .expect("oversized packet error should preserve RX dispatch metadata");
        assert_eq!(completed.processed_buffers(), 0);
        assert_eq!(completed.delivered_packets(), 0);
        assert_eq!(completed.backend_metrics(), expected_backend_metrics);
        assert_eq!(
            source.backend_metrics,
            VirtioNetworkBackendMetrics::default(),
            "backend metrics must be consumed by the failing dispatch"
        );
        assert_eq!(source.consume_calls, 0);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 0);
        let active_rx_queue = handler
            .activation_handler()
            .active_rx_dispatch_queue()
            .expect("RX queue should remain active");
        assert_eq!(active_rx_queue.available_ring().next_avail(), 0);
        assert_eq!(active_rx_queue.used_ring().next_used(), 0);
        assert_eq!(read_interrupt_status(&handler), 0);
    }

    #[test]
    fn virtio_network_notifications_preserve_partial_rx_dispatch_error() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x10], vec![0x20]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE).expect("RX minimum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let error = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect_err("invalid second RX head should fail after partial dispatch");

        match &error {
            VirtioNetworkDeviceNotificationError::RxQueueDispatch {
                source: VirtioNetworkRxQueueDispatchError::AvailableRing { .. },
                ..
            } => {}
            other => panic!("expected RX available-ring dispatch error, got {other:?}"),
        }
        let completed = error
            .completed_rx_dispatch()
            .expect("partial RX dispatch metadata should be preserved");
        assert_eq!(completed.processed_buffers(), 1);
        assert_eq!(completed.delivered_packets(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(source.consume_calls, 1);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 1);
        assert_eq!(read_rx_used_element(&memory, 0), (0, rx_used_len(1)));
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_notifications_rx_source_failure_preserves_metadata_without_interrupt() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::failing_on_peek(1, vec![vec![0x90]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let error = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect_err("RX source failure should fail dispatch");

        match &error {
            VirtioNetworkDeviceNotificationError::RxQueueDispatch {
                source: VirtioNetworkRxQueueDispatchError::PacketSource { .. },
                ..
            } => {}
            other => panic!("expected RX source dispatch error, got {other:?}"),
        }
        let completed = error
            .completed_rx_dispatch()
            .expect("source error should preserve RX dispatch metadata");
        assert_eq!(completed.processed_buffers(), 0);
        assert_eq!(completed.source_failures(), 1);
        assert_eq!(
            completed
                .first_source_failure()
                .expect("source failure should be recorded")
                .message(),
            "test source failure on peek 1"
        );
        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics.record_rx_queue_dispatch(completed);
        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default().with_rx_fails(1)
        );
        assert_eq!(source.consume_calls, 0);
        assert_eq!(read_interrupt_status(&handler), 0);
    }

    #[test]
    fn virtio_network_notifications_preserve_completed_rx_when_later_tx_fails() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0xab]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE).expect("RX minimum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0]);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 4,
                None,
            )],
        );
        write_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let error = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect_err("invalid second TX head should fail after RX dispatch");

        match &error {
            VirtioNetworkDeviceNotificationError::TxQueueDispatch {
                source: VirtioNetworkTxQueueDispatchError::AvailableRing { .. },
                ..
            } => {}
            other => panic!("expected TX available-ring dispatch error, got {other:?}"),
        }
        assert_eq!(
            error.drained_notifications(),
            [VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_QUEUE_INDEX]
        );
        let completed_rx = error
            .completed_rx_dispatch()
            .expect("completed RX dispatch metadata should be preserved");
        assert_eq!(completed_rx.processed_buffers(), 1);
        assert_eq!(completed_rx.delivered_packets(), 1);
        let completed_tx = error
            .completed_tx_dispatch()
            .expect("partial TX dispatch metadata should be preserved");
        assert_eq!(completed_tx.processed_frames(), 1);
        assert_eq!(completed_tx.successful_frames(), 1);
        assert_eq!(source.consume_calls, 1);
        assert_eq!(source.remaining_packets(), 0);
        assert_eq!(read_rx_used_index(&memory), 1);
        assert_eq!(read_rx_used_element(&memory, 0), (0, rx_used_len(1)));
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_rx_rate_limit_throttles_without_side_effects_and_retries() {
        let now = Instant::now();
        let rate_limiter =
            NetworkRateLimiterConfig::new(Some(NetworkTokenBucketConfig::new(16, None, 100)), None);
        let mut memory = tx_frame_memory();
        let mut handler =
            network_activation_handler_with_rate_limiters_at(Some(rate_limiter), None, now);
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![
            vec![0x10, 0x11, 0x12, 0x13],
            vec![0x20, 0x21, 0x22, 0x23],
        ]);

        configure_network_handler_queues_for_capture(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[
                TestDescriptor::writable(
                    TEST_RX_BUFFER,
                    u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                        .expect("RX minimum should fit u32"),
                    None,
                ),
                TestDescriptor::writable(
                    TEST_RX_SECOND_BUFFER,
                    u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                        .expect("RX minimum should fit u32"),
                    None,
                ),
            ],
        );
        write_rx_available_heads(&mut memory, &[0, 1]);
        memory
            .write_slice(&[0xa5; 16], TEST_RX_SECOND_BUFFER)
            .expect("second RX buffer sentinel should write");

        let first = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                vec![VIRTIO_NET_RX_QUEUE_INDEX],
                &mut sink,
                &mut source,
                now,
            )
            .expect("initial rate-limited RX dispatch should complete");

        assert_eq!(first.drained_notifications(), [VIRTIO_NET_RX_QUEUE_INDEX]);
        let first_rx = first
            .rx_queue_dispatch()
            .expect("initial RX dispatch should be present");
        assert_eq!(first_rx.processed_buffers(), 1);
        assert_eq!(first_rx.delivered_packets(), 1);
        assert_eq!(first_rx.rate_limiter_throttled_packets(), 1);
        assert_eq!(
            first_rx.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            first.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(source.consume_calls, 1);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 1);
        assert_eq!(
            read_guest_bytes(&memory, TEST_RX_SECOND_BUFFER, 16),
            [0xa5; 16]
        );
        assert!(handler.has_pending_network_queue_work());

        let config = input()
            .with_guest_mac(test_guest_mac().to_string())
            .with_rx_rate_limiter(rate_limiter)
            .validate()
            .expect("RX capture config should validate");
        let profile = NetworkDeviceProfile::from_config(&config);
        let (captured, validation) = handler
            .capture_network_state_at(&config, profile, &memory, Some(4), now)
            .expect("cached rate-limited RX should be capture-ready");
        let (captured_again, validation_again) = handler
            .capture_network_state_at(&config, profile, &memory, Some(4), now)
            .expect("cached RX capture should be stable at one instant");
        assert_eq!(captured, captured_again);
        assert_eq!(validation, validation_again);
        assert_eq!(
            validation.source_rx_retry(),
            Some(VirtioNetworkRetryCaptureState::After {
                remaining_nanos: 100_000_000,
            })
        );
        assert_eq!(
            captured
                .device()
                .active_rx_queue()
                .map(|queue| (queue.next_available(), queue.next_used())),
            Some((1, 1))
        );
        assert!(captured.device().source_rx_retry_normalized());
        assert_eq!(
            captured.device().tx_retry(),
            VirtioNetworkRetryCaptureState::None
        );
        let (_, due_validation) = handler
            .capture_network_state_at(
                &config,
                profile,
                &memory,
                Some(4),
                now + Duration::from_millis(100),
            )
            .expect("due cached RX work should capture as immediate");
        assert_eq!(
            due_validation.source_rx_retry(),
            Some(VirtioNetworkRetryCaptureState::Immediate)
        );
        assert!(matches!(
            handler.capture_network_state_at(&config, profile, &memory, None, now),
            Err(VirtioNetworkDeviceCaptureError::PendingRxWithoutCache)
        ));
        assert!(matches!(
            handler.capture_network_state_at(&config, profile, &memory, Some(0), now),
            Err(VirtioNetworkDeviceCaptureError::CachedRxPacketInvalid)
        ));

        let limiter_after_first = handler
            .activation_handler()
            .rx_rate_limiter()
            .expect("RX limiter should remain configured")
            .clone();
        let repeated = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                Vec::new(),
                &mut sink,
                &mut source,
                now,
            )
            .expect("repeated RX throttle should remain retryable");
        let repeated_rx = repeated
            .rx_queue_dispatch()
            .expect("repeated RX dispatch should be present");
        assert_eq!(repeated_rx.processed_buffers(), 0);
        assert_eq!(repeated_rx.rate_limiter_throttled_packets(), 1);
        assert_eq!(
            repeated_rx.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(source.consume_calls, 1);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 1);
        assert_eq!(
            read_guest_bytes(&memory, TEST_RX_SECOND_BUFFER, 16),
            [0xa5; 16]
        );
        assert_eq!(
            handler
                .activation_handler()
                .rx_rate_limiter()
                .expect("RX limiter should remain configured"),
            &limiter_after_first
        );

        let retry = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                Vec::new(),
                &mut sink,
                &mut source,
                now + Duration::from_millis(100),
            )
            .expect("refilled RX work should retry without a new notification");

        assert!(retry.drained_notifications().is_empty());
        let retry_rx = retry
            .rx_queue_dispatch()
            .expect("retry RX dispatch should be present");
        assert_eq!(retry_rx.processed_buffers(), 1);
        assert_eq!(retry_rx.delivered_packets(), 1);
        assert_eq!(retry_rx.rate_limiter_throttled_packets(), 0);
        assert_eq!(retry_rx.rate_limiter_retry_after(), None);
        assert_eq!(retry.rate_limiter_retry_after(), None);
        assert_eq!(source.consume_calls, 2);
        assert_eq!(source.remaining_packets(), 0);
        assert_eq!(read_rx_used_index(&memory), 2);
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_SECOND_BUFFER
                    .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                    .expect("second RX payload address should not overflow"),
                4,
            ),
            [0x20, 0x21, 0x22, 0x23]
        );
        assert!(!handler.has_pending_network_queue_work());

        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics.record_notification_dispatch(&first);
        metrics.record_notification_dispatch(&repeated);
        metrics.record_notification_dispatch(&retry);
        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_rx_queue_event_count(1)
                .with_rx_bytes_count(32)
                .with_rx_packets_count(2)
                .with_rx_count(2)
                .with_rx_rate_limiter_event_count(2)
                .with_rx_rate_limiter_throttled(2)
        );
    }

    #[test]
    fn virtio_network_tx_rate_limit_throttles_without_side_effects_and_retries() {
        let now = Instant::now();
        let rate_limiter =
            NetworkRateLimiterConfig::new(Some(NetworkTokenBucketConfig::new(16, None, 100)), None);
        let mut memory = tx_frame_memory();
        let mut handler =
            network_activation_handler_with_rate_limiters_at(None, Some(rate_limiter), now);
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::default();

        configure_network_handler_queues_for_capture(&mut handler);
        activate_network_handler(&mut handler);
        write_two_tx_frames(&mut memory);

        let first = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                vec![VIRTIO_NET_TX_QUEUE_INDEX],
                &mut sink,
                &mut source,
                now,
            )
            .expect("initial rate-limited TX dispatch should complete");

        assert_eq!(first.drained_notifications(), [VIRTIO_NET_TX_QUEUE_INDEX]);
        let first_tx = first
            .tx_queue_dispatch()
            .expect("initial TX dispatch should be present");
        assert_eq!(first_tx.processed_frames(), 1);
        assert_eq!(first_tx.sink_successful_frames(), 1);
        assert_eq!(first_tx.rate_limiter_throttled_frames(), 1);
        assert_eq!(
            first_tx.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(
            first.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(sink.calls, 1);
        assert_eq!(sink.frame_heads, [0]);
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
        assert!(handler.has_pending_network_queue_work());

        let config = input()
            .with_guest_mac(test_guest_mac().to_string())
            .with_tx_rate_limiter(rate_limiter)
            .validate()
            .expect("TX capture config should validate");
        let profile = NetworkDeviceProfile::from_config(&config);
        let (captured, validation) = handler
            .capture_network_state_at(&config, profile, &memory, None, now)
            .expect("rate-limited TX should be capture-ready");
        let (captured_again, validation_again) = handler
            .capture_network_state_at(&config, profile, &memory, None, now)
            .expect("pending TX capture should be stable at one instant");
        assert_eq!(captured, captured_again);
        assert_eq!(validation, validation_again);
        assert_eq!(validation.source_rx_retry(), None);
        assert_eq!(
            captured
                .device()
                .active_tx_queue()
                .map(|queue| (queue.next_available(), queue.next_used())),
            Some((1, 1))
        );
        assert_eq!(
            captured.device().tx_retry(),
            VirtioNetworkRetryCaptureState::After {
                remaining_nanos: 100_000_000,
            }
        );
        assert!(!captured.device().source_rx_retry_normalized());
        let (due, _) = handler
            .capture_network_state_at(
                &config,
                profile,
                &memory,
                None,
                now + Duration::from_millis(100),
            )
            .expect("due TX work should capture as immediate");
        assert_eq!(
            due.device().tx_retry(),
            VirtioNetworkRetryCaptureState::Immediate
        );

        let limiter_after_first = handler
            .activation_handler()
            .tx_rate_limiter()
            .expect("TX limiter should remain configured")
            .clone();
        let repeated = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                Vec::new(),
                &mut sink,
                &mut source,
                now,
            )
            .expect("repeated TX throttle should remain retryable");
        let repeated_tx = repeated
            .tx_queue_dispatch()
            .expect("repeated TX dispatch should be present");
        assert_eq!(repeated_tx.processed_frames(), 0);
        assert_eq!(repeated_tx.rate_limiter_throttled_frames(), 1);
        assert_eq!(
            repeated_tx.rate_limiter_retry_after(),
            Some(Duration::from_millis(100))
        );
        assert_eq!(sink.calls, 1);
        assert_eq!(sink.frame_heads, [0]);
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(
            handler
                .activation_handler()
                .tx_rate_limiter()
                .expect("TX limiter should remain configured"),
            &limiter_after_first
        );

        let retry = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                Vec::new(),
                &mut sink,
                &mut source,
                now + Duration::from_millis(100),
            )
            .expect("refilled TX work should retry without a new notification");

        assert!(retry.drained_notifications().is_empty());
        let retry_tx = retry
            .tx_queue_dispatch()
            .expect("retry TX dispatch should be present");
        assert_eq!(retry_tx.processed_frames(), 1);
        assert_eq!(retry_tx.sink_successful_frames(), 1);
        assert_eq!(retry_tx.rate_limiter_throttled_frames(), 0);
        assert_eq!(retry_tx.rate_limiter_retry_after(), None);
        assert_eq!(retry.rate_limiter_retry_after(), None);
        assert_eq!(sink.calls, 2);
        assert_eq!(sink.frame_heads, [0, 2]);
        assert_eq!(
            sink.packets,
            [vec![0x10, 0x11, 0x12, 0x13], vec![0x20, 0x21, 0x22, 0x23]]
        );
        assert_eq!(read_tx_used_index(&memory), 2);
        assert_eq!(read_tx_used_element(&memory, 1), (2, 0));
        assert!(!handler.has_pending_network_queue_work());

        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics.record_notification_dispatch(&first);
        metrics.record_notification_dispatch(&repeated);
        metrics.record_notification_dispatch(&retry);
        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_no_tx_avail_buffer(1)
                .with_tx_queue_event_count(1)
                .with_tx_bytes_count(32)
                .with_tx_packets_count(2)
                .with_tx_count(2)
                .with_tx_rate_limiter_event_count(2)
                .with_tx_rate_limiter_throttled(2)
                .with_tx_remaining_reqs_count(1)
        );
    }

    #[test]
    fn virtio_network_tx_rate_limiter_refunds_successful_detours() {
        let now = Instant::now();
        let rate_limiter = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(16, None, 100)),
            Some(NetworkTokenBucketConfig::new(1, None, 100)),
        );
        let mut memory = tx_frame_memory();
        let mut handler =
            network_activation_handler_with_rate_limiters_at(None, Some(rate_limiter), now);
        let mut sink = RecordingTxPacketSink::detouring();
        let mut source = RecordingRxPacketSource::default();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_two_tx_frames(&mut memory);
        let initial_limiter = handler
            .activation_handler()
            .tx_rate_limiter()
            .expect("TX limiter should exist")
            .clone();

        let notification = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                vec![VIRTIO_NET_TX_QUEUE_INDEX],
                &mut sink,
                &mut source,
                now,
            )
            .expect("detoured TX frames should dispatch");

        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch should be present");
        assert_eq!(dispatch.processed_frames(), 2);
        assert_eq!(dispatch.sink_successful_frames(), 2);
        assert_eq!(dispatch.rate_limiter_throttled_frames(), 0);
        assert_eq!(sink.calls, 2);
        assert_eq!(read_tx_used_index(&memory), 2);
        assert_eq!(
            handler
                .activation_handler()
                .tx_rate_limiter()
                .expect("TX limiter should remain configured"),
            &initial_limiter
        );
        assert!(!handler.has_pending_network_queue_work());
    }

    #[test]
    fn virtio_network_tx_rate_limiter_charges_sink_failures() {
        let now = Instant::now();
        let rate_limiter =
            NetworkRateLimiterConfig::new(None, Some(NetworkTokenBucketConfig::new(1, None, 100)));
        let mut memory = tx_frame_memory();
        let mut handler =
            network_activation_handler_with_rate_limiters_at(None, Some(rate_limiter), now);
        let mut sink = RecordingTxPacketSink::failing_on(1);
        let mut source = RecordingRxPacketSource::default();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_two_tx_frames(&mut memory);

        let notification = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                vec![VIRTIO_NET_TX_QUEUE_INDEX],
                &mut sink,
                &mut source,
                now,
            )
            .expect("sink failure should remain a completed queue outcome");

        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch should be present");
        assert_eq!(dispatch.processed_frames(), 1);
        assert_eq!(dispatch.sink_failures(), 1);
        assert_eq!(dispatch.rate_limiter_throttled_frames(), 1);
        assert_eq!(sink.calls, 1);
        assert_eq!(read_tx_used_index(&memory), 1);
        assert!(handler.has_pending_network_queue_work());
    }

    #[test]
    fn virtio_network_notifications_dispatch_tx_frame_and_mark_interrupt() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 4, None),
            ],
        );
        write_tx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications(&mut memory)
            .expect("TX queue notification should dispatch");

        assert_eq!(
            notification.drained_notifications(),
            [VIRTIO_NET_TX_QUEUE_INDEX]
        );
        assert!(notification.needs_queue_interrupt());
        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(dispatch.processed_frames(), 1);
        assert_eq!(dispatch.successful_frames(), 1);
        assert_eq!(dispatch.parse_failures(), 0);
        assert_eq!(dispatch.sink_successful_frames(), 1);
        assert_eq!(dispatch.sink_failures(), 0);
        assert!(dispatch.first_parse_failure().is_none());
        assert!(dispatch.first_sink_failure().is_none());
        assert!(dispatch.needs_queue_interrupt());
        let frame = dispatch
            .frames()
            .first()
            .expect("parsed TX frame should be recorded");
        assert_eq!(frame.descriptor_head(), 0);
        assert_eq!(frame.payload_len(), 4);
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should remain active");
        assert_eq!(active_tx_queue.available_ring().next_avail(), 1);
        assert_eq!(active_tx_queue.used_ring().next_used(), 1);
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_network_notifications_deliver_tx_frame_to_sink() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, TEST_TX_PAYLOAD, &[0xde, 0xad, 0xbe, 0xef]);
        tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 4, None),
            ],
        );
        write_tx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect("TX queue notification should dispatch through sink");

        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(dispatch.processed_frames(), 1);
        assert_eq!(dispatch.successful_frames(), 1);
        assert_eq!(dispatch.sink_successful_frames(), 1);
        assert_eq!(dispatch.sink_successful_bytes(), 16);
        assert_eq!(dispatch.sink_failures(), 0);
        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics.record_notification_dispatch(&notification);
        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_tx_queue_event_count(1)
                .with_tx_bytes_count(16)
                .with_tx_packets_count(1)
                .with_tx_count(1)
        );
        assert_eq!(sink.calls, 1);
        assert_eq!(sink.frame_heads, [0]);
        assert_eq!(sink.packets, [vec![0xde, 0xad, 0xbe, 0xef]]);
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_notifications_post_tx_rx_hint_delivers_without_rx_notification() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0xca, 0xfe], vec![0xba]])
            .with_retry_after_tx_hint();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[
                TestDescriptor::writable(
                    TEST_RX_BUFFER,
                    u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                        .expect("RX minimum should fit u32"),
                    None,
                ),
                TestDescriptor::writable(
                    TEST_RX_SECOND_BUFFER,
                    u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                        .expect("RX minimum should fit u32"),
                    None,
                ),
            ],
        );
        write_rx_available_heads(&mut memory, &[0, 1]);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, TEST_TX_PAYLOAD, &[0x10]);
        tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 1, None),
            ],
        );
        write_tx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("TX queue notification should run post-TX RX retry");

        assert_eq!(
            notification.drained_notifications(),
            [VIRTIO_NET_TX_QUEUE_INDEX]
        );
        assert!(notification.rx_queue_dispatch().is_none());
        let tx_dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx_dispatch.processed_frames(), 1);
        assert_eq!(tx_dispatch.successful_frames(), 1);
        let rx_dispatch = notification
            .post_tx_rx_queue_dispatch()
            .expect("post-TX RX dispatch summary should be present");
        assert_eq!(rx_dispatch.processed_buffers(), 2);
        assert_eq!(rx_dispatch.delivered_packets(), 2);
        assert_eq!(source.peek_calls, 2);
        assert_eq!(source.consume_calls, 2);
        assert_eq!(source.remaining_packets(), 0);
        assert_eq!(read_rx_used_index(&memory), 2);
        assert_eq!(read_rx_used_element(&memory, 0), (0, rx_used_len(2)));
        assert_eq!(read_rx_used_element(&memory, 1), (1, rx_used_len(1)));
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_BUFFER
                    .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                    .expect("RX payload address should not overflow"),
                2,
            ),
            vec![0xca, 0xfe]
        );
        assert_eq!(
            read_guest_bytes(
                &memory,
                TEST_RX_SECOND_BUFFER
                    .checked_add(u64::from(VIRTIO_NET_TX_HEADER_SIZE))
                    .expect("second RX payload address should not overflow"),
                1,
            ),
            vec![0xba]
        );
        assert!(notification.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_notifications_post_tx_rx_without_hint_does_not_poll_source() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x21]]);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, TEST_TX_PAYLOAD, &[0x11]);
        tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 1, None),
            ],
        );
        write_tx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("TX queue notification should dispatch without RX retry");

        assert!(notification.rx_queue_dispatch().is_none());
        assert!(notification.post_tx_rx_queue_dispatch().is_none());
        assert_eq!(source.peek_calls, 0);
        assert_eq!(source.consume_calls, 0);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 0);
        assert_eq!(sink.calls, 1);
    }

    #[test]
    fn virtio_network_notifications_post_tx_rx_hint_without_rx_buffer_keeps_packet() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source =
            RecordingRxPacketSource::with_packets(vec![vec![0x31]]).with_retry_after_tx_hint();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, TEST_TX_PAYLOAD, &[0x12]);
        tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 1, None),
            ],
        );
        write_tx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("post-TX RX retry should no-op without RX buffers");

        let rx_dispatch = notification
            .post_tx_rx_queue_dispatch()
            .expect("post-TX RX dispatch summary should be present");
        assert_eq!(rx_dispatch.processed_buffers(), 0);
        assert_eq!(rx_dispatch.delivered_packets(), 0);
        assert_eq!(source.peek_calls, 1);
        assert_eq!(source.consume_calls, 0);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(read_rx_used_index(&memory), 0);
        assert!(notification.needs_queue_interrupt());
    }

    #[test]
    fn virtio_network_notifications_post_tx_rx_failure_preserves_tx_metadata() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::failing_on_peek(1, vec![vec![0x41]])
            .with_retry_after_tx_hint();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, TEST_TX_PAYLOAD, &[0x13]);
        tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 1, None),
            ],
        );
        write_tx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let error = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect_err("post-TX RX source failure should fail dispatch");

        match &error {
            VirtioNetworkDeviceNotificationError::RxQueueDispatch {
                source: VirtioNetworkRxQueueDispatchError::PacketSource { .. },
                ..
            } => {}
            other => panic!("expected post-TX RX source error, got {other:?}"),
        }
        assert_eq!(error.drained_notifications(), [VIRTIO_NET_TX_QUEUE_INDEX]);
        let tx_dispatch = error
            .completed_tx_dispatch()
            .expect("completed TX dispatch metadata should be preserved");
        assert_eq!(tx_dispatch.processed_frames(), 1);
        assert_eq!(tx_dispatch.successful_frames(), 1);
        assert!(error.completed_initial_rx_dispatch().is_none());
        let rx_dispatch = error
            .completed_rx_dispatch()
            .expect("failed post-TX RX dispatch metadata should be preserved");
        assert_eq!(rx_dispatch.processed_buffers(), 0);
        assert_eq!(rx_dispatch.source_failures(), 1);
        assert_eq!(source.peek_calls, 1);
        assert_eq!(source.consume_calls, 0);
        assert_eq!(source.remaining_packets(), 1);
        assert_eq!(sink.calls, 1);
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_notifications_post_tx_rx_keeps_initial_and_retry_metadata() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x51], vec![0x52, 0x53]])
            .with_retry_after_tx_hint()
            .with_empty_peeks_after_first_consume(1);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[
                TestDescriptor::writable(
                    TEST_RX_BUFFER,
                    u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                        .expect("RX minimum should fit u32"),
                    None,
                ),
                TestDescriptor::writable(
                    TEST_RX_SECOND_BUFFER,
                    u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE)
                        .expect("RX minimum should fit u32"),
                    None,
                ),
            ],
        );
        write_rx_available_heads(&mut memory, &[0, 1]);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, TEST_TX_PAYLOAD, &[0x14]);
        tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 1, None),
            ],
        );
        write_tx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("RX+TX notifications should keep initial and post-TX RX metadata");

        assert_eq!(
            notification.drained_notifications(),
            [VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_QUEUE_INDEX]
        );
        let initial_rx_dispatch = notification
            .rx_queue_dispatch()
            .expect("initial RX dispatch summary should be present");
        assert_eq!(initial_rx_dispatch.processed_buffers(), 1);
        assert_eq!(initial_rx_dispatch.delivered_packets(), 1);
        let tx_dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx_dispatch.processed_frames(), 1);
        assert_eq!(tx_dispatch.successful_frames(), 1);
        let post_tx_rx_dispatch = notification
            .post_tx_rx_queue_dispatch()
            .expect("post-TX RX dispatch summary should be present");
        assert_eq!(post_tx_rx_dispatch.processed_buffers(), 1);
        assert_eq!(post_tx_rx_dispatch.delivered_packets(), 1);
        assert_eq!(source.peek_calls, 3);
        assert_eq!(source.consume_calls, 2);
        assert_eq!(source.remaining_packets(), 0);
        assert_eq!(read_rx_used_index(&memory), 2);
        assert_eq!(read_rx_used_element(&memory, 0), (0, rx_used_len(1)));
        assert_eq!(read_rx_used_element(&memory, 1), (1, rx_used_len(2)));
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn virtio_network_notifications_empty_tx_queue_has_no_interrupt() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect("empty TX queue notification should dispatch as no-op");

        assert_eq!(
            notification.drained_notifications(),
            [VIRTIO_NET_TX_QUEUE_INDEX]
        );
        assert!(!notification.needs_queue_interrupt());
        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(dispatch.processed_frames(), 0);
        assert_eq!(dispatch.successful_frames(), 0);
        assert_eq!(dispatch.parse_failures(), 0);
        assert_eq!(dispatch.sink_successful_frames(), 0);
        assert_eq!(dispatch.sink_failures(), 0);
        assert!(dispatch.frames().is_empty());
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(sink.calls, 0);
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should remain active");
        assert_eq!(active_tx_queue.available_ring().next_avail(), 0);
        assert_eq!(active_tx_queue.used_ring().next_used(), 0);
        assert_eq!(read_tx_used_index(&memory), 0);
    }

    #[test]
    fn virtio_network_notifications_suppress_rx_interrupt_with_event_idx() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut source = RecordingRxPacketSource::with_packets(vec![vec![0x41]]);

        configure_network_handler_queues_with_event_idx(&mut handler);
        activate_network_handler(&mut handler);
        write_rx_descriptors(
            &mut memory,
            &[TestDescriptor::writable(
                TEST_RX_BUFFER,
                u32::try_from(VIRTIO_NET_RX_MIN_BUFFER_SIZE).expect("RX minimum should fit u32"),
                None,
            )],
        );
        write_rx_available_heads(&mut memory, &[0]);
        write_rx_available_used_event(&mut memory, 1);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_RX_QUEUE_INDEX
                    .try_into()
                    .expect("RX queue index should fit"),
            )
            .expect("RX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_packet_io(
                &mut memory,
                &mut sink,
                &mut source,
            )
            .expect("event-index RX notification should dispatch");

        let active_rx_queue = handler
            .activation_handler()
            .active_rx_dispatch_queue()
            .expect("RX dispatch queue should be active");
        assert!(active_rx_queue.event_idx_enabled());
        let rx_dispatch = notification
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx_dispatch.processed_buffers(), 1);
        assert_eq!(rx_dispatch.delivered_packets(), 1);
        assert!(!rx_dispatch.needs_queue_interrupt());
        assert!(!notification.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert_eq!(read_rx_used_index(&memory), 1);
        assert_eq!(read_rx_used_element(&memory, 0), (0, rx_used_len(1)));
        assert_eq!(source.consume_calls, 1);
        assert_eq!(sink.calls, 0);
    }

    #[test]
    fn virtio_network_notifications_suppress_tx_interrupt_with_event_idx() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();

        configure_network_handler_queues_with_event_idx(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, TEST_TX_PAYLOAD, &[0x20]);
        tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 1, None),
            ],
        );
        write_tx_available_heads(&mut memory, &[0]);
        write_tx_available_used_event(&mut memory, 1);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect("event-index TX notification should dispatch");

        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should be active");
        assert!(active_tx_queue.event_idx_enabled());
        let tx_dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(tx_dispatch.processed_frames(), 1);
        assert_eq!(tx_dispatch.successful_frames(), 1);
        assert!(!tx_dispatch.needs_queue_interrupt());
        assert!(!notification.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
        assert_eq!(sink.calls, 1);
    }

    #[test]
    fn virtio_network_notifications_preserve_suppressed_partial_tx_error_with_event_idx() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();
        let mut expected_backend_metrics = VirtioNetworkBackendMetrics::default();
        expected_backend_metrics.record_vmnet_write(1, Ok(1), Duration::from_micros(7));
        sink.backend_metrics = expected_backend_metrics;

        configure_network_handler_queues_with_event_idx(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, TEST_TX_PAYLOAD, &[0x30]);
        tx_descriptor_chain(
            &mut memory,
            &[
                TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(TEST_TX_PAYLOAD, 1, None),
            ],
        );
        write_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        write_tx_available_used_event(&mut memory, 1);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let error = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect_err("invalid second TX descriptor head should fail");

        assert!(matches!(
            error,
            VirtioNetworkDeviceNotificationError::TxQueueDispatch {
                source: VirtioNetworkTxQueueDispatchError::AvailableRing { .. },
                ..
            }
        ));
        let completed = error
            .completed_tx_dispatch()
            .expect("completed TX dispatch metadata should be preserved");
        assert_eq!(completed.processed_frames(), 1);
        assert_eq!(completed.successful_frames(), 1);
        assert_eq!(completed.backend_metrics(), expected_backend_metrics);
        assert_eq!(
            sink.backend_metrics,
            VirtioNetworkBackendMetrics::default(),
            "backend metrics must be consumed by the failing dispatch"
        );
        assert!(!completed.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
        assert_eq!(sink.calls, 1);
    }

    #[test]
    fn virtio_network_notifications_record_tx_parse_failure_and_complete_used_ring() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::default();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE - 1,
                None,
            )],
        );
        write_tx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect("malformed TX frame should still complete descriptor head");

        assert_eq!(
            notification.drained_notifications(),
            [VIRTIO_NET_TX_QUEUE_INDEX]
        );
        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(dispatch.processed_frames(), 1);
        assert_eq!(dispatch.successful_frames(), 0);
        assert_eq!(dispatch.parse_failures(), 1);
        assert_eq!(dispatch.sink_successful_frames(), 0);
        assert_eq!(dispatch.sink_failures(), 0);
        assert_eq!(dispatch.sink_successful_bytes(), 0);
        assert!(matches!(
            dispatch.first_parse_failure(),
            Some(VirtioNetworkTxFrameParseError::HeaderDescriptorTooSmall {
                index: 0,
                len: 11,
                min: 12,
            })
        ));
        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics.record_notification_dispatch(&notification);
        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_tx_queue_event_count(1)
                .with_tx_malformed_frames(1)
        );
        assert!(dispatch.first_sink_failure().is_none());
        assert!(dispatch.frames().is_empty());
        assert_eq!(sink.calls, 0);
        assert!(notification.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_network_notifications_record_sink_failure_and_continue_dispatch() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::failing_on(2);
        let first_payload = tx_payload_address_after_header(TEST_TX_HEADER);
        let second_payload = tx_payload_address_after_header(TEST_TX_PAYLOAD);
        let third_payload = tx_payload_address_after_header(TEST_TX_SECOND_PAYLOAD);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        write_tx_payload(&mut memory, first_payload, &[0x10, 0x11]);
        write_tx_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_TX_HEADER, VIRTIO_NET_TX_HEADER_SIZE + 2, None),
        );
        write_tx_header(&mut memory, TEST_TX_PAYLOAD);
        write_tx_payload(&mut memory, second_payload, &[0x20, 0x21, 0x22]);
        write_tx_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(TEST_TX_PAYLOAD, VIRTIO_NET_TX_HEADER_SIZE + 3, None),
        );
        write_tx_header(&mut memory, TEST_TX_SECOND_PAYLOAD);
        write_tx_payload(&mut memory, third_payload, &[0x30, 0x31, 0x32, 0x33]);
        write_tx_descriptor(
            &mut memory,
            2,
            TestDescriptor::readable(TEST_TX_SECOND_PAYLOAD, VIRTIO_NET_TX_HEADER_SIZE + 4, None),
        );
        write_tx_available_heads(&mut memory, &[0, 1, 2]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect("sink failure should not fail queue dispatch");

        let dispatch = notification
            .tx_queue_dispatch()
            .expect("TX dispatch summary should be present");
        assert_eq!(dispatch.processed_frames(), 3);
        assert_eq!(dispatch.successful_frames(), 3);
        assert_eq!(dispatch.parse_failures(), 0);
        assert_eq!(dispatch.sink_successful_frames(), 2);
        assert_eq!(dispatch.sink_failures(), 1);
        assert_eq!(dispatch.sink_successful_bytes(), 30);
        assert_eq!(
            dispatch
                .first_sink_failure()
                .expect("first sink failure should be recorded")
                .message(),
            "test sink failure on call 2"
        );
        assert_eq!(sink.calls, 3);
        assert_eq!(sink.frame_heads, [0, 1, 2]);
        assert_eq!(
            sink.packets,
            [vec![0x10, 0x11], vec![0x30, 0x31, 0x32, 0x33]]
        );
        let metrics = SharedNetworkInterfaceMetrics::default();
        metrics.record_notification_dispatch(&notification);
        assert_eq!(
            metrics.snapshot(),
            NetworkInterfaceMetrics::default()
                .with_tx_queue_event_count(1)
                .with_tx_bytes_count(30)
                .with_tx_fails(1)
                .with_tx_packets_count(2)
                .with_tx_count(2)
                .with_tx_remaining_reqs_count(3)
        );
        assert!(notification.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_tx_used_index(&memory), 3);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
        assert_eq!(read_tx_used_element(&memory, 1), (1, 0));
        assert_eq!(read_tx_used_element(&memory, 2), (2, 0));
    }

    #[test]
    fn virtio_network_staged_tx_maps_ordered_short_batch_results() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingStagedTxPacketSink::failing_flush_for(2);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_two_tx_frames(&mut memory);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect("staged TX queue should record a short-batch suffix failure");

        let dispatch = notification
            .tx_queue_dispatch()
            .expect("staged TX dispatch should be present");
        assert_eq!(dispatch.processed_frames(), 2);
        assert_eq!(dispatch.successful_frames(), 2);
        assert_eq!(dispatch.sink_successful_frames(), 1);
        assert_eq!(dispatch.sink_failures(), 1);
        assert_eq!(dispatch.sink_successful_bytes(), 16);
        assert_eq!(
            dispatch
                .first_sink_failure()
                .expect("staged suffix failure should be recorded")
                .message(),
            "test staged flush failure for head 2"
        );
        assert_eq!(sink.flush_calls, 1);
        assert_eq!(
            sink.flushed_packets,
            [vec![0x10, 0x11, 0x12, 0x13], vec![0x20, 0x21, 0x22, 0x23]]
        );
        assert_eq!(
            sink.events,
            ["stage:0", "commit:0", "stage:2", "commit:2", "flush:0,2"]
        );
        assert_eq!(read_tx_used_index(&memory), 2);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
        assert_eq!(read_tx_used_element(&memory, 1), (2, 0));
    }

    #[test]
    fn virtio_network_staged_tx_flushes_external_frames_before_immediate_detour() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingStagedTxPacketSink::detouring_after_flush(2);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_two_tx_frames(&mut memory);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let notification = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect("external TX should flush before an immediate detour");

        let dispatch = notification
            .tx_queue_dispatch()
            .expect("staged TX dispatch should be present");
        assert_eq!(dispatch.processed_frames(), 2);
        assert_eq!(dispatch.sink_successful_frames(), 2);
        assert_eq!(dispatch.sink_failures(), 0);
        assert_eq!(dispatch.sink_successful_bytes(), 32);
        assert_eq!(sink.flush_calls, 1);
        assert_eq!(sink.flushed_packets, [vec![0x10, 0x11, 0x12, 0x13]]);
        assert_eq!(
            sink.events,
            ["stage:0", "commit:0", "stage:2", "flush:0", "commit:2"]
        );
        assert_eq!(read_tx_used_index(&memory), 2);
    }

    #[test]
    fn virtio_network_staged_tx_flushes_committed_prefix_before_queue_error() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingStagedTxPacketSink::default();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_two_tx_frames(&mut memory);
        write_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let error = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect_err("invalid descriptor head should preserve the staged prefix");

        assert!(matches!(
            error,
            VirtioNetworkDeviceNotificationError::TxQueueDispatch {
                source: VirtioNetworkTxQueueDispatchError::AvailableRing { .. },
                ..
            }
        ));
        let completed = error
            .completed_tx_dispatch()
            .expect("staged prefix metadata should be retained");
        assert_eq!(completed.processed_frames(), 1);
        assert_eq!(completed.sink_successful_frames(), 1);
        assert_eq!(completed.sink_failures(), 0);
        assert_eq!(sink.flush_calls, 1);
        assert_eq!(sink.flushed_packets, [vec![0x10, 0x11, 0x12, 0x13]]);
        assert_eq!(sink.events, ["stage:0", "commit:0", "flush:0"]);
        assert_eq!(read_tx_used_index(&memory), 1);
    }

    #[test]
    fn virtio_network_staged_tx_discards_frame_when_used_publication_fails() {
        let mut memory = tx_frame_memory();
        write_two_tx_frames(&mut memory);
        write_tx_available_heads(&mut memory, &[0]);
        let queue_state = VirtioMmioQueueState::from_parts(
            TEST_QUEUE_SIZE,
            TEST_QUEUE_SIZE,
            true,
            TEST_TX_DESCRIPTOR_TABLE,
            TEST_TX_AVAILABLE_RING,
            GuestAddress::new(TEST_TX_MEMORY_SIZE),
        );
        let mut queue = VirtioNetworkTxQueue::from_mmio_queue_state(queue_state)
            .expect("unmapped used ring should remain a dispatch-time failure");
        let mut sink = RecordingStagedTxPacketSink::default();

        let error = queue
            .dispatch_with_sink(&mut memory, &mut sink)
            .expect_err("used-ring publication should fail after staging");

        assert!(matches!(
            error,
            VirtioNetworkTxQueueDispatchError::UsedRing {
                descriptor_head: 0,
                ..
            }
        ));
        assert_eq!(
            error
                .completed_dispatch()
                .expect("failed publication should retain empty dispatch metadata")
                .processed_frames(),
            0
        );
        assert_eq!(sink.discard_calls, 1);
        assert_eq!(sink.flush_calls, 0);
        assert!(sink.flushed_packets.is_empty());
        assert_eq!(sink.events, ["stage:0", "discard:0"]);
    }

    #[test]
    fn virtio_network_staged_tx_refunds_only_immediate_detours() {
        let now = Instant::now();
        let rate_limiter = NetworkRateLimiterConfig::new(
            Some(NetworkTokenBucketConfig::new(16, None, 100)),
            Some(NetworkTokenBucketConfig::new(1, None, 100)),
        );
        let mut memory = tx_frame_memory();
        let mut handler =
            network_activation_handler_with_rate_limiters_at(None, Some(rate_limiter), now);
        let mut sink = RecordingStagedTxPacketSink::detouring_after_flush(0);
        let mut source = RecordingRxPacketSource::default();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_two_tx_frames(&mut memory);

        let notification = handler
            .activation_handler_mut()
            .dispatch_drained_queue_notifications_with_packet_io_at(
                &mut memory,
                vec![VIRTIO_NET_TX_QUEUE_INDEX],
                &mut sink,
                &mut source,
                now,
            )
            .expect("refunded detour should leave capacity for the forwarded frame");

        let dispatch = notification
            .tx_queue_dispatch()
            .expect("staged TX dispatch should be present");
        assert_eq!(dispatch.processed_frames(), 2);
        assert_eq!(dispatch.sink_successful_frames(), 2);
        assert_eq!(dispatch.rate_limiter_throttled_frames(), 0);
        assert_eq!(sink.flush_calls, 1);
        assert_eq!(sink.flushed_packets, [vec![0x20, 0x21, 0x22, 0x23]]);
        assert_eq!(
            sink.events,
            ["stage:0", "commit:0", "stage:2", "commit:2", "flush:2"]
        );
        assert_eq!(read_tx_used_index(&memory), 2);
    }

    #[test]
    fn virtio_network_notifications_do_not_redispatch_after_drain() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 4,
                None,
            )],
        );
        write_tx_available_heads(&mut memory, &[0]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let first = handler
            .dispatch_network_queue_notifications(&mut memory)
            .expect("first TX dispatch should succeed");
        assert!(first.tx_queue_dispatch().is_some());
        acknowledge_queue_interrupt(&mut handler);

        let second = handler
            .dispatch_network_queue_notifications(&mut memory)
            .expect("second dispatch without notification should be a no-op");

        assert!(second.drained_notifications().is_empty());
        assert!(second.tx_queue_dispatch().is_none());
        assert!(!second.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should remain active");
        assert_eq!(active_tx_queue.available_ring().next_avail(), 1);
        assert_eq!(active_tx_queue.used_ring().next_used(), 1);
        assert_eq!(read_tx_used_index(&memory), 1);
    }

    #[test]
    fn virtio_network_notifications_preserve_partial_tx_dispatch_error() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 4,
                None,
            )],
        );
        write_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let error = handler
            .dispatch_network_queue_notifications(&mut memory)
            .expect_err("invalid second TX head should fail after partial dispatch");

        match &error {
            VirtioNetworkDeviceNotificationError::TxQueueDispatch {
                source: VirtioNetworkTxQueueDispatchError::AvailableRing { .. },
                ..
            } => {}
            other => panic!("expected TX available-ring dispatch error, got {other:?}"),
        }
        assert_eq!(error.drained_notifications(), [VIRTIO_NET_TX_QUEUE_INDEX]);
        let completed = error
            .completed_tx_dispatch()
            .expect("partial dispatch metadata should be preserved");
        assert_eq!(completed.processed_frames(), 1);
        assert_eq!(completed.successful_frames(), 1);
        assert_eq!(completed.sink_successful_frames(), 1);
        assert_eq!(completed.sink_failures(), 0);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
        let active_tx_queue = handler
            .activation_handler()
            .active_tx_dispatch_queue()
            .expect("TX dispatch queue should remain active");
        assert_eq!(active_tx_queue.available_ring().next_avail(), 1);
        assert_eq!(active_tx_queue.used_ring().next_used(), 1);
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_network_notifications_preserve_sink_failure_in_partial_dispatch_error() {
        let mut memory = tx_frame_memory();
        let mut handler = network_activation_handler();
        let mut sink = RecordingTxPacketSink::failing_on(1);

        configure_network_handler_queues(&mut handler);
        activate_network_handler(&mut handler);
        write_tx_header(&mut memory, TEST_TX_HEADER);
        tx_descriptor_chain(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 4,
                None,
            )],
        );
        write_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                VIRTIO_NET_TX_QUEUE_INDEX
                    .try_into()
                    .expect("TX queue index should fit"),
            )
            .expect("TX notification should write");

        let error = handler
            .dispatch_network_queue_notifications_with_tx_sink(&mut memory, &mut sink)
            .expect_err("invalid second TX head should fail after sink failure");

        match &error {
            VirtioNetworkDeviceNotificationError::TxQueueDispatch {
                source: VirtioNetworkTxQueueDispatchError::AvailableRing { .. },
                ..
            } => {}
            other => panic!("expected TX available-ring dispatch error, got {other:?}"),
        }
        assert_eq!(error.drained_notifications(), [VIRTIO_NET_TX_QUEUE_INDEX]);
        let completed = error
            .completed_tx_dispatch()
            .expect("partial dispatch metadata should be preserved");
        assert_eq!(completed.processed_frames(), 1);
        assert_eq!(completed.successful_frames(), 1);
        assert_eq!(completed.sink_successful_frames(), 0);
        assert_eq!(completed.sink_failures(), 1);
        assert_eq!(
            completed
                .first_sink_failure()
                .expect("first sink failure should be recorded")
                .message(),
            "test sink failure on call 1"
        );
        assert!(completed.needs_queue_interrupt());
        assert_eq!(sink.calls, 1);
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
        assert_eq!(read_tx_used_index(&memory), 1);
        assert_eq!(read_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn virtio_network_notifications_reject_unsupported_queue_index() {
        let mut memory = tx_frame_memory();
        let mut device = VirtioNetworkDevice::new();
        let mut sink = RecordingTxPacketSink::default();
        let registers = network_device_registers();
        let queues =
            configured_network_queues(Some(TEST_QUEUE_SIZE), true, Some(TEST_QUEUE_SIZE), true);
        device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect("network device should activate");

        let error = device
            .dispatch_drained_queue_notifications_with_tx_sink(&mut memory, vec![2], &mut sink)
            .expect_err("unsupported queue index should fail");

        match &error {
            VirtioNetworkDeviceNotificationError::UnsupportedQueue { queue_index, .. } => {
                assert_eq!(*queue_index, 2);
            }
            other => panic!("expected unsupported queue error, got {other:?}"),
        }
        assert_eq!(error.drained_notifications(), &[2]);
        assert!(error.completed_tx_dispatch().is_none());
    }
}
