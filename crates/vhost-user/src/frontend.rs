//! Typed Firecracker-shaped vhost-user frontend state machine.

use std::fmt;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::error::VhostUserError;
use crate::message::{
    MAX_ATTACHED_FDS, MAX_BODY_BYTES, MemoryRegionWire, Request, VringAddressWire,
    decode_config_reply, decode_u64, encode_config_request, encode_memory_table, encode_u64,
    encode_vring_address, encode_vring_state,
};
use crate::notifier::{BackendCallEndpoint, BackendKickEndpoint};
use crate::transport::{Transport, TransportError};

/// Virtio read-only block feature bit.
pub const VIRTIO_BLK_F_RO: u64 = 1 << 5;
/// Virtio block flush feature bit.
pub const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
/// Virtio event-index feature bit.
pub const VIRTIO_F_EVENT_IDX: u64 = 1 << 29;
/// Vhost-user protocol-feature negotiation bit in the virtio feature mask.
pub const VHOST_USER_F_PROTOCOL_FEATURES: u64 = 1 << 30;
/// Virtio version-1 feature bit.
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// Vhost-user protocol reply-acknowledgement feature bit.
pub const VHOST_USER_PROTOCOL_F_REPLY_ACK: u64 = 1 << 3;
/// Vhost-user device-configuration feature bit.
pub const VHOST_USER_PROTOCOL_F_CONFIG: u64 = 1 << 9;

/// Complete reviewed Firecracker v1.16 block virtio feature subset.
pub const SUPPORTED_VIRTIO_FEATURES: u64 = VIRTIO_BLK_F_RO
    | VIRTIO_BLK_F_FLUSH
    | VIRTIO_F_EVENT_IDX
    | VHOST_USER_F_PROTOCOL_FEATURES
    | VIRTIO_F_VERSION_1;

/// Complete reviewed protocol feature subset.
pub const SUPPORTED_PROTOCOL_FEATURES: u64 =
    VHOST_USER_PROTOCOL_F_REPLY_ACK | VHOST_USER_PROTOCOL_F_CONFIG;

const MEMORY_ALIGNMENT: u64 = 4096;
const MAX_QUEUE_COUNT: u16 = 256;
const MAX_QUEUE_SIZE: u16 = 0x8000;
const MAX_CONFIG_END: u32 = 0x1000;
const MAX_OPERATION_TIMEOUT: Duration = Duration::from_secs(60);

/// Validated immutable frontend construction limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VhostUserFrontendOptions {
    queue_count: u16,
    max_queue_size: u16,
    max_memory_regions: u8,
    allowed_virtio_features: u64,
    operation_timeout: Duration,
}

impl VhostUserFrontendOptions {
    /// Creates reviewed frontend limits.
    pub fn new(
        queue_count: u16,
        max_queue_size: u16,
        max_memory_regions: u8,
        allowed_virtio_features: u64,
        operation_timeout: Duration,
    ) -> Result<Self, VhostUserError> {
        if queue_count == 0
            || queue_count > MAX_QUEUE_COUNT
            || max_queue_size == 0
            || max_queue_size > MAX_QUEUE_SIZE
            || !max_queue_size.is_power_of_two()
            || max_memory_regions == 0
            || usize::from(max_memory_regions) > MAX_ATTACHED_FDS
            || allowed_virtio_features & !SUPPORTED_VIRTIO_FEATURES != 0
            || allowed_virtio_features & VHOST_USER_F_PROTOCOL_FEATURES == 0
            || operation_timeout.is_zero()
            || operation_timeout > MAX_OPERATION_TIMEOUT
        {
            return Err(VhostUserError::InvalidConfiguration);
        }
        Ok(Self {
            queue_count,
            max_queue_size,
            max_memory_regions,
            allowed_virtio_features,
            operation_timeout,
        })
    }

    /// Returns the exact pinned one-queue/256-entry block limits.
    pub fn firecracker_block(operation_timeout: Duration) -> Result<Self, VhostUserError> {
        Self::new(
            1,
            256,
            u8::try_from(MAX_ATTACHED_FDS).map_err(|_| VhostUserError::InvalidConfiguration)?,
            SUPPORTED_VIRTIO_FEATURES,
            operation_timeout,
        )
    }

    /// Number of queues the frontend is allowed to configure.
    pub const fn queue_count(self) -> u16 {
        self.queue_count
    }

    /// Maximum accepted power-of-two queue size.
    pub const fn max_queue_size(self) -> u16 {
        self.max_queue_size
    }

    /// Maximum accepted memory-region count.
    pub const fn max_memory_regions(self) -> u8 {
        self.max_memory_regions
    }
}

/// Closed configuration-read flags used by pinned Firecracker block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VhostUserConfigFlags {
    /// Ask the backend for fields writable by the frontend/driver.
    Writable,
}

impl VhostUserConfigFlags {
    const fn bits(self) -> u32 {
        match self {
            Self::Writable => 0x1,
        }
    }
}

/// Validated backend device-configuration bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct VhostUserConfig {
    offset: u32,
    flags: VhostUserConfigFlags,
    bytes: Vec<u8>,
}

impl fmt::Debug for VhostUserConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VhostUserConfig")
            .field("offset", &self.offset)
            .field("flags", &self.flags)
            .field("bytes", &"redacted")
            .field("length", &self.bytes.len())
            .finish()
    }
}

impl VhostUserConfig {
    /// Configuration-space offset returned by the backend.
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Exact validated configuration bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// One validated descriptor-backed guest-memory region.
#[derive(Clone, Copy)]
pub struct VhostUserMemoryRegion<'descriptor> {
    guest_phys_addr: u64,
    memory_size: u64,
    userspace_addr: u64,
    mmap_offset: u64,
    backing: BorrowedFd<'descriptor>,
}

impl fmt::Debug for VhostUserMemoryRegion<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("VhostUserMemoryRegion")
            .field(&"redacted")
            .finish()
    }
}

