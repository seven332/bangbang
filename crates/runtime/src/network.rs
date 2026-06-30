//! Backend-neutral network-interface configuration model.

use std::collections::TryReserveError;
use std::fmt;
use std::str::FromStr;

use crate::memory::{GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryRange};
use crate::mmio::{MmioAccessBytes, MmioAccessBytesError, MmioHandlerError};
use crate::virtio_mmio::{
    VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
    VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError, VirtioMmioDeviceConfigHandler,
    VirtioMmioQueueRegisterError, VirtioMmioQueueState, VirtioMmioRegisterHandler,
};
use crate::virtio_queue::{VirtqueueDescriptor, VirtqueueDescriptorChain};

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
pub const VIRTIO_NET_TX_HEADER_SIZE: u32 = 12;
pub const VIRTIO_NET_MAX_BUFFER_SIZE: u64 = 65_562;
pub const VIRTIO_NET_RX_MIN_BUFFER_SIZE: u64 = 1_526;

const VIRTIO_NET_RX_QUEUE_INDEX_U32: u32 = 0;
const VIRTIO_NET_TX_QUEUE_INDEX_U32: u32 = 1;

pub type VirtioNetworkMmioHandler =
    VirtioMmioRegisterHandler<VirtioNetworkConfigSpace, VirtioNetworkDevice>;

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
}