impl VhostUserMemoryRegion<'_> {
    /// Creates one page-aligned, nonempty, checked region description.
    pub fn new(
        guest_phys_addr: u64,
        memory_size: u64,
        userspace_addr: u64,
        mmap_offset: u64,
        backing: BorrowedFd<'_>,
    ) -> Result<VhostUserMemoryRegion<'_>, VhostUserError> {
        if memory_size == 0
            || userspace_addr == 0
            || !guest_phys_addr.is_multiple_of(MEMORY_ALIGNMENT)
            || !memory_size.is_multiple_of(MEMORY_ALIGNMENT)
            || !userspace_addr.is_multiple_of(MEMORY_ALIGNMENT)
            || !mmap_offset.is_multiple_of(MEMORY_ALIGNMENT)
            || guest_phys_addr.checked_add(memory_size).is_none()
            || userspace_addr.checked_add(memory_size).is_none()
            || mmap_offset.checked_add(memory_size).is_none()
        {
            return Err(VhostUserError::InvalidConfiguration);
        }
        Ok(VhostUserMemoryRegion {
            guest_phys_addr,
            memory_size,
            userspace_addr,
            mmap_offset,
            backing,
        })
    }

    fn wire(self) -> MemoryRegionWire {
        MemoryRegionWire {
            guest_phys_addr: self.guest_phys_addr,
            memory_size: self.memory_size,
            userspace_addr: self.userspace_addr,
            mmap_offset: self.mmap_offset,
        }
    }
}

/// Validated frontend virtual addresses for one virtqueue.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VhostUserVringAddress {
    descriptor: u64,
    used: u64,
    available: u64,
}

impl fmt::Debug for VhostUserVringAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("VhostUserVringAddress")
            .field(&"redacted")
            .finish()
    }
}

impl VhostUserVringAddress {
    /// Creates ring addresses with the protocol-required alignments.
    pub fn new(descriptor: u64, used: u64, available: u64) -> Result<Self, VhostUserError> {
        if descriptor == 0
            || used == 0
            || available == 0
            || !descriptor.is_multiple_of(16)
            || !used.is_multiple_of(4)
            || !available.is_multiple_of(2)
        {
            return Err(VhostUserError::InvalidConfiguration);
        }
        Ok(Self {
            descriptor,
            used,
            available,
        })
    }
}

/// Coarse redacted protocol state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VhostUserFrontendState {
    /// No request has been sent.
    New,
    /// Ownership was claimed.
    Owner,
    /// Backend virtio features are known.
    Features,
    /// Protocol features are known or selected.
    ProtocolFeatures,
    /// Guest-acked virtio features were installed.
    FeaturesSet,
    /// The memory table was installed and queues are not yet configured.
    MemoryTable,
    /// At least one queue setup operation remains.
    QueueSetup,
    /// Every configured queue is enabled.
    Ready,
    /// Framing is permanently terminal after a stream failure.
    Poisoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueueStage {
    Empty,
    Size,
    Address,
    Base,
    Call,
    Kick,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueueState {
    stage: QueueStage,
    size: Option<u16>,
}

impl QueueState {
    const EMPTY: Self = Self {
        stage: QueueStage::Empty,
        size: None,
    };
}

/// One synchronous closed vhost-user frontend.
pub struct VhostUserFrontend {
    transport: Transport,
    options: VhostUserFrontendOptions,
    owner_set: bool,
    advertised_features: Option<u64>,
    advertised_protocol_features: Option<u64>,
    negotiated_protocol_features: Option<u64>,
    acked_features: Option<u64>,
    memory_set: bool,
    queues: Vec<QueueState>,
    poisoned: bool,
}

impl fmt::Debug for VhostUserFrontend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VhostUserFrontend")
            .field("state", &self.state())
            .field("options", &self.options)
            .field("peer_features", &"redacted")
            .finish()
    }
}

impl VhostUserFrontend {
    /// Adopts an already connected Unix stream under the supplied strict limits.
    pub fn new(
        stream: UnixStream,
        options: VhostUserFrontendOptions,
    ) -> Result<Self, VhostUserError> {
        let transport = Transport::new(stream, options.operation_timeout)?;
        Ok(Self {
            transport,
            options,
            owner_set: false,
            advertised_features: None,
            advertised_protocol_features: None,
            negotiated_protocol_features: None,
            acked_features: None,
            memory_set: false,
            queues: vec![QueueState::EMPTY; usize::from(options.queue_count)],
            poisoned: false,
        })
    }

    /// Returns the coarse value-redacted state.
    pub fn state(&self) -> VhostUserFrontendState {
        if self.poisoned {
            return VhostUserFrontendState::Poisoned;
        }
        if self.memory_set
            && self
                .queues
                .iter()
                .all(|queue| queue.stage == QueueStage::Enabled)
        {
            return VhostUserFrontendState::Ready;
        }
        if self.memory_set
            && self
                .queues
                .iter()
                .any(|queue| queue.stage != QueueStage::Empty)
        {
            return VhostUserFrontendState::QueueSetup;
        }
        if self.memory_set {
            return VhostUserFrontendState::MemoryTable;
        }
        if self.acked_features.is_some() {
            return VhostUserFrontendState::FeaturesSet;
        }
        if self.negotiated_protocol_features.is_some()
            || self.advertised_protocol_features.is_some()
        {
            return VhostUserFrontendState::ProtocolFeatures;
        }
        if self.advertised_features.is_some() {
            return VhostUserFrontendState::Features;
        }
        if self.owner_set {
            return VhostUserFrontendState::Owner;
        }
        VhostUserFrontendState::New
    }

    /// Claims frontend ownership exactly once.
    pub fn set_owner(&mut self) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        if self.owner_set {
            return Err(VhostUserError::InvalidState);
        }
        self.send_without_ack(Request::SetOwner, &[], &[])?;
        self.owner_set = true;
        Ok(())
    }

    /// Queries backend virtio features exactly once.
    pub fn get_features(&mut self) -> Result<u64, VhostUserError> {
        self.ensure_live()?;
        if !self.owner_set || self.advertised_features.is_some() {
            return Err(VhostUserError::InvalidState);
        }
        let features = self.query_u64(Request::GetFeatures)?;
        self.advertised_features = Some(features);
        Ok(features)
    }

    /// Queries backend protocol features after bit 30 is advertised.
    pub fn get_protocol_features(&mut self) -> Result<u64, VhostUserError> {
        self.ensure_live()?;
        let advertised = self
            .advertised_features
            .ok_or(VhostUserError::InvalidState)?;
        if advertised & VHOST_USER_F_PROTOCOL_FEATURES == 0 {
            return Err(VhostUserError::UnsupportedFeature);
        }
        if self.advertised_protocol_features.is_some()
            || self.negotiated_protocol_features.is_some()
            || self.acked_features.is_some()
        {
            return Err(VhostUserError::InvalidState);
        }
        let features = self.query_u64(Request::GetProtocolFeatures)?;
        self.advertised_protocol_features = Some(features);
        Ok(features)
    }

    /// Selects a subset of advertised reviewed protocol features exactly once.
    pub fn set_protocol_features(&mut self, features: u64) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        let advertised = self
            .advertised_protocol_features
            .ok_or(VhostUserError::InvalidState)?;
        if self.negotiated_protocol_features.is_some() || self.acked_features.is_some() {
            return Err(VhostUserError::InvalidState);
        }
        if features & !SUPPORTED_PROTOCOL_FEATURES != 0 || features & !advertised != 0 {
            return Err(VhostUserError::UnsupportedFeature);
        }
        self.send_without_ack(Request::SetProtocolFeatures, &encode_u64(features), &[])?;
        self.negotiated_protocol_features = Some(features);
        Ok(())
    }

    /// Reads exact configuration bytes after CONFIG negotiation.
    pub fn get_config(
        &mut self,
        offset: u32,
        size: u32,
        flags: VhostUserConfigFlags,
    ) -> Result<VhostUserConfig, VhostUserError> {
        self.ensure_live()?;
        if self.negotiated_protocol_features.unwrap_or(0) & VHOST_USER_PROTOCOL_F_CONFIG == 0 {
            return Err(VhostUserError::UnsupportedFeature);
        }
        let end = offset
            .checked_add(size)
            .ok_or(VhostUserError::InvalidConfiguration)?;
        let body_size = 12_usize
            .checked_add(usize::try_from(size).map_err(|_| VhostUserError::InvalidConfiguration)?)
            .ok_or(VhostUserError::InvalidConfiguration)?;
        if size == 0 || end > MAX_CONFIG_END || body_size > MAX_BODY_BYTES {
            return Err(VhostUserError::InvalidConfiguration);
        }
        let request = encode_config_request(offset, size, flags.bits());
        let result = self
            .transport
            .request_reply(Request::GetConfig, &request, &[], false);
        let reply = self.finish_transport(result)?;
        match decode_config_reply(&reply, offset, size, flags.bits()) {
            Ok(Some(bytes)) => Ok(VhostUserConfig {
                offset,
                flags,
                bytes,
            }),
            Ok(None) => Err(self.poison(VhostUserError::BackendFailure)),
            Err(_) => Err(self.poison(VhostUserError::InvalidMessage)),
        }
    }

    /// Installs guest-acked virtio features exactly once.
    pub fn set_features(&mut self, features: u64) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        let advertised = self
            .advertised_features
            .ok_or(VhostUserError::InvalidState)?;
        if self.acked_features.is_some() {
            return Err(VhostUserError::InvalidState);
        }
        if features & !self.options.allowed_virtio_features != 0 || features & !advertised != 0 {
            return Err(VhostUserError::UnsupportedFeature);
        }
        let protocol_negotiated = self.negotiated_protocol_features.is_some();
        if (features & VHOST_USER_F_PROTOCOL_FEATURES != 0) != protocol_negotiated {
            return Err(VhostUserError::InvalidState);
        }
        self.send_mutating(Request::SetFeatures, &encode_u64(features), &[])?;
        self.acked_features = Some(features);
        Ok(())
    }

    /// Installs one exact nonempty memory table and matching descriptor list.
    pub fn set_memory_table(
        &mut self,
        regions: &[VhostUserMemoryRegion<'_>],
    ) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        if self.acked_features.is_none() || self.memory_set {
            return Err(VhostUserError::InvalidState);
        }
        if regions.is_empty() || regions.len() > usize::from(self.options.max_memory_regions) {
            return Err(VhostUserError::InvalidConfiguration);
        }
        validate_region_overlaps(regions)?;
        let wire: Vec<MemoryRegionWire> = regions
            .iter()
            .copied()
            .map(|region| region.wire())
            .collect();
        let body = encode_memory_table(&wire).map_err(|_| VhostUserError::InvalidConfiguration)?;
        let descriptors: Vec<BorrowedFd<'_>> =
            regions.iter().map(|region| region.backing).collect();
        self.send_mutating(Request::SetMemoryTable, &body, &descriptors)?;
        self.memory_set = true;
        Ok(())
    }

    /// Sets one queue size; all sizes must precede every queue address.
    pub fn set_vring_num(&mut self, queue_index: u16, size: u16) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        let index = self.queue_index(queue_index)?;
        let queue = self.queues.get(index).ok_or(VhostUserError::InvalidState)?;
        if !self.memory_set || queue.stage != QueueStage::Empty {
            return Err(VhostUserError::InvalidState);
        }
        if size == 0 || size > self.options.max_queue_size || !size.is_power_of_two() {
            return Err(VhostUserError::InvalidConfiguration);
        }
        let body = encode_vring_state(u32::from(queue_index), u32::from(size));
        self.send_mutating(Request::SetVringNum, &body, &[])?;
        let queue = self
            .queues
            .get_mut(index)
            .ok_or(VhostUserError::InvalidState)?;
        queue.size = Some(size);
        queue.stage = QueueStage::Size;
        Ok(())
    }

    /// Sets the frontend virtual ring addresses for one sized queue.
    pub fn set_vring_addr(
        &mut self,
        queue_index: u16,
        address: VhostUserVringAddress,
    ) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        if !self.all_queue_sizes_set() {
            return Err(VhostUserError::InvalidState);
        }
        let index = self.queue_index(queue_index)?;
        if self
            .queues
            .get(index)
            .is_none_or(|queue| queue.stage != QueueStage::Size)
        {
            return Err(VhostUserError::InvalidState);
        }
        let body = encode_vring_address(VringAddressWire {
            index: u32::from(queue_index),
            descriptor: address.descriptor,
            used: address.used,
            available: address.available,
        });
        self.send_mutating(Request::SetVringAddr, &body, &[])?;
        self.set_queue_stage(index, QueueStage::Address)
    }

    /// Sets the available-ring base for one addressed queue.
    pub fn set_vring_base(&mut self, queue_index: u16, base: u16) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        let index = self.queue_index(queue_index)?;
        let queue = self.queues.get(index).ok_or(VhostUserError::InvalidState)?;
        if queue.stage != QueueStage::Address || queue.size.is_none_or(|size| base >= size) {
            return Err(VhostUserError::InvalidState);
        }
        let body = encode_vring_state(u32::from(queue_index), u32::from(base));
        self.send_mutating(Request::SetVringBase, &body, &[])?;
        self.set_queue_stage(index, QueueStage::Base)
    }

    /// Transfers the backend-facing call writer for one based queue.
    pub fn set_vring_call(
        &mut self,
        queue_index: u16,
        endpoint: &BackendCallEndpoint,
    ) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        let index = self.require_queue_stage(queue_index, QueueStage::Base)?;
        self.send_mutating(
            Request::SetVringCall,
            &encode_u64(u64::from(queue_index)),
            &[endpoint.as_fd()],
        )?;
        self.set_queue_stage(index, QueueStage::Call)
    }

    /// Transfers the backend-facing kick reader for one called queue.
    pub fn set_vring_kick(
        &mut self,
        queue_index: u16,
        endpoint: &BackendKickEndpoint,
    ) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        let index = self.require_queue_stage(queue_index, QueueStage::Call)?;
        self.send_mutating(
            Request::SetVringKick,
            &encode_u64(u64::from(queue_index)),
            &[endpoint.as_fd()],
        )?;
        self.set_queue_stage(index, QueueStage::Kick)
    }

    /// Enables or disables one fully configured queue.
    pub fn set_vring_enable(
        &mut self,
        queue_index: u16,
        enabled: bool,
    ) -> Result<(), VhostUserError> {
        self.ensure_live()?;
        if self.acked_features.unwrap_or(0) & VHOST_USER_F_PROTOCOL_FEATURES == 0
            || !self.all_queue_sizes_set()
        {
            return Err(VhostUserError::InvalidState);
        }
        let required = if enabled {
            QueueStage::Kick
        } else {
            QueueStage::Enabled
        };
        let index = self.require_queue_stage(queue_index, required)?;
        let body = encode_vring_state(u32::from(queue_index), u32::from(enabled));
        self.send_mutating(Request::SetVringEnable, &body, &[])?;
        self.set_queue_stage(
            index,
            if enabled {
                QueueStage::Enabled
            } else {
                QueueStage::Kick
            },
        )
    }

    /// Returns selected protocol features after successful negotiation.
    pub const fn negotiated_protocol_features(&self) -> Option<u64> {
        self.negotiated_protocol_features
    }

    /// Returns guest-acked virtio features after successful installation.
    pub const fn acked_features(&self) -> Option<u64> {
        self.acked_features
    }

    fn query_u64(&mut self, request: Request) -> Result<u64, VhostUserError> {
        let result = self.transport.request_reply(request, &[], &[], false);
        let reply = self.finish_transport(result)?;
        decode_u64(&reply).map_err(|_| self.poison(VhostUserError::InvalidMessage))
    }

    fn send_without_ack(
        &mut self,
        request: Request,
        body: &[u8],
        descriptors: &[BorrowedFd<'_>],
    ) -> Result<(), VhostUserError> {
        let result = self.transport.send(request, body, descriptors, false);
        self.finish_transport(result)
    }

    fn send_mutating(
        &mut self,
        request: Request,
        body: &[u8],
        descriptors: &[BorrowedFd<'_>],
    ) -> Result<(), VhostUserError> {
        let need_reply =
            self.negotiated_protocol_features.unwrap_or(0) & VHOST_USER_PROTOCOL_F_REPLY_ACK != 0;
        if !need_reply {
            return self.send_without_ack(request, body, descriptors);
        }
        let result = self
            .transport
            .request_reply(request, body, descriptors, true);
        let reply = self.finish_transport(result)?;
        match decode_u64(&reply) {
            Ok(0) => Ok(()),
            Ok(_) => Err(self.poison(VhostUserError::BackendFailure)),
            Err(_) => Err(self.poison(VhostUserError::InvalidMessage)),
        }
    }

    fn finish_transport<T>(
        &mut self,
        result: Result<T, TransportError>,
    ) -> Result<T, VhostUserError> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => Err(self.poison(error.into())),
        }
    }

    fn poison(&mut self, error: VhostUserError) -> VhostUserError {
        self.poisoned = true;
        self.transport.shutdown();
        error
    }

    fn ensure_live(&self) -> Result<(), VhostUserError> {
        if self.poisoned {
            Err(VhostUserError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn queue_index(&self, queue_index: u16) -> Result<usize, VhostUserError> {
        let index = usize::from(queue_index);
        if index >= self.queues.len() {
            Err(VhostUserError::InvalidConfiguration)
        } else {
            Ok(index)
        }
    }

    fn all_queue_sizes_set(&self) -> bool {
        self.queues.iter().all(|queue| queue.size.is_some())
    }

    fn require_queue_stage(
        &self,
        queue_index: u16,
        required: QueueStage,
    ) -> Result<usize, VhostUserError> {
        let index = self.queue_index(queue_index)?;
        if self
            .queues
            .get(index)
            .is_some_and(|queue| queue.stage == required)
        {
            Ok(index)
        } else {
            Err(VhostUserError::InvalidState)
        }
    }

    fn set_queue_stage(&mut self, index: usize, stage: QueueStage) -> Result<(), VhostUserError> {
        let queue = self
            .queues
            .get_mut(index)
            .ok_or(VhostUserError::InvalidState)?;
        queue.stage = stage;
        Ok(())
    }
}

fn validate_region_overlaps(regions: &[VhostUserMemoryRegion<'_>]) -> Result<(), VhostUserError> {
    for (index, region) in regions.iter().enumerate() {
        for other in regions.iter().skip(index.saturating_add(1)) {
            if ranges_overlap(
                region.guest_phys_addr,
                region.memory_size,
                other.guest_phys_addr,
                other.memory_size,
            ) || ranges_overlap(
                region.userspace_addr,
                region.memory_size,
                other.userspace_addr,
                other.memory_size,
            ) {
                return Err(VhostUserError::InvalidConfiguration);
            }
        }
    }
    Ok(())
}

fn ranges_overlap(first_start: u64, first_size: u64, second_start: u64, second_size: u64) -> bool {
    let first_end = first_start.saturating_add(first_size);
    let second_end = second_start.saturating_add(second_size);
    first_start < second_end && second_start < first_end
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::{self, Write};
    use std::os::fd::{AsRawFd, OwnedFd};
    use std::thread;

    use crate::message::{HEADER_BYTES, Header, encode_config_request, reply_frame};
    use crate::notifier::{
        CallDrainOutcome, KickSignalOutcome, create_call_notifier, create_kick_notifier,
    };
    use crate::transport::{recvmsg_once, sendmsg_once};

    use super::*;

    struct PeerRequest {
        header: Header,
        body: Vec<u8>,
        descriptors: Vec<OwnedFd>,
    }

    fn receive_peer_request(stream: &mut UnixStream) -> PeerRequest {
        let mut header_bytes = [0_u8; HEADER_BYTES];
        let mut header_received = 0_usize;
        let mut descriptors = Vec::new();
        while header_received < HEADER_BYTES {
            let attempt = recvmsg_once(stream.as_raw_fd(), &mut header_bytes[header_received..])
                .expect("request header should receive");
            assert_ne!(attempt.bytes, 0, "request header must not reach EOF");
            header_received += attempt.bytes;
            descriptors.extend(attempt.descriptors);
        }
        let header = Header::decode(&header_bytes).expect("request header should decode");
        assert!(!header.is_reply);
        let mut body = vec![0_u8; header.body_size];
        let mut body_received = 0_usize;
        while body_received < body.len() {
            let attempt = recvmsg_once(stream.as_raw_fd(), &mut body[body_received..])
                .expect("request body should receive");
            assert_ne!(attempt.bytes, 0, "request body must not reach EOF");
            body_received += attempt.bytes;
            descriptors.extend(attempt.descriptors);
        }
        PeerRequest {
            header,
            body,
            descriptors,
        }
    }

    fn expect_request(
        stream: &mut UnixStream,
        request: Request,
        need_reply: bool,
        descriptor_count: usize,
    ) -> PeerRequest {
        let received = receive_peer_request(stream);
        assert_eq!(received.header.request, request);
        assert_eq!(received.header.need_reply, need_reply);
        assert_eq!(received.descriptors.len(), descriptor_count);
        received
    }

    fn send_peer_reply(stream: &mut UnixStream, request: Request, body: &[u8]) {
        let encoded = reply_frame(request, body).expect("reply should encode");
        stream.write_all(&encoded).expect("reply should send");
    }

    fn send_peer_reply_with_fd(
        stream: &mut UnixStream,
        request: Request,
        body: &[u8],
        descriptor: BorrowedFd<'_>,
    ) {
        let encoded = reply_frame(request, body).expect("reply should encode");
        let sent = sendmsg_once(stream.as_raw_fd(), &encoded, &[descriptor.as_raw_fd()])
            .expect("descriptor reply should send");
        if sent < encoded.len() {
            stream
                .write_all(&encoded[sent..])
                .expect("reply remainder should send");
        }
    }

    fn send_peer_reply_with_fds(
        stream: &mut UnixStream,
        request: Request,
        body: &[u8],
        descriptors: &[BorrowedFd<'_>],
    ) {
        let encoded = reply_frame(request, body).expect("reply should encode");
        let raw: Vec<_> = descriptors.iter().map(AsRawFd::as_raw_fd).collect();
        let sent =
            sendmsg_once(stream.as_raw_fd(), &encoded, &raw).expect("descriptor reply should send");
        if sent < encoded.len() {
            stream
                .write_all(&encoded[sent..])
                .expect("reply remainder should send");
        }
    }

    fn acknowledge_if_requested(stream: &mut UnixStream, request: &PeerRequest) {
        if request.header.need_reply {
            send_peer_reply(stream, request.header.request, &encode_u64(0));
        }
    }

    fn assert_cloexec(descriptor: &OwnedFd) {
        // SAFETY: F_GETFD reads flags from the live received descriptor.
        let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    fn write_pipe_unit(descriptor: &OwnedFd) {
        let bytes = [0_u8; 8];
        // SAFETY: The received call descriptor is a live pipe writer and the
        // fixed buffer is readable for exactly eight bytes.
        let result =
            unsafe { libc::write(descriptor.as_raw_fd(), bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(result, 8);
    }

    fn read_pipe_unit(descriptor: &OwnedFd) {
        let mut poll_descriptor = libc::pollfd {
            fd: descriptor.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: One initialized poll entry is writable for this bounded wait.
        let ready = unsafe { libc::poll(&raw mut poll_descriptor, 1, 1_000) };
        assert_eq!(ready, 1);
        let mut bytes = [1_u8; 8];
        // SAFETY: The received kick descriptor is a live pipe reader and the
        // fixed buffer is writable for exactly eight bytes.
        let result = unsafe {
            libc::read(
                descriptor.as_raw_fd(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        assert_eq!(result, 8);
        assert_eq!(bytes, [0; 8]);
    }

    fn run_complete_transcript(protocol_features: u64) -> VhostUserFrontend {
        let reply_ack = protocol_features & VHOST_USER_PROTOCOL_F_REPLY_ACK != 0;
        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            let owner = expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            assert!(owner.body.is_empty());

            let get_features = expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            assert!(get_features.body.is_empty());
            send_peer_reply(
                &mut peer_stream,
                Request::GetFeatures,
                &encode_u64(SUPPORTED_VIRTIO_FEATURES),
            );

            let get_protocol =
                expect_request(&mut peer_stream, Request::GetProtocolFeatures, false, 0);
            assert!(get_protocol.body.is_empty());
            send_peer_reply(
                &mut peer_stream,
                Request::GetProtocolFeatures,
                &encode_u64(protocol_features),
            );

            let set_protocol =
                expect_request(&mut peer_stream, Request::SetProtocolFeatures, false, 0);
            assert_eq!(decode_u64(&set_protocol.body), Ok(protocol_features));

            let get_config = expect_request(&mut peer_stream, Request::GetConfig, false, 0);
            assert_eq!(get_config.body.len(), 72);
            assert_eq!(
                &get_config.body[..12],
                &encode_config_request(0, 60, 1)[..12]
            );
            assert!(get_config.body[12..].iter().all(|byte| *byte == 0));
            let mut config_reply = encode_config_request(0, 60, 1);
            config_reply[12..].fill(0x5a);
            send_peer_reply(&mut peer_stream, Request::GetConfig, &config_reply);

            let set_features = expect_request(&mut peer_stream, Request::SetFeatures, reply_ack, 0);
            assert_eq!(
                decode_u64(&set_features.body),
                Ok(SUPPORTED_VIRTIO_FEATURES)
            );
            acknowledge_if_requested(&mut peer_stream, &set_features);

            let memory = expect_request(&mut peer_stream, Request::SetMemoryTable, reply_ack, 1);
            assert_eq!(memory.body.len(), 40);
            assert_eq!(
                u32::from_ne_bytes(memory.body[..4].try_into().expect("count should decode")),
                1
            );
            assert_eq!(&memory.body[4..8], &[0; 4]);
            assert_cloexec(&memory.descriptors[0]);
            acknowledge_if_requested(&mut peer_stream, &memory);

            let number = expect_request(&mut peer_stream, Request::SetVringNum, reply_ack, 0);
            assert_eq!(number.body, encode_vring_state(0, 256));
            acknowledge_if_requested(&mut peer_stream, &number);

            let address = expect_request(&mut peer_stream, Request::SetVringAddr, reply_ack, 0);
            assert_eq!(address.body.len(), 40);
            assert_eq!(&address.body[..8], &[0; 8]);
            assert_eq!(&address.body[32..40], &[0; 8]);
            acknowledge_if_requested(&mut peer_stream, &address);

            let base = expect_request(&mut peer_stream, Request::SetVringBase, reply_ack, 0);
            assert_eq!(base.body, encode_vring_state(0, 0));
            acknowledge_if_requested(&mut peer_stream, &base);

            let call = expect_request(&mut peer_stream, Request::SetVringCall, reply_ack, 1);
            assert_eq!(decode_u64(&call.body), Ok(0));
            assert_cloexec(&call.descriptors[0]);
            write_pipe_unit(&call.descriptors[0]);
            acknowledge_if_requested(&mut peer_stream, &call);

            let kick = expect_request(&mut peer_stream, Request::SetVringKick, reply_ack, 1);
            assert_eq!(decode_u64(&kick.body), Ok(0));
            assert_cloexec(&kick.descriptors[0]);
            acknowledge_if_requested(&mut peer_stream, &kick);

            let enable = expect_request(&mut peer_stream, Request::SetVringEnable, reply_ack, 0);
            assert_eq!(enable.body, encode_vring_state(0, 1));
            acknowledge_if_requested(&mut peer_stream, &enable);
            read_pipe_unit(&kick.descriptors[0]);
        });

        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_secs(2))
            .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should initialize");
        frontend.set_owner().expect("owner should set");
        assert_eq!(
            frontend.get_features().expect("features should read"),
            SUPPORTED_VIRTIO_FEATURES
        );
        assert_eq!(
            frontend
                .get_protocol_features()
                .expect("protocol features should read"),
            protocol_features
        );
        frontend
            .set_protocol_features(protocol_features)
            .expect("protocol features should set");
        let config = frontend
            .get_config(0, 60, VhostUserConfigFlags::Writable)
            .expect("config should read");
        assert_eq!(config.as_bytes(), &[0x5a; 60]);
        frontend
            .set_features(SUPPORTED_VIRTIO_FEATURES)
            .expect("features should set");

        let backing = File::open("/dev/zero").expect("memory fixture should open");
        let region = VhostUserMemoryRegion::new(0, 0x2000, 0x1000_0000, 0, backing.as_fd())
            .expect("memory region should validate");
        frontend
            .set_memory_table(&[region])
            .expect("memory table should set");
        frontend
            .set_vring_num(0, 256)
            .expect("queue size should set");
        frontend
            .set_vring_addr(
                0,
                VhostUserVringAddress::new(0x1000_0000, 0x1000_4000, 0x1000_3000)
                    .expect("addresses should validate"),
            )
            .expect("queue address should set");
        frontend
            .set_vring_base(0, 0)
            .expect("queue base should set");
        let (call, backend_call) = create_call_notifier().expect("call pipe should open");
        let (kick, backend_kick) = create_kick_notifier().expect("kick pipe should open");
        frontend
            .set_vring_call(0, &backend_call)
            .expect("call endpoint should set");
        frontend
            .set_vring_kick(0, &backend_kick)
            .expect("kick endpoint should set");
        drop(backend_call);
        drop(backend_kick);
        frontend
            .set_vring_enable(0, true)
            .expect("queue should enable");
        assert_eq!(kick.signal(), Ok(KickSignalOutcome::Signaled));
        peer.join().expect("control peer should complete");
        assert_eq!(call.drain(), Ok(CallDrainOutcome::Closed(1)));
        assert_eq!(frontend.state(), VhostUserFrontendState::Ready);
        frontend
    }

    #[test]
    fn completes_firecracker_config_only_transcript() {
        let mut frontend = run_complete_transcript(VHOST_USER_PROTOCOL_F_CONFIG);
        assert_eq!(
            frontend.negotiated_protocol_features(),
            Some(VHOST_USER_PROTOCOL_F_CONFIG)
        );
        assert_eq!(
            frontend.set_vring_num(0, 256),
            Err(VhostUserError::InvalidState)
        );
        assert_eq!(frontend.state(), VhostUserFrontendState::Ready);
    }

    #[test]
    fn completes_firecracker_reply_ack_transcript() {
        let frontend =
            run_complete_transcript(VHOST_USER_PROTOCOL_F_CONFIG | VHOST_USER_PROTOCOL_F_REPLY_ACK);
        assert_eq!(frontend.state(), VhostUserFrontendState::Ready);
        assert_eq!(frontend.acked_features(), Some(SUPPORTED_VIRTIO_FEATURES));
    }

    #[test]
    fn constructors_reject_bounds_overflow_overlap_and_alignment() {
        for options in [
            VhostUserFrontendOptions::new(
                0,
                256,
                1,
                SUPPORTED_VIRTIO_FEATURES,
                Duration::from_secs(1),
            ),
            VhostUserFrontendOptions::new(
                1,
                255,
                1,
                SUPPORTED_VIRTIO_FEATURES,
                Duration::from_secs(1),
            ),
            VhostUserFrontendOptions::new(
                1,
                256,
                33,
                SUPPORTED_VIRTIO_FEATURES,
                Duration::from_secs(1),
            ),
            VhostUserFrontendOptions::new(1, 256, 1, u64::MAX, Duration::from_secs(1)),
            VhostUserFrontendOptions::new(1, 256, 1, SUPPORTED_VIRTIO_FEATURES, Duration::ZERO),
        ] {
            assert_eq!(options, Err(VhostUserError::InvalidConfiguration));
        }

        let backing = File::open("/dev/zero").expect("fixture should open");
        for region in [
            VhostUserMemoryRegion::new(0, 0, 0x1000, 0, backing.as_fd()),
            VhostUserMemoryRegion::new(1, 0x1000, 0x1000, 0, backing.as_fd()),
            VhostUserMemoryRegion::new(0, 0x1001, 0x1000, 0, backing.as_fd()),
            VhostUserMemoryRegion::new(0, 0x1000, u64::MAX - 0xfff, 0, backing.as_fd()),
        ] {
            assert!(matches!(region, Err(VhostUserError::InvalidConfiguration)));
        }
        let first = VhostUserMemoryRegion::new(0, 0x2000, 0x10_0000, 0, backing.as_fd())
            .expect("first region should validate");
        let guest_overlap =
            VhostUserMemoryRegion::new(0x1000, 0x1000, 0x20_0000, 0, backing.as_fd())
                .expect("overlap candidate should construct");
        assert_eq!(
            validate_region_overlaps(&[first, guest_overlap]),
            Err(VhostUserError::InvalidConfiguration)
        );
        let host_overlap =
            VhostUserMemoryRegion::new(0x4000, 0x1000, 0x10_1000, 0, backing.as_fd())
                .expect("overlap candidate should construct");
        assert_eq!(
            validate_region_overlaps(&[first, host_overlap]),
            Err(VhostUserError::InvalidConfiguration)
        );

        for address in [
            VhostUserVringAddress::new(0, 0x2000, 0x3000),
            VhostUserVringAddress::new(0x1001, 0x2000, 0x3000),
            VhostUserVringAddress::new(0x1000, 0x2002, 0x3000),
            VhostUserVringAddress::new(0x1000, 0x2000, 0x3001),
        ] {
            assert_eq!(address, Err(VhostUserError::InvalidConfiguration));
        }
    }

    #[test]
    fn local_state_rejections_do_not_poison_or_write() {
        let (stream, _peer) = UnixStream::pair().expect("stream pair should open");
        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_secs(1))
            .expect("options should validate");
        let mut frontend = VhostUserFrontend::new(stream, options).expect("frontend should open");
        assert_eq!(frontend.set_features(0), Err(VhostUserError::InvalidState));
        assert_eq!(
            frontend.get_protocol_features(),
            Err(VhostUserError::InvalidState)
        );
        assert_eq!(
            frontend.set_vring_num(0, 256),
            Err(VhostUserError::InvalidState)
        );
        assert_eq!(frontend.state(), VhostUserFrontendState::New);
    }

    #[test]
    fn mismatched_reply_and_timeout_poison_the_stream() {
        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            send_peer_reply(
                &mut peer_stream,
                Request::GetProtocolFeatures,
                &encode_u64(0),
            );
        });
        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_secs(1))
            .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should open");
        frontend.set_owner().expect("owner should set");
        assert_eq!(frontend.get_features(), Err(VhostUserError::InvalidMessage));
        assert_eq!(frontend.state(), VhostUserFrontendState::Poisoned);
        assert_eq!(frontend.get_features(), Err(VhostUserError::Poisoned));
        peer.join().expect("peer should complete");

        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            thread::sleep(Duration::from_millis(100));
        });
        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_millis(20))
            .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should open");
        frontend.set_owner().expect("owner should set");
        assert_eq!(frontend.get_features(), Err(VhostUserError::Timeout));
        assert_eq!(frontend.state(), VhostUserFrontendState::Poisoned);
        peer.join().expect("peer should complete");
    }

    #[test]
    fn unexpected_reply_descriptor_is_closed_and_poisoned() {
        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let (kick, backend_reader) = create_kick_notifier().expect("pipe should open");
        let peer = thread::spawn(move || {
            expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            send_peer_reply_with_fd(
                &mut peer_stream,
                Request::GetFeatures,
                &encode_u64(SUPPORTED_VIRTIO_FEATURES),
                backend_reader.as_fd(),
            );
        });
        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_secs(1))
            .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should open");
        frontend.set_owner().expect("owner should set");
        assert_eq!(frontend.get_features(), Err(VhostUserError::InvalidMessage));
        peer.join().expect("peer should complete");
        assert!(matches!(
            kick.signal(),
            Err(crate::VhostUserNotifierError::Io(io::ErrorKind::BrokenPipe))
        ));
    }

    #[test]
    fn excessive_reply_descriptors_are_all_closed() {
        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let mut writers = Vec::new();
        let mut readers = Vec::new();
        for _ in 0..(MAX_ATTACHED_FDS + 1) {
            let (writer, reader) = create_kick_notifier().expect("pipe should open");
            writers.push(writer);
            readers.push(reader);
        }
        let peer = thread::spawn(move || {
            expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            let borrowed: Vec<_> = readers.iter().map(AsFd::as_fd).collect();
            send_peer_reply_with_fds(
                &mut peer_stream,
                Request::GetFeatures,
                &encode_u64(SUPPORTED_VIRTIO_FEATURES),
                &borrowed,
            );
        });
        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_secs(1))
            .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should open");
        frontend.set_owner().expect("owner should set");
        assert_eq!(frontend.get_features(), Err(VhostUserError::InvalidMessage));
        peer.join().expect("peer should complete");
        for writer in writers {
            assert!(matches!(
                writer.signal(),
                Err(crate::VhostUserNotifierError::Io(io::ErrorKind::BrokenPipe))
            ));
        }
    }

    #[test]
    fn fragmented_reply_succeeds_and_short_eof_poisoned() {
        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            let encoded = reply_frame(Request::GetFeatures, &encode_u64(SUPPORTED_VIRTIO_FEATURES))
                .expect("reply should encode");
            for byte in encoded {
                peer_stream
                    .write_all(&[byte])
                    .expect("fragment should send");
            }
        });
        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_secs(1))
            .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should open");
        frontend.set_owner().expect("owner should set");
        assert_eq!(frontend.get_features(), Ok(SUPPORTED_VIRTIO_FEATURES));
        peer.join().expect("peer should complete");

        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            let encoded =
                reply_frame(Request::GetFeatures, &encode_u64(1)).expect("reply should encode");
            peer_stream
                .write_all(&encoded[..HEADER_BYTES + 4])
                .expect("short reply should send");
        });
        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_secs(1))
            .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should open");
        frontend.set_owner().expect("owner should set");
        assert_eq!(frontend.get_features(), Err(VhostUserError::Disconnected));
        assert_eq!(frontend.state(), VhostUserFrontendState::Poisoned);
        peer.join().expect("peer should complete");
    }

    #[test]
    fn backend_ack_and_config_failures_are_terminal() {
        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            send_peer_reply(
                &mut peer_stream,
                Request::GetFeatures,
                &encode_u64(SUPPORTED_VIRTIO_FEATURES),
            );
            expect_request(&mut peer_stream, Request::GetProtocolFeatures, false, 0);
            send_peer_reply(
                &mut peer_stream,
                Request::GetProtocolFeatures,
                &encode_u64(SUPPORTED_PROTOCOL_FEATURES),
            );
            expect_request(&mut peer_stream, Request::SetProtocolFeatures, false, 0);
            let features = expect_request(&mut peer_stream, Request::SetFeatures, true, 0);
            send_peer_reply(&mut peer_stream, features.header.request, &encode_u64(1));
        });
        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_secs(1))
            .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should open");
        frontend.set_owner().expect("owner should set");
        frontend.get_features().expect("features should read");
        frontend
            .get_protocol_features()
            .expect("protocol features should read");
        frontend
            .set_protocol_features(SUPPORTED_PROTOCOL_FEATURES)
            .expect("protocol features should set");
        assert_eq!(
            frontend.set_features(SUPPORTED_VIRTIO_FEATURES),
            Err(VhostUserError::BackendFailure)
        );
        assert_eq!(frontend.state(), VhostUserFrontendState::Poisoned);
        peer.join().expect("peer should complete");

        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            send_peer_reply(
                &mut peer_stream,
                Request::GetFeatures,
                &encode_u64(SUPPORTED_VIRTIO_FEATURES),
            );
            expect_request(&mut peer_stream, Request::GetProtocolFeatures, false, 0);
            send_peer_reply(
                &mut peer_stream,
                Request::GetProtocolFeatures,
                &encode_u64(VHOST_USER_PROTOCOL_F_CONFIG),
            );
            expect_request(&mut peer_stream, Request::SetProtocolFeatures, false, 0);
            expect_request(&mut peer_stream, Request::GetConfig, false, 0);
            send_peer_reply(
                &mut peer_stream,
                Request::GetConfig,
                &encode_config_request(0, 0, 1),
            );
        });
        let options = VhostUserFrontendOptions::firecracker_block(Duration::from_secs(1))
            .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should open");
        frontend.set_owner().expect("owner should set");
        frontend.get_features().expect("features should read");
        frontend
            .get_protocol_features()
            .expect("protocol features should read");
        frontend
            .set_protocol_features(VHOST_USER_PROTOCOL_F_CONFIG)
            .expect("protocol features should set");
        assert_eq!(
            frontend.get_config(0, 60, VhostUserConfigFlags::Writable),
            Err(VhostUserError::BackendFailure)
        );
        assert_eq!(frontend.state(), VhostUserFrontendState::Poisoned);
        peer.join().expect("peer should complete");
    }

    #[test]
    fn every_queue_size_precedes_the_first_queue_address() {
        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("stream pair should open");
        let peer = thread::spawn(move || {
            expect_request(&mut peer_stream, Request::SetOwner, false, 0);
            expect_request(&mut peer_stream, Request::GetFeatures, false, 0);
            send_peer_reply(
                &mut peer_stream,
                Request::GetFeatures,
                &encode_u64(SUPPORTED_VIRTIO_FEATURES),
            );
            expect_request(&mut peer_stream, Request::GetProtocolFeatures, false, 0);
            send_peer_reply(
                &mut peer_stream,
                Request::GetProtocolFeatures,
                &encode_u64(VHOST_USER_PROTOCOL_F_CONFIG),
            );
            expect_request(&mut peer_stream, Request::SetProtocolFeatures, false, 0);
            expect_request(&mut peer_stream, Request::SetFeatures, false, 0);
            expect_request(&mut peer_stream, Request::SetMemoryTable, false, 1);
            let first = expect_request(&mut peer_stream, Request::SetVringNum, false, 0);
            assert_eq!(first.body, encode_vring_state(0, 256));
            let second = expect_request(&mut peer_stream, Request::SetVringNum, false, 0);
            assert_eq!(second.body, encode_vring_state(1, 256));
            let address = expect_request(&mut peer_stream, Request::SetVringAddr, false, 0);
            assert_eq!(
                u32::from_ne_bytes(address.body[..4].try_into().expect("index should decode")),
                0
            );
        });
        let options = VhostUserFrontendOptions::new(
            2,
            256,
            1,
            SUPPORTED_VIRTIO_FEATURES,
            Duration::from_secs(1),
        )
        .expect("options should validate");
        let mut frontend =
            VhostUserFrontend::new(frontend_stream, options).expect("frontend should open");
        frontend.set_owner().expect("owner should set");
        frontend.get_features().expect("features should read");
        frontend
            .get_protocol_features()
            .expect("protocol features should read");
        frontend
            .set_protocol_features(VHOST_USER_PROTOCOL_F_CONFIG)
            .expect("protocol features should set");
        frontend
            .set_features(SUPPORTED_VIRTIO_FEATURES)
            .expect("features should set");
        let backing = File::open("/dev/zero").expect("fixture should open");
        let region = VhostUserMemoryRegion::new(0, 0x1000, 0x1000_0000, 0, backing.as_fd())
            .expect("region should validate");
        frontend
            .set_memory_table(&[region])
            .expect("memory should set");
        frontend
            .set_vring_num(0, 256)
            .expect("first size should set");
        let address =
            VhostUserVringAddress::new(0x1000, 0x2000, 0x3000).expect("address should validate");
        assert_eq!(
            frontend.set_vring_addr(0, address),
            Err(VhostUserError::InvalidState)
        );
        frontend
            .set_vring_num(1, 256)
            .expect("second size should set");
        frontend
            .set_vring_addr(0, address)
            .expect("address should set after every size");
        peer.join().expect("peer should complete");
        assert_ne!(frontend.state(), VhostUserFrontendState::Poisoned);
    }

    #[test]
    fn public_debug_surfaces_redact_addresses_fds_payloads_and_features() {
        let backing = File::open("/dev/zero").expect("fixture should open");
        let region =
            VhostUserMemoryRegion::new(0x1111_0000, 0x1000, 0x2222_0000, 0, backing.as_fd())
                .expect("region should validate");
        let address = VhostUserVringAddress::new(0x3333_0000, 0x4444_0000, 0x5555_0000)
            .expect("address should validate");
        assert_eq!(format!("{region:?}"), "VhostUserMemoryRegion(\"redacted\")");
        assert_eq!(
            format!("{address:?}"),
            "VhostUserVringAddress(\"redacted\")"
        );
        let config = VhostUserConfig {
            offset: 0,
            flags: VhostUserConfigFlags::Writable,
            bytes: b"private-backend-payload".to_vec(),
        };
        let debug = format!("{config:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains("private-backend-payload"));
    }
}