impl VirtioNetworkTxFrame {
    pub fn parse(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioNetworkTxFrameParseError> {
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
            return Err(VirtioNetworkTxFrameParseError::MissingPayload {
                descriptor_head: header_descriptor.index(),
            });
        }

        Ok(Self {
            descriptor_head: header_descriptor.index(),
            header,
            payload_segments,
            payload_len,
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

    pub fn frame_len(&self) -> u64 {
        u64::from(VIRTIO_NET_TX_HEADER_SIZE) + self.payload_len
    }
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
        let descriptor_head = chain
            .descriptors()
            .first()
            .ok_or(VirtioNetworkRxBufferParseError::DescriptorChainTooShort {
                expected: 1,
                actual: chain.len(),
            })?
            .index();
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

        if len < VIRTIO_NET_RX_MIN_BUFFER_SIZE {
            return Err(VirtioNetworkRxBufferParseError::BufferTooSmall {
                len,
                min: VIRTIO_NET_RX_MIN_BUFFER_SIZE,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtioNetworkDevice {
    active_rx_queue: Option<VirtioMmioQueueState>,
    active_tx_queue: Option<VirtioMmioQueueState>,
}

impl VirtioNetworkDevice {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_activated(&self) -> bool {
        self.active_rx_queue.is_some() && self.active_tx_queue.is_some()
    }

    pub const fn active_rx_queue(&self) -> Option<VirtioMmioQueueState> {
        self.active_rx_queue
    }

    pub const fn active_tx_queue(&self) -> Option<VirtioMmioQueueState> {
        self.active_tx_queue
    }

    pub fn activate_network(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioNetworkDeviceActivationError> {
        if self.is_activated() {
            return Err(VirtioNetworkDeviceActivationError::AlreadyActive);
        }

        let active_rx_queue =
            active_network_queue_state(activation, VIRTIO_NET_RX_QUEUE_INDEX_U32)?;
        let active_tx_queue =
            active_network_queue_state(activation, VIRTIO_NET_TX_QUEUE_INDEX_U32)?;

        self.active_rx_queue = Some(active_rx_queue);
        self.active_tx_queue = Some(active_tx_queue);

        Ok(())
    }

    pub fn reset(&mut self) {
        self.active_rx_queue = None;
        self.active_tx_queue = None;
    }

    fn dispatch_drained_queue_notifications(
        &mut self,
        drained_notifications: Vec<usize>,
    ) -> Result<VirtioNetworkDeviceNotificationDispatch, VirtioNetworkDeviceNotificationError> {
        if drained_notifications.is_empty() {
            return Ok(VirtioNetworkDeviceNotificationDispatch::new(
                drained_notifications,
            ));
        }

        if !self.is_activated() {
            return Err(VirtioNetworkDeviceNotificationError::Inactive {
                drained_notifications,
            });
        }

        if let Some(queue_index) = drained_notifications.iter().copied().find(|queue_index| {
            *queue_index != VIRTIO_NET_RX_QUEUE_INDEX && *queue_index != VIRTIO_NET_TX_QUEUE_INDEX
        }) {
            return Err(VirtioNetworkDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        match drained_notifications.first().copied() {
            Some(queue_index) => Err(
                VirtioNetworkDeviceNotificationError::UnsupportedQueueExecution {
                    drained_notifications,
                    queue_index,
                },
            ),
            None => Ok(VirtioNetworkDeviceNotificationDispatch::new(
                drained_notifications,
            )),
        }
    }
}

impl<C: VirtioMmioDeviceConfigHandler> VirtioMmioRegisterHandler<C, VirtioNetworkDevice> {
    pub fn dispatch_network_queue_notifications(
        &mut self,
    ) -> Result<VirtioNetworkDeviceNotificationDispatch, VirtioNetworkDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        self.activation_handler_mut()
            .dispatch_drained_queue_notifications(drained_notifications)
    }
}

impl VirtioMmioDeviceActivationHandler for VirtioNetworkDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        self.activate_network(activation).map_err(Into::into)
    }

    fn reset(&mut self) {
        VirtioNetworkDevice::reset(self);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedNetworkDevice {
    iface_id: String,
    host_dev_name: String,
    config_space: VirtioNetworkConfigSpace,
    device: VirtioNetworkDevice,
}

impl PreparedNetworkDevice {
    fn from_config(config: &NetworkInterfaceConfig) -> Self {
        Self {
            iface_id: config.iface_id().to_string(),
            host_dev_name: config.host_dev_name().to_string(),
            config_space: VirtioNetworkConfigSpace::new(config.guest_mac()),
            device: VirtioNetworkDevice::new(),
        }
    }

    pub fn iface_id(&self) -> &str {
        &self.iface_id
    }

    pub fn host_dev_name(&self) -> &str {
        &self.host_dev_name
    }

    pub const fn config_space(&self) -> VirtioNetworkConfigSpace {
        self.config_space
    }

    pub const fn device(&self) -> &VirtioNetworkDevice {
        &self.device
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PreparedNetworkDevices {
    devices: Vec<PreparedNetworkDevice>,
}

impl PreparedNetworkDevices {
    pub fn from_configs(
        configs: &NetworkInterfaceConfigs,
    ) -> Result<Self, PreparedNetworkDeviceError> {
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(configs.as_slice().len())
            .map_err(|source| PreparedNetworkDeviceError::AllocateDevices { source })?;

        for config in configs.as_slice() {
            devices.push(PreparedNetworkDevice::from_config(config));
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
}

#[derive(Debug)]
pub enum PreparedNetworkDeviceError {
    AllocateDevices { source: TryReserveError },
}

impl fmt::Display for PreparedNetworkDeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocateDevices { source } => {
                write!(f, "failed to allocate prepared network devices: {source}")
            }
        }
    }
}

impl std::error::Error for PreparedNetworkDeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateDevices { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioNetworkDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
}

impl VirtioNetworkDeviceNotificationDispatch {
    const fn new(drained_notifications: Vec<usize>) -> Self {
        Self {
            drained_notifications,
        }
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
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
    UnsupportedQueueExecution {
        drained_notifications: Vec<usize>,
        queue_index: usize,
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
            | Self::UnsupportedQueueExecution {
                drained_notifications,
                ..
            } => drained_notifications,
        }
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
            Self::UnsupportedQueueExecution { queue_index, .. } => {
                write!(
                    f,
                    "virtio-net queue {queue_index} notification execution is not supported"
                )
            }
        }
    }
}

impl std::error::Error for VirtioNetworkDeviceNotificationError {}

#[derive(Debug)]
pub enum VirtioNetworkDeviceActivationError {
    AlreadyActive,
    QueueMetadata {
        queue_index: u32,
        source: VirtioMmioQueueRegisterError,
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

    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};
    use crate::mmio::{MmioAccess, MmioAccessBytes, MmioBus, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation,
        VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
        VirtioMmioDeviceRegisters, VirtioMmioQueueRegisterError, VirtioMmioQueueRegisters,
        VirtioMmioRegister, VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
        VirtqueueDescriptorChain, read_descriptor_chain,
    };

    use super::{
        GuestMacAddress, InterfaceIdSource, NetworkInterfaceConfig, NetworkInterfaceConfigError,
        NetworkInterfaceConfigInput, NetworkInterfaceConfigs, PreparedNetworkDevices,
        VIRTIO_FEATURE_VERSION_1, VIRTIO_NET_CONFIG_MAC_SIZE, VIRTIO_NET_DEVICE_ID,
        VIRTIO_NET_F_MAC, VIRTIO_NET_MAX_BUFFER_SIZE, VIRTIO_NET_QUEUE_COUNT,
        VIRTIO_NET_QUEUE_SIZE, VIRTIO_NET_QUEUE_SIZES, VIRTIO_NET_RX_MIN_BUFFER_SIZE,
        VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_HEADER_SIZE, VIRTIO_NET_TX_QUEUE_INDEX,
        VIRTIO_RING_FEATURE_EVENT_IDX, VirtioNetworkConfigSpace, VirtioNetworkDevice,
        VirtioNetworkDeviceActivationError, VirtioNetworkDeviceNotificationError,
        VirtioNetworkMmioHandler, VirtioNetworkRxBuffer, VirtioNetworkRxBufferParseError,
        VirtioNetworkTxFrame, VirtioNetworkTxFrameParseError,
    };

    const TEST_MMIO_BASE: GuestAddress = GuestAddress::new(0x1000);
    const TEST_RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x10_0000);
    const TEST_RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x11_0000);
    const TEST_RX_USED_RING: GuestAddress = GuestAddress::new(0x12_0000);
    const TEST_TX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x20_0000);
    const TEST_TX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x21_0000);
    const TEST_TX_USED_RING: GuestAddress = GuestAddress::new(0x22_0000);
    const TEST_TX_HEADER: GuestAddress = GuestAddress::new(0x23_0000);
    const TEST_TX_PAYLOAD: GuestAddress = GuestAddress::new(0x24_0000);
    const TEST_TX_SECOND_PAYLOAD: GuestAddress = GuestAddress::new(0x25_0000);
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

    fn network_activation_handler() -> VirtioNetworkMmioHandler {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()));
        VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_NET_DEVICE_ID,
            config.available_features(),
            &VIRTIO_NET_QUEUE_SIZES,
            config,
            VirtioNetworkDevice::new(),
        )
        .expect("network activation handler should build")
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

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test address should fit in queue low register")
    }

    fn network_device_registers() -> VirtioMmioDeviceRegisters {
        let config = VirtioNetworkConfigSpace::new(Some(test_guest_mac()));
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

    fn write_tx_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_TX_DESCRIPTOR_TABLE
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
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX);

        assert_eq!(devices.len(), 1);
        assert_eq!(device.iface_id(), "eth0");
        assert_eq!(device.host_dev_name(), "tap0");
        assert_eq!(device.config_space().guest_mac(), None);
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
                | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX)
                | virtio_feature_bit(VIRTIO_NET_F_MAC)
        );
        assert!(!device.device().is_activated());
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
        assert_eq!(VIRTIO_NET_TX_HEADER_SIZE, 12);
        assert_eq!(VIRTIO_NET_MAX_BUFFER_SIZE, 65_562);
        assert_eq!(VIRTIO_NET_RX_MIN_BUFFER_SIZE, 1_526);
    }

    #[test]
    fn virtio_network_tx_frame_parser_accepts_single_descriptor_frame() {
        let mut memory = tx_frame_memory();
        write_tx_header(&mut memory, TEST_TX_HEADER);
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
    fn virtio_network_notifications_without_pending_work_are_noop() {
        let mut device = VirtioNetworkDevice::new();

        let dispatch = device
            .dispatch_drained_queue_notifications(Vec::new())
            .expect("empty notification drain should be a no-op");

        assert_eq!(dispatch.drained_notifications(), &[]);
    }

    #[test]
    fn virtio_network_notifications_reject_inactive_device_with_drained_metadata() {
        let mut device = VirtioNetworkDevice::new();

        let error = device
            .dispatch_drained_queue_notifications(vec![VIRTIO_NET_RX_QUEUE_INDEX])
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
        assert!(std::error::Error::source(&error).is_none());
    }

    #[test]
    fn virtio_network_notifications_reject_unsupported_queue_execution_and_drain() {
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

        let error = handler
            .dispatch_network_queue_notifications()
            .expect_err("network packet execution is not supported yet");

        match &error {
            VirtioNetworkDeviceNotificationError::UnsupportedQueueExecution {
                queue_index, ..
            } => assert_eq!(*queue_index, VIRTIO_NET_RX_QUEUE_INDEX),
            other => panic!("expected unsupported queue execution, got {other:?}"),
        }
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_NET_RX_QUEUE_INDEX, VIRTIO_NET_TX_QUEUE_INDEX]
        );
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn virtio_network_notifications_reject_unsupported_queue_index() {
        let mut device = VirtioNetworkDevice::new();
        let registers = network_device_registers();
        let queues =
            configured_network_queues(Some(TEST_QUEUE_SIZE), true, Some(TEST_QUEUE_SIZE), true);
        device
            .activate_network(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect("network device should activate");

        let error = device
            .dispatch_drained_queue_notifications(vec![2])
            .expect_err("unsupported queue index should fail");

        match &error {
            VirtioNetworkDeviceNotificationError::UnsupportedQueue { queue_index, .. } => {
                assert_eq!(*queue_index, 2);
            }
            other => panic!("expected unsupported queue error, got {other:?}"),
        }
        assert_eq!(error.drained_notifications(), &[2]);
    }
}
