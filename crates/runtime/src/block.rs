//! Backend-neutral block-device configuration model.

pub mod async_executor;

use std::collections::{BTreeMap, TryReserveError};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd};
#[cfg(unix)]
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bangbang_vhost_user::{
    BackendCallEndpoint, BackendKickEndpoint, CallDrainOutcome, CallNotifier, KickNotifier,
    VHOST_USER_F_PROTOCOL_FEATURES, VHOST_USER_PROTOCOL_F_CONFIG, VHOST_USER_PROTOCOL_F_REPLY_ACK,
    VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_RO, VIRTIO_F_EVENT_IDX, VIRTIO_F_VERSION_1,
    VIRTIO_RING_F_INDIRECT_DESC, VhostUserConfigFlags, VhostUserError, VhostUserFrontend,
    VhostUserFrontendOptions, VhostUserMemoryRegion, VhostUserNotifierError, VhostUserVringAddress,
    create_call_notifier, create_kick_notifier,
};

use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryRange,
    GuestMemorySharedBacking, GuestMemorySharedBackingError,
};
use crate::mmio::{
    MmioAccessBytes, MmioAccessBytesError, MmioBusError, MmioDispatchError, MmioDispatcher,
    MmioHandlerError, MmioRegion, MmioRegionId,
};
use crate::token_bucket::{
    PersistedTokenBucketState, PersistedTokenBucketStateError, TokenBucket, TokenBucketConfig,
};
use crate::virtio::VirtioInterruptIntent;
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError,
    VirtioMmioDeviceActivationHandler, VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError,
    VirtioMmioDeviceConfigHandler, VirtioMmioQueueRegisterError, VirtioMmioQueueState,
    VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError, VirtioMmioTransportState,
    VirtioMmioTransportStateError,
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

use self::async_executor::{
    BlockAsyncAdmissionError, BlockAsyncDriveGeneration, BlockAsyncGenerationCaptureState,
    BlockAsyncOperation, BlockAsyncOperationBuildError, BlockAsyncOperationKind,
    BlockAsyncOperationStatus, BlockAsyncRequestIdentity, BlockAsyncRuntimeError,
    SharedBlockAsyncRuntime,
};

pub const VIRTIO_BLOCK_DEVICE_ID: u32 = 2;
pub const VIRTIO_BLOCK_QUEUE_COUNT: usize = 1;
pub const VIRTIO_BLOCK_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_BLOCK_QUEUE_SIZES: [u16; VIRTIO_BLOCK_QUEUE_COUNT] = [VIRTIO_BLOCK_QUEUE_SIZE];
pub const VIRTIO_BLOCK_SECTOR_SHIFT: u32 = 9;
pub const VIRTIO_BLOCK_SECTOR_SIZE: u64 = 1 << VIRTIO_BLOCK_SECTOR_SHIFT;
pub const VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE: usize = 8;
pub const VIRTIO_BLOCK_CONFIG_SIZE: usize = 60;
pub const VIRTIO_BLOCK_FEATURE_READ_ONLY: u32 = 5;
pub const VIRTIO_BLOCK_FEATURE_FLUSH: u32 = 9;
pub const VIRTIO_RING_FEATURE_INDIRECT_DESC: u32 = 28;
pub const VIRTIO_RING_FEATURE_EVENT_IDX: u32 = 29;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;
pub const VIRTIO_BLOCK_REQUEST_HEADER_SIZE: u32 = 16;
pub const VIRTIO_BLOCK_STATUS_SIZE: u32 = 1;
pub const VIRTIO_BLOCK_ID_BYTES: u32 = 20;
pub const VIRTIO_BLOCK_REQUEST_TYPE_IN: u32 = 0;
pub const VIRTIO_BLOCK_REQUEST_TYPE_OUT: u32 = 1;
pub const VIRTIO_BLOCK_REQUEST_TYPE_FLUSH: u32 = 4;
pub const VIRTIO_BLOCK_REQUEST_TYPE_GET_ID: u32 = 8;
pub const VIRTIO_BLOCK_STATUS_OK: u8 = 0;
pub const VIRTIO_BLOCK_STATUS_IOERR: u8 = 1;
pub const VIRTIO_BLOCK_STATUS_UNSUPPORTED: u8 = 2;

pub type VirtioBlockMmioHandler =
    VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice>;
pub type VirtioBlockPciEndpoint = VirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice>;

#[derive(Clone, PartialEq, Eq)]
pub struct DriveConfigInput {
    path_drive_id: String,
    body_drive_id: String,
    path_on_host: Option<PathBuf>,
    is_root_device: bool,
    is_read_only: Option<bool>,
    partuuid: Option<String>,
    cache_type: Option<DriveCacheType>,
    io_engine: Option<DriveIoEngine>,
    rate_limiter: Option<DriveRateLimiterConfig>,
    socket: Option<PathBuf>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DriveUpdateInput {
    path_drive_id: String,
    body_drive_id: String,
    path_on_host: Option<PathBuf>,
    rate_limiter: Option<DriveRateLimiterConfig>,
}

impl fmt::Debug for DriveConfigInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DriveConfigInput")
            .field("path_drive_id", &self.path_drive_id)
            .field("body_drive_id", &self.body_drive_id)
            .field(
                "path_on_host",
                &self.path_on_host.as_ref().map(|_| "<redacted>"),
            )
            .field("is_root_device", &self.is_root_device)
            .field("is_read_only", &self.is_read_only)
            .field("partuuid", &self.partuuid)
            .field("cache_type", &self.cache_type)
            .field("io_engine", &self.io_engine)
            .field("rate_limiter", &self.rate_limiter)
            .field("socket", &self.socket.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl fmt::Debug for DriveUpdateInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DriveUpdateInput")
            .field("path_drive_id", &self.path_drive_id)
            .field("body_drive_id", &self.body_drive_id)
            .field(
                "path_on_host",
                &self.path_on_host.as_ref().map(|_| "<redacted>"),
            )
            .field("rate_limiter", &self.rate_limiter)
            .finish()
    }
}

impl DriveUpdateInput {
    pub fn new(
        path_drive_id: impl Into<String>,
        body_drive_id: impl Into<String>,
        path_on_host: Option<PathBuf>,
    ) -> Self {
        Self {
            path_drive_id: path_drive_id.into(),
            body_drive_id: body_drive_id.into(),
            path_on_host,
            rate_limiter: None,
        }
    }

    pub fn path_drive_id(&self) -> &str {
        &self.path_drive_id
    }

    pub fn body_drive_id(&self) -> &str {
        &self.body_drive_id
    }

    pub fn path_on_host(&self) -> Option<&Path> {
        self.path_on_host.as_deref()
    }

    pub const fn rate_limiter(&self) -> Option<DriveRateLimiterConfig> {
        self.rate_limiter
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        matches!(self.rate_limiter, Some(rate_limiter) if rate_limiter.is_configured())
    }

    pub const fn with_rate_limiter(mut self, rate_limiter: DriveRateLimiterConfig) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    pub const fn with_rate_limiter_configured(self) -> Self {
        self.with_rate_limiter(DriveRateLimiterConfig::new(
            None,
            Some(DriveTokenBucketConfig::new(1, None, 1)),
        ))
    }

    pub fn validate(self) -> Result<DriveUpdate, DriveUpdateError> {
        DriveUpdate::try_from(self)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DriveUpdate {
    drive_id: String,
    path_on_host: Option<PathBuf>,
    rate_limiter: Option<DriveRateLimiterConfig>,
}

impl fmt::Debug for DriveUpdate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DriveUpdate")
            .field("drive_id", &self.drive_id)
            .field(
                "path_on_host",
                &self.path_on_host.as_ref().map(|_| "<redacted>"),
            )
            .field("rate_limiter", &self.rate_limiter)
            .finish()
    }
}

impl DriveUpdate {
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    pub fn path_on_host(&self) -> Option<&Path> {
        self.path_on_host.as_deref()
    }

    pub const fn rate_limiter(&self) -> Option<DriveRateLimiterConfig> {
        self.rate_limiter
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        matches!(self.rate_limiter, Some(rate_limiter) if rate_limiter.is_configured())
    }
}

impl DriveConfigInput {
    pub fn new(
        path_drive_id: impl Into<String>,
        body_drive_id: impl Into<String>,
        path_on_host: impl Into<PathBuf>,
        is_root_device: bool,
    ) -> Self {
        Self {
            path_drive_id: path_drive_id.into(),
            body_drive_id: body_drive_id.into(),
            path_on_host: Some(path_on_host.into()),
            is_root_device,
            is_read_only: None,
            partuuid: None,
            cache_type: None,
            io_engine: None,
            rate_limiter: None,
            socket: None,
        }
    }

    pub fn new_without_path_on_host(
        path_drive_id: impl Into<String>,
        body_drive_id: impl Into<String>,
        is_root_device: bool,
    ) -> Self {
        Self {
            path_drive_id: path_drive_id.into(),
            body_drive_id: body_drive_id.into(),
            path_on_host: None,
            is_root_device,
            is_read_only: None,
            partuuid: None,
            cache_type: None,
            io_engine: None,
            rate_limiter: None,
            socket: None,
        }
    }

    pub fn path_drive_id(&self) -> &str {
        &self.path_drive_id
    }

    pub fn body_drive_id(&self) -> &str {
        &self.body_drive_id
    }

    pub fn path_on_host(&self) -> Option<&Path> {
        self.path_on_host.as_deref()
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root_device
    }

    pub const fn is_read_only(&self) -> Option<bool> {
        self.is_read_only
    }

    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    pub const fn cache_type(&self) -> Option<DriveCacheType> {
        self.cache_type
    }

    pub const fn io_engine(&self) -> Option<DriveIoEngine> {
        self.io_engine
    }

    pub const fn rate_limiter(&self) -> Option<DriveRateLimiterConfig> {
        self.rate_limiter
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        matches!(self.rate_limiter, Some(rate_limiter) if rate_limiter.is_configured())
    }

    pub fn socket(&self) -> Option<&Path> {
        self.socket.as_deref()
    }

    pub fn with_is_read_only(mut self, is_read_only: bool) -> Self {
        self.is_read_only = Some(is_read_only);
        self
    }

    pub fn with_partuuid(mut self, partuuid: impl Into<String>) -> Self {
        self.partuuid = Some(partuuid.into());
        self
    }

    pub const fn with_cache_type(mut self, cache_type: DriveCacheType) -> Self {
        self.cache_type = Some(cache_type);
        self
    }

    pub const fn with_io_engine(mut self, io_engine: DriveIoEngine) -> Self {
        self.io_engine = Some(io_engine);
        self
    }

    pub const fn with_rate_limiter(mut self, rate_limiter: DriveRateLimiterConfig) -> Self {
        self.rate_limiter = Some(rate_limiter);
        self
    }

    pub const fn with_rate_limiter_configured(self) -> Self {
        self.with_rate_limiter(DriveRateLimiterConfig::new(
            None,
            Some(DriveTokenBucketConfig::new(1, None, 1)),
        ))
    }

    pub fn with_socket(mut self, socket: impl Into<PathBuf>) -> Self {
        self.socket = Some(socket.into());
        self
    }

    pub fn validate(self) -> Result<DriveConfig, DriveConfigError> {
        DriveConfig::try_from(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveRateLimiterConfig {
    bandwidth: Option<DriveTokenBucketConfig>,
    ops: Option<DriveTokenBucketConfig>,
}

impl DriveRateLimiterConfig {
    pub const fn new(
        bandwidth: Option<DriveTokenBucketConfig>,
        ops: Option<DriveTokenBucketConfig>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    pub const fn bandwidth(self) -> Option<DriveTokenBucketConfig> {
        self.bandwidth
    }

    pub const fn ops(self) -> Option<DriveTokenBucketConfig> {
        self.ops
    }

    pub const fn is_configured(self) -> bool {
        self.bandwidth.is_some() || self.ops.is_some()
    }

    fn applied_to(self, existing: Option<Self>) -> Option<Self> {
        let updated = Self {
            bandwidth: updated_token_bucket(existing.and_then(Self::bandwidth), self.bandwidth),
            ops: updated_token_bucket(existing.and_then(Self::ops), self.ops),
        };
        updated.is_configured().then_some(updated)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DriveTokenBucketConfig {
    size: u64,
    one_time_burst: Option<u64>,
    refill_time: u64,
}

impl DriveTokenBucketConfig {
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

const fn updated_token_bucket(
    existing: Option<DriveTokenBucketConfig>,
    update: Option<DriveTokenBucketConfig>,
) -> Option<DriveTokenBucketConfig> {
    match update {
        Some(bucket) if bucket.is_enabled() => Some(bucket),
        Some(_) => None,
        None => existing,
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct DriveConfig {
    drive_id: String,
    is_root_device: bool,
    partuuid: Option<String>,
    cache_type: DriveCacheType,
    backend: DriveBackendConfig,
}

#[derive(Clone, PartialEq, Eq)]
pub enum DriveBackendConfig {
    File {
        path_on_host: PathBuf,
        is_read_only: bool,
        io_engine: DriveIoEngine,
        rate_limiter: Option<DriveRateLimiterConfig>,
    },
    VhostUser {
        socket: PathBuf,
    },
}

/// A connected vhost-user block frontend after bounded, pre-VM discovery.
pub struct PreparedVhostUserBlockFrontend {
    frontend: VhostUserFrontend,
    available_features: u64,
    config_bytes: [u8; VIRTIO_BLOCK_CONFIG_SIZE],
}

impl fmt::Debug for PreparedVhostUserBlockFrontend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedVhostUserBlockFrontend")
            .field("frontend", &self.frontend)
            .field("available_features", &"<redacted>")
            .field("config_bytes", &"<redacted>")
            .finish()
    }
}

impl PreparedVhostUserBlockFrontend {
    /// Performs the Firecracker-shaped discovery sequence without committing
    /// guest-accepted virtio features or transferring guest-memory descriptors.
    pub fn discover(
        stream: UnixStream,
        cache_type: DriveCacheType,
        operation_timeout: Duration,
    ) -> Result<Self, PreparedVhostUserBlockFrontendError> {
        let options = VhostUserFrontendOptions::firecracker_block(operation_timeout)
            .map_err(PreparedVhostUserBlockFrontendError::Frontend)?;
        let mut frontend = VhostUserFrontend::new(stream, options)
            .map_err(PreparedVhostUserBlockFrontendError::Frontend)?;
        frontend
            .set_owner()
            .map_err(PreparedVhostUserBlockFrontendError::Frontend)?;
        let backend_features = frontend
            .get_features()
            .map_err(PreparedVhostUserBlockFrontendError::Frontend)?;
        let required_features = VIRTIO_F_VERSION_1 | VHOST_USER_F_PROTOCOL_FEATURES;
        if backend_features & required_features != required_features {
            return Err(PreparedVhostUserBlockFrontendError::MissingRequiredVirtioFeatures);
        }
        let mut requested_features =
            required_features | VIRTIO_RING_F_INDIRECT_DESC | VIRTIO_F_EVENT_IDX | VIRTIO_BLK_F_RO;
        if matches!(cache_type, DriveCacheType::Writeback) {
            requested_features |= VIRTIO_BLK_F_FLUSH;
        }
        let available_features = backend_features & requested_features;

        let backend_protocol_features = frontend
            .get_protocol_features()
            .map_err(PreparedVhostUserBlockFrontendError::Frontend)?;
        if backend_protocol_features & VHOST_USER_PROTOCOL_F_CONFIG == 0 {
            return Err(PreparedVhostUserBlockFrontendError::MissingConfigProtocolFeature);
        }
        let negotiated_protocol_features = VHOST_USER_PROTOCOL_F_CONFIG
            | (backend_protocol_features & VHOST_USER_PROTOCOL_F_REPLY_ACK);
        frontend
            .set_protocol_features(negotiated_protocol_features)
            .map_err(PreparedVhostUserBlockFrontendError::Frontend)?;
        let config = frontend
            .get_config(
                0,
                u32::try_from(VIRTIO_BLOCK_CONFIG_SIZE)
                    .map_err(|_| PreparedVhostUserBlockFrontendError::InvalidConfig)?,
                VhostUserConfigFlags::Writable,
            )
            .map_err(PreparedVhostUserBlockFrontendError::Frontend)?;
        let config_bytes = config
            .as_bytes()
            .try_into()
            .map_err(|_| PreparedVhostUserBlockFrontendError::InvalidConfig)?;

        Ok(Self {
            frontend,
            available_features,
            config_bytes,
        })
    }

    pub const fn available_features(&self) -> u64 {
        self.available_features
    }

    pub const fn is_read_only(&self) -> bool {
        self.available_features & VIRTIO_BLK_F_RO != 0
    }

    pub const fn config_bytes(&self) -> &[u8; VIRTIO_BLOCK_CONFIG_SIZE] {
        &self.config_bytes
    }

    fn into_parts(self) -> (VhostUserFrontend, u64, [u8; VIRTIO_BLOCK_CONFIG_SIZE]) {
        (self.frontend, self.available_features, self.config_bytes)
    }
}

/// Redacted pre-VM vhost-user block discovery failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedVhostUserBlockFrontendError {
    Frontend(VhostUserError),
    MissingRequiredVirtioFeatures,
    MissingConfigProtocolFeature,
    InvalidConfig,
}

impl fmt::Display for PreparedVhostUserBlockFrontendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Frontend(source) => write!(formatter, "vhost-user discovery failed: {source}"),
            Self::MissingRequiredVirtioFeatures => {
                formatter.write_str("vhost-user backend lacks required virtio features")
            }
            Self::MissingConfigProtocolFeature => {
                formatter.write_str("vhost-user backend lacks configuration protocol support")
            }
            Self::InvalidConfig => {
                formatter.write_str("vhost-user backend returned invalid block configuration")
            }
        }
    }
}

impl std::error::Error for PreparedVhostUserBlockFrontendError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Frontend(source) => Some(source),
            Self::MissingRequiredVirtioFeatures
            | Self::MissingConfigProtocolFeature
            | Self::InvalidConfig => None,
        }
    }
}

impl fmt::Debug for DriveBackendConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File {
                is_read_only,
                io_engine,
                rate_limiter,
                ..
            } => formatter
                .debug_struct("File")
                .field("path_on_host", &"<redacted>")
                .field("is_read_only", is_read_only)
                .field("io_engine", io_engine)
                .field("rate_limiter", rate_limiter)
                .finish(),
            Self::VhostUser { .. } => formatter
                .debug_struct("VhostUser")
                .field("socket", &"<redacted>")
                .finish(),
        }
    }
}

impl fmt::Debug for DriveConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DriveConfig")
            .field("drive_id", &self.drive_id)
            .field("is_root_device", &self.is_root_device)
            .field("partuuid", &self.partuuid)
            .field("cache_type", &self.cache_type)
            .field("backend", &self.backend)
            .finish()
    }
}

impl DriveConfig {
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    pub fn backend(&self) -> &DriveBackendConfig {
        &self.backend
    }

    pub fn path_on_host(&self) -> Option<&Path> {
        match &self.backend {
            DriveBackendConfig::File { path_on_host, .. } => Some(path_on_host),
            DriveBackendConfig::VhostUser { .. } => None,
        }
    }

    pub fn socket(&self) -> Option<&Path> {
        match &self.backend {
            DriveBackendConfig::File { .. } => None,
            DriveBackendConfig::VhostUser { socket } => Some(socket),
        }
    }

    pub const fn is_vhost_user(&self) -> bool {
        matches!(self.backend, DriveBackendConfig::VhostUser { .. })
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root_device
    }

    pub const fn is_read_only(&self) -> Option<bool> {
        match self.backend {
            DriveBackendConfig::File { is_read_only, .. } => Some(is_read_only),
            DriveBackendConfig::VhostUser { .. } => None,
        }
    }

    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    pub const fn cache_type(&self) -> DriveCacheType {
        self.cache_type
    }

    pub const fn io_engine(&self) -> Option<DriveIoEngine> {
        match self.backend {
            DriveBackendConfig::File { io_engine, .. } => Some(io_engine),
            DriveBackendConfig::VhostUser { .. } => None,
        }
    }

    pub const fn rate_limiter(&self) -> Option<DriveRateLimiterConfig> {
        match self.backend {
            DriveBackendConfig::File { rate_limiter, .. } => rate_limiter,
            DriveBackendConfig::VhostUser { .. } => None,
        }
    }

    fn updated(&self, update: &DriveUpdate) -> Result<Self, DriveUpdateError> {
        if self.drive_id() != update.drive_id() {
            return Err(DriveUpdateError::UnknownDrive {
                drive_id: update.drive_id().to_string(),
            });
        }

        let DriveBackendConfig::File {
            path_on_host,
            is_read_only,
            io_engine,
            rate_limiter,
        } = &self.backend
        else {
            if update.path_on_host().is_some() || update.rate_limiter().is_some() {
                return Err(DriveUpdateError::UnsupportedBackend);
            }
            return Ok(self.clone());
        };
        Ok(Self {
            drive_id: self.drive_id.clone(),
            is_root_device: self.is_root_device,
            partuuid: self.partuuid.clone(),
            cache_type: self.cache_type,
            backend: DriveBackendConfig::File {
                path_on_host: update
                    .path_on_host()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| path_on_host.clone()),
                is_read_only: *is_read_only,
                io_engine: *io_engine,
                rate_limiter: match update.rate_limiter() {
                    Some(update) => update.applied_to(*rate_limiter),
                    None => *rate_limiter,
                },
            },
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriveConfigs {
    configs: Vec<DriveConfig>,
}

/// A validated runtime-only drive insertion whose backing device is not yet live.
#[derive(Debug)]
pub struct PreparedDriveConfigInsert {
    config: DriveConfig,
}

impl PreparedDriveConfigInsert {
    pub const fn config(&self) -> &DriveConfig {
        &self.config
    }
}

/// A validated runtime-only drive removal whose live device is not yet removed.
#[derive(Debug)]
pub struct PreparedDriveConfigRemoval {
    drive_id: String,
    index: usize,
}

impl PreparedDriveConfigRemoval {
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }
}

impl DriveConfigs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_slice(&self) -> &[DriveConfig] {
        &self.configs
    }

    pub fn has_root_device(&self) -> bool {
        self.configs.iter().any(DriveConfig::is_root_device)
    }

    pub(crate) fn from_validated_single(
        config: DriveConfig,
    ) -> Result<Self, std::collections::TryReserveError> {
        let mut configs = Vec::new();
        configs.try_reserve_exact(1)?;
        configs.push(config);
        Ok(Self { configs })
    }

    pub fn insert(&mut self, input: DriveConfigInput) -> Result<(), DriveConfigError> {
        let config = self.validate_insert(input)?;
        self.commit_insert(config);
        Ok(())
    }

    /// Validates an insertion without mutating the configured drive set.
    pub fn validate_insert(
        &self,
        input: DriveConfigInput,
    ) -> Result<DriveConfig, DriveConfigError> {
        let config = input.validate()?;
        if config.is_root_device()
            && self.configs.iter().any(|existing| {
                existing.is_root_device() && existing.drive_id() != config.drive_id()
            })
        {
            return Err(DriveConfigError::RootDeviceAlreadyConfigured);
        }

        Ok(config)
    }

    /// Commits a drive configuration that was checked by [`Self::validate_insert`].
    pub fn commit_insert(&mut self, config: DriveConfig) {
        let is_root_device = config.is_root_device();
        if let Some(index) = self
            .configs
            .iter()
            .position(|existing| existing.drive_id() == config.drive_id())
        {
            self.configs.remove(index);
            self.configs.insert(index, config);
            if is_root_device && index != 0 {
                self.configs.swap(0, index);
            }
        } else if is_root_device {
            self.configs.insert(0, config);
        } else {
            self.configs.push(config);
        }
    }

    pub fn updated_config(&self, input: DriveUpdateInput) -> Result<DriveConfig, DriveUpdateError> {
        let update = input.validate()?;
        let Some(existing) = self
            .configs
            .iter()
            .find(|config| config.drive_id() == update.drive_id())
        else {
            return Err(DriveUpdateError::UnknownDrive {
                drive_id: update.drive_id().to_string(),
            });
        };

        existing.updated(&update)
    }

    /// Validates a same-ID live replacement without mutating the configured set.
    pub fn validate_runtime_replacement(
        &self,
        replacement: DriveConfig,
    ) -> Result<DriveConfig, DriveUpdateError> {
        let drive_id = replacement.drive_id().to_string();
        let Some(existing) = self
            .configs
            .iter()
            .find(|config| config.drive_id() == drive_id)
        else {
            return Err(DriveUpdateError::UnknownDrive { drive_id });
        };

        if existing.is_vhost_user() || replacement.is_vhost_user() {
            return Err(DriveUpdateError::UnsupportedBackend);
        }
        let mismatched_field = if existing.is_root_device() != replacement.is_root_device() {
            Some(DriveReplacementIdentityField::RootDevice)
        } else if existing.is_read_only() != replacement.is_read_only() {
            Some(DriveReplacementIdentityField::ReadOnly)
        } else if existing.partuuid() != replacement.partuuid() {
            Some(DriveReplacementIdentityField::Partuuid)
        } else if existing.cache_type() != replacement.cache_type() {
            Some(DriveReplacementIdentityField::CacheType)
        } else {
            None
        };
        if let Some(field) = mismatched_field {
            return Err(DriveUpdateError::ReplacementIdentityMismatch { drive_id, field });
        }

        Ok(replacement)
    }

    pub fn commit_update(&mut self, config: DriveConfig) -> Result<(), DriveUpdateError> {
        let drive_id = config.drive_id().to_string();
        let Some(existing) = self
            .configs
            .iter_mut()
            .find(|existing| existing.drive_id() == drive_id)
        else {
            return Err(DriveUpdateError::UnknownDrive { drive_id });
        };

        *existing = config;
        Ok(())
    }

    /// Validates and reserves storage for a post-start insertion without changing
    /// the configured drive projection.
    pub fn prepare_runtime_insert(
        &mut self,
        input: DriveConfigInput,
    ) -> Result<PreparedDriveConfigInsert, DriveRuntimeMutationError> {
        let config = input
            .validate()
            .map_err(DriveRuntimeMutationError::InvalidConfig)?;
        if config.is_root_device() {
            return Err(DriveRuntimeMutationError::RootInsertUnsupported);
        }
        if self
            .configs
            .iter()
            .any(|existing| existing.drive_id() == config.drive_id())
        {
            return Err(DriveRuntimeMutationError::DuplicateDrive {
                drive_id: config.drive_id().to_string(),
            });
        }
        self.configs
            .try_reserve_exact(1)
            .map_err(|_| DriveRuntimeMutationError::ConfigurationAllocation)?;
        Ok(PreparedDriveConfigInsert { config })
    }

    /// Publishes a previously prepared runtime insertion after its live endpoint
    /// has committed successfully.
    pub fn commit_runtime_insert(&mut self, prepared: PreparedDriveConfigInsert) {
        debug_assert!(!prepared.config.is_root_device());
        debug_assert!(
            !self
                .configs
                .iter()
                .any(|existing| existing.drive_id() == prepared.config.drive_id())
        );
        debug_assert!(self.configs.len() < self.configs.capacity());
        self.configs.push(prepared.config);
    }

    /// Validates a post-start removal without changing the configured drive
    /// projection.
    pub fn prepare_runtime_removal(
        &self,
        drive_id: &str,
    ) -> Result<PreparedDriveConfigRemoval, DriveRuntimeMutationError> {
        if drive_id.is_empty() {
            return Err(DriveRuntimeMutationError::EmptyDriveId);
        }
        if !drive_id
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
        {
            return Err(DriveRuntimeMutationError::InvalidDriveId {
                drive_id: drive_id.to_string(),
            });
        }
        let Some((index, config)) = self
            .configs
            .iter()
            .enumerate()
            .find(|(_, config)| config.drive_id() == drive_id)
        else {
            return Err(DriveRuntimeMutationError::UnknownDrive {
                drive_id: drive_id.to_string(),
            });
        };
        if config.is_root_device() {
            return Err(DriveRuntimeMutationError::RootRemovalUnsupported {
                drive_id: drive_id.to_string(),
            });
        }
        Ok(PreparedDriveConfigRemoval {
            drive_id: drive_id.to_string(),
            index,
        })
    }

    /// Commits a previously prepared removal after the live endpoint has been
    /// removed. The process controller serializes configuration mutations, so the
    /// prepared index remains stable until this call.
    pub fn commit_runtime_removal(&mut self, prepared: PreparedDriveConfigRemoval) {
        debug_assert_eq!(
            self.configs.get(prepared.index).map(DriveConfig::drive_id),
            Some(prepared.drive_id.as_str())
        );
        self.configs.remove(prepared.index);
    }
}

impl TryFrom<DriveConfigInput> for DriveConfig {
    type Error = DriveConfigError;

    fn try_from(input: DriveConfigInput) -> Result<Self, Self::Error> {
        validate_drive_id(DriveIdSource::Path, &input.path_drive_id)?;
        validate_drive_id(DriveIdSource::Body, &input.body_drive_id)?;
        if input.path_drive_id != input.body_drive_id {
            return Err(DriveConfigError::MismatchedDriveId {
                path_drive_id: input.path_drive_id,
                body_drive_id: input.body_drive_id,
            });
        }

        let cache_type = input.cache_type.unwrap_or_default();
        let backend = match (input.path_on_host, input.socket) {
            (Some(path_on_host), None) => {
                if path_on_host.as_os_str().is_empty() {
                    return Err(DriveConfigError::EmptyPathOnHost);
                }
                let io_engine = input.io_engine.unwrap_or_default();
                DriveBackendConfig::File {
                    path_on_host,
                    is_read_only: input.is_read_only.unwrap_or(false),
                    io_engine,
                    rate_limiter: match input.rate_limiter {
                        Some(rate_limiter) if rate_limiter.is_configured() => Some(rate_limiter),
                        _ => None,
                    },
                }
            }
            (None, Some(socket)) => {
                if socket.as_os_str().is_empty() {
                    return Err(DriveConfigError::EmptySocket);
                }
                if input.is_read_only.is_some()
                    || input.io_engine.is_some()
                    || input.rate_limiter.is_some()
                {
                    return Err(DriveConfigError::InvalidVhostUserConfiguration);
                }
                DriveBackendConfig::VhostUser { socket }
            }
            (None, None) => return Err(DriveConfigError::EmptyPathOnHost),
            (Some(_), Some(_)) => {
                return Err(DriveConfigError::InvalidVhostUserConfiguration);
            }
        };

        Ok(Self {
            drive_id: input.path_drive_id,
            is_root_device: input.is_root_device,
            partuuid: input.partuuid,
            cache_type,
            backend,
        })
    }
}

impl TryFrom<DriveUpdateInput> for DriveUpdate {
    type Error = DriveUpdateError;

    fn try_from(input: DriveUpdateInput) -> Result<Self, Self::Error> {
        validate_drive_update_id(DriveIdSource::Path, &input.path_drive_id)?;
        validate_drive_update_id(DriveIdSource::Body, &input.body_drive_id)?;
        if input.path_drive_id != input.body_drive_id {
            return Err(DriveUpdateError::MismatchedDriveId {
                path_drive_id: input.path_drive_id,
                body_drive_id: input.body_drive_id,
            });
        }

        if input
            .path_on_host
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(DriveUpdateError::EmptyPathOnHost);
        }

        Ok(Self {
            drive_id: input.path_drive_id,
            path_on_host: input.path_on_host,
            rate_limiter: input.rate_limiter,
        })
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DriveCacheType {
    #[default]
    Unsafe,
    Writeback,
}

impl fmt::Display for DriveCacheType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsafe => f.write_str("Unsafe"),
            Self::Writeback => f.write_str("Writeback"),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DriveIoEngine {
    #[default]
    Sync,
    Async,
}

impl fmt::Display for DriveIoEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sync => f.write_str("Sync"),
            Self::Async => f.write_str("Async"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveIdSource {
    Path,
    Body,
}

impl fmt::Display for DriveIdSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path => f.write_str("path drive_id"),
            Self::Body => f.write_str("body drive_id"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveConfigError {
    EmptyDriveId {
        source: DriveIdSource,
    },
    InvalidDriveId {
        source: DriveIdSource,
        drive_id: String,
    },
    MismatchedDriveId {
        path_drive_id: String,
        body_drive_id: String,
    },
    EmptyPathOnHost,
    EmptySocket,
    InvalidVhostUserConfiguration,
    IncompatibleMemoryHotplug,
    RootDeviceAlreadyConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveUpdateError {
    EmptyDriveId {
        source: DriveIdSource,
    },
    InvalidDriveId {
        source: DriveIdSource,
        drive_id: String,
    },
    MismatchedDriveId {
        path_drive_id: String,
        body_drive_id: String,
    },
    EmptyPathOnHost,
    UnsupportedBackend,
    ReplacementIdentityMismatch {
        drive_id: String,
        field: DriveReplacementIdentityField,
    },
    UnknownDrive {
        drive_id: String,
    },
    OpenBacking {
        drive_id: String,
        message: String,
    },
    HandlerLookup {
        drive_id: String,
        region_id: MmioRegionId,
        message: String,
    },
    ActiveSessionCommand {
        message: String,
    },
    TerminalActiveSessionCommand {
        message: String,
    },
    ActiveSessionUnavailable,
    MmioDispatcherUnavailable,
}

/// A guest-visible drive identity field that same-ID live replacement cannot change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveReplacementIdentityField {
    RootDevice,
    ReadOnly,
    Partuuid,
    CacheType,
}

impl fmt::Display for DriveReplacementIdentityField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RootDevice => "is_root_device",
            Self::ReadOnly => "is_read_only",
            Self::Partuuid => "partuuid",
            Self::CacheType => "cache_type",
        })
    }
}

/// Redacted failure while preparing or committing a post-start block-device
/// insertion or removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveRuntimeMutationError {
    InvalidConfig(DriveConfigError),
    EmptyDriveId,
    InvalidDriveId { drive_id: String },
    DuplicateDrive { drive_id: String },
    RootInsertUnsupported,
    RootRemovalUnsupported { drive_id: String },
    UnknownDrive { drive_id: String },
    ConfigurationAllocation,
    PciNotEnabled,
    ActiveSessionUnavailable,
    ActiveSessionCommand { message: String },
    PrepareDevice { message: String },
    PublishDevice { message: String },
    TerminalInsertion { message: String },
    RemoveDevice { message: String },
    TerminalRemoval { message: String },
}

impl fmt::Display for DriveConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDriveId { source } => write!(f, "{source} must not be empty"),
            Self::InvalidDriveId { source, .. } => {
                write!(
                    f,
                    "{source} must contain only alphanumeric characters or '_'"
                )
            }
            Self::MismatchedDriveId { .. } => f.write_str("path drive_id must match body drive_id"),
            Self::EmptyPathOnHost => f.write_str("drive path_on_host must not be empty"),
            Self::EmptySocket => f.write_str("drive socket must not be empty"),
            Self::InvalidVhostUserConfiguration => {
                f.write_str("vhost-user drive contains incompatible file-backed fields")
            }
            Self::IncompatibleMemoryHotplug => {
                f.write_str("vhost-user drive is incompatible with dynamic memory hotplug")
            }
            Self::RootDeviceAlreadyConfigured => f.write_str("a root drive is already configured"),
        }
    }
}

impl std::error::Error for DriveConfigError {}

impl fmt::Display for DriveUpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDriveId { source } => write!(f, "{source} must not be empty"),
            Self::InvalidDriveId { source, .. } => {
                write!(
                    f,
                    "{source} must contain only alphanumeric characters or '_'"
                )
            }
            Self::MismatchedDriveId { .. } => f.write_str("path drive_id must match body drive_id"),
            Self::EmptyPathOnHost => f.write_str("drive path_on_host must not be empty"),
            Self::UnsupportedBackend => {
                f.write_str("file-backed drive updates are unsupported for vhost-user drives")
            }
            Self::ReplacementIdentityMismatch { drive_id, field } => {
                write!(
                    f,
                    "live replacement for drive {drive_id} cannot change {field}"
                )
            }
            Self::UnknownDrive { drive_id } => {
                write!(f, "drive {drive_id} is not configured")
            }
            Self::OpenBacking { message, .. } => {
                write!(f, "failed to open updated drive backing: {message}")
            }
            Self::HandlerLookup {
                drive_id,
                region_id,
                message,
            } => write!(
                f,
                "failed to find active drive {drive_id} handler for MMIO region {region_id}: {message}"
            ),
            Self::ActiveSessionCommand { message } => {
                write!(f, "active drive update command failed: {message}")
            }
            Self::TerminalActiveSessionCommand { message } => {
                write!(f, "active drive update entered terminal state: {message}")
            }
            Self::ActiveSessionUnavailable => {
                f.write_str("active drive update session is unavailable")
            }
            Self::MmioDispatcherUnavailable => {
                f.write_str("active drive MMIO dispatcher is unavailable")
            }
        }
    }
}

impl std::error::Error for DriveUpdateError {}

impl fmt::Display for DriveRuntimeMutationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(source) => write!(f, "{source}"),
            Self::EmptyDriveId => f.write_str("path drive_id must not be empty"),
            Self::InvalidDriveId { .. } => {
                f.write_str("path drive_id must contain only alphanumeric characters or '_'")
            }
            Self::DuplicateDrive { .. } => f.write_str("drive is already configured"),
            Self::RootInsertUnsupported => {
                f.write_str("a root drive cannot be inserted after the microVM starts")
            }
            Self::RootRemovalUnsupported { .. } => f.write_str("root drive cannot be removed"),
            Self::UnknownDrive { .. } => f.write_str("drive is not configured"),
            Self::ConfigurationAllocation => {
                f.write_str("failed to reserve runtime drive configuration storage")
            }
            Self::PciNotEnabled => {
                f.write_str("runtime drive insertion and removal require PCI transport")
            }
            Self::ActiveSessionUnavailable => {
                f.write_str("active runtime drive session is unavailable")
            }
            Self::ActiveSessionCommand { message } => {
                write!(f, "runtime drive command failed: {message}")
            }
            Self::PrepareDevice { message } => {
                write!(f, "failed to prepare runtime drive: {message}")
            }
            Self::PublishDevice { message } => {
                write!(f, "failed to publish runtime drive: {message}")
            }
            Self::TerminalInsertion { message } => {
                write!(
                    f,
                    "runtime drive insertion entered terminal cleanup: {message}"
                )
            }
            Self::RemoveDevice { message } => {
                write!(f, "failed to remove runtime drive: {message}")
            }
            Self::TerminalRemoval { message } => {
                write!(
                    f,
                    "runtime drive removal entered terminal cleanup: {message}"
                )
            }
        }
    }
}

impl std::error::Error for DriveRuntimeMutationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidConfig(source) => Some(source),
            Self::EmptyDriveId
            | Self::InvalidDriveId { .. }
            | Self::DuplicateDrive { .. }
            | Self::RootInsertUnsupported
            | Self::RootRemovalUnsupported { .. }
            | Self::UnknownDrive { .. }
            | Self::ConfigurationAllocation
            | Self::PciNotEnabled
            | Self::ActiveSessionUnavailable
            | Self::ActiveSessionCommand { .. }
            | Self::PrepareDevice { .. }
            | Self::PublishDevice { .. }
            | Self::TerminalInsertion { .. }
            | Self::RemoveDevice { .. }
            | Self::TerminalRemoval { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockConfigSpace {
    bytes: [u8; VIRTIO_BLOCK_CONFIG_SIZE],
    len: u8,
    capacity_sectors: u64,
    is_read_only: bool,
    cache_type: DriveCacheType,
    available_features: u64,
}

impl VirtioBlockConfigSpace {
    pub fn new(backing_len: u64, is_read_only: bool, cache_type: DriveCacheType) -> Self {
        let capacity_sectors = backing_len >> VIRTIO_BLOCK_SECTOR_SHIFT;
        let mut bytes = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        let capacity = capacity_sectors.to_le_bytes();
        for (destination, source) in bytes.iter_mut().zip(capacity) {
            *destination = source;
        }
        let mut available_features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_INDIRECT_DESC)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX);
        if matches!(cache_type, DriveCacheType::Writeback) {
            available_features |= virtio_feature_bit(VIRTIO_BLOCK_FEATURE_FLUSH);
        }
        if is_read_only {
            available_features |= virtio_feature_bit(VIRTIO_BLOCK_FEATURE_READ_ONLY);
        }
        Self {
            bytes,
            len: VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE as u8,
            capacity_sectors,
            is_read_only,
            cache_type,
            available_features,
        }
    }

    pub const fn from_vhost_user(
        bytes: [u8; VIRTIO_BLOCK_CONFIG_SIZE],
        available_features: u64,
        cache_type: DriveCacheType,
    ) -> Self {
        let capacity_sectors = u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]);
        Self {
            bytes,
            len: VIRTIO_BLOCK_CONFIG_SIZE as u8,
            capacity_sectors,
            is_read_only: available_features & virtio_feature_bit(VIRTIO_BLOCK_FEATURE_READ_ONLY)
                != 0,
            cache_type,
            available_features,
        }
    }

    pub fn from_backing(backing: &BlockFileBacking, cache_type: DriveCacheType) -> Self {
        Self::new(backing.len(), backing.is_read_only(), cache_type)
    }

    pub const fn capacity_sectors(self) -> u64 {
        self.capacity_sectors
    }

    pub const fn is_read_only(self) -> bool {
        self.is_read_only
    }

    pub const fn cache_type(self) -> DriveCacheType {
        self.cache_type
    }

    pub const fn available_features(self) -> u64 {
        self.available_features
    }

    pub const fn config_len(self) -> usize {
        self.len as usize
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioBlockConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let config = self.bytes.get(..self.config_len()).ok_or(
            VirtioMmioDeviceConfigError::UnsupportedRead {
                offset: access.offset(),
                len: access.len(),
            },
        )?;
        let bytes = read_virtio_block_config_bytes(config, access)?;
        MmioAccessBytes::new(bytes).map_err(config_bytes_error)
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

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

fn virtio_feature_enabled(features: u64, feature: u32) -> bool {
    features & virtio_feature_bit(feature) != 0
}

fn read_virtio_block_config_bytes(
    config: &[u8],
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

    config
        .get(offset..end)
        .ok_or(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        })
}

fn config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: crate::mmio::MmioHandlerError::new(format!(
            "virtio-block config access bytes failed: {source}"
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBlockRequestType {
    In,
    Out,
    Flush,
    GetDeviceId,
    Unsupported(u32),
}

impl VirtioBlockRequestType {
    pub const fn raw_value(self) -> u32 {
        match self {
            Self::In => VIRTIO_BLOCK_REQUEST_TYPE_IN,
            Self::Out => VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            Self::Flush => VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            Self::GetDeviceId => VIRTIO_BLOCK_REQUEST_TYPE_GET_ID,
            Self::Unsupported(value) => value,
        }
    }
}

impl From<u32> for VirtioBlockRequestType {
    fn from(value: u32) -> Self {
        match value {
            VIRTIO_BLOCK_REQUEST_TYPE_IN => Self::In,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT => Self::Out,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH => Self::Flush,
            VIRTIO_BLOCK_REQUEST_TYPE_GET_ID => Self::GetDeviceId,
            value => Self::Unsupported(value),
        }
    }
}

impl fmt::Display for VirtioBlockRequestType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::In => f.write_str("In"),
            Self::Out => f.write_str("Out"),
            Self::Flush => f.write_str("Flush"),
            Self::GetDeviceId => f.write_str("GetDeviceId"),
            Self::Unsupported(value) => write!(f, "Unsupported({value})"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockDataDescriptor {
    index: u16,
    address: GuestAddress,
    len: u32,
    is_write_only: bool,
}

impl VirtioBlockDataDescriptor {
    pub const fn index(self) -> u16 {
        self.index
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

    pub const fn is_write_only(self) -> bool {
        self.is_write_only
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockStatusDescriptor {
    index: u16,
    address: GuestAddress,
    len: u32,
}

impl VirtioBlockStatusDescriptor {
    pub const fn index(self) -> u16 {
        self.index
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
pub struct VirtioBlockRequest {
    descriptor_head: u16,
    request_type: VirtioBlockRequestType,
    sector: u64,
    data: Option<VirtioBlockDataDescriptor>,
    status: VirtioBlockStatusDescriptor,
}

impl VirtioBlockRequest {
    pub fn parse(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
        capacity_sectors: u64,
    ) -> Result<Self, VirtioBlockRequestError> {
        let header = descriptor_at(chain, 0, 1)?;
        validate_header_descriptor(header)?;
        let header_data = read_virtio_block_request_header(memory, header)?;
        let request_type = VirtioBlockRequestType::from(header_data.request_type);
        let (data, status_descriptor) =
            split_virtio_block_request_descriptors(chain, request_type)?;

        if let Some(data) = data {
            validate_data_descriptor_direction(request_type, data)?;
            validate_data_descriptor_length(
                request_type,
                data,
                header_data.sector,
                capacity_sectors,
            )?;
        }

        let status = validate_status_descriptor(status_descriptor)?;

        Ok(Self {
            descriptor_head: chain.head_index(),
            request_type,
            sector: header_data.sector,
            data,
            status,
        })
    }

    pub const fn descriptor_head(&self) -> u16 {
        self.descriptor_head
    }

    pub const fn request_type(&self) -> VirtioBlockRequestType {
        self.request_type
    }

    pub const fn sector(&self) -> u64 {
        self.sector
    }

    pub const fn data(&self) -> Option<VirtioBlockDataDescriptor> {
        self.data
    }

    pub const fn status(&self) -> VirtioBlockStatusDescriptor {
        self.status
    }

    const fn rate_limiter_bandwidth_bytes(&self) -> u64 {
        match self.request_type {
            VirtioBlockRequestType::In | VirtioBlockRequestType::Out => match self.data {
                Some(data) => data.len() as u64,
                None => 0,
            },
            VirtioBlockRequestType::Flush
            | VirtioBlockRequestType::GetDeviceId
            | VirtioBlockRequestType::Unsupported(_) => 0,
        }
    }

    pub fn execute(
        &self,
        memory: &mut GuestMemory,
        backing: &BlockFileBacking,
        device_id: VirtioBlockDeviceId,
    ) -> VirtioBlockRequestExecution {
        let (status_code, bytes_written_to_guest, outcome, latency_sample) =
            match self.execute_side_effects(memory, backing, device_id) {
                Ok(VirtioBlockRequestSideEffect::Completed {
                    bytes_written_to_guest,
                    latency_sample,
                }) => (
                    VIRTIO_BLOCK_STATUS_OK,
                    bytes_written_to_guest,
                    VirtioBlockRequestExecutionOutcome::Ok,
                    latency_sample,
                ),
                Ok(VirtioBlockRequestSideEffect::Unsupported { request_type }) => (
                    VIRTIO_BLOCK_STATUS_UNSUPPORTED,
                    0,
                    VirtioBlockRequestExecutionOutcome::Unsupported { request_type },
                    None,
                ),
                Err(error) => (
                    VIRTIO_BLOCK_STATUS_IOERR,
                    0,
                    VirtioBlockRequestExecutionOutcome::IoError { error: error.error },
                    error.latency_sample,
                ),
            };

        let (status_code, bytes_written_to_guest, outcome) =
            normalize_completion_status(status_code, bytes_written_to_guest, outcome);
        self.finish_execution(
            memory,
            status_code,
            bytes_written_to_guest,
            outcome,
            latency_sample,
        )
    }

    fn execute_side_effects(
        &self,
        memory: &mut GuestMemory,
        backing: &BlockFileBacking,
        device_id: VirtioBlockDeviceId,
    ) -> Result<VirtioBlockRequestSideEffect, VirtioBlockRequestSideEffectError> {
        match self.request_type {
            VirtioBlockRequestType::In => {
                let started_at = Instant::now();
                let bytes_written_to_guest = self.execute_in(memory, backing).map_err(|error| {
                    VirtioBlockRequestSideEffectError::new(
                        error,
                        Some(VirtioBlockRequestLatencySample::new(
                            VirtioBlockRequestType::In,
                            elapsed_microseconds_since(started_at),
                        )),
                    )
                })?;
                Ok(VirtioBlockRequestSideEffect::Completed {
                    bytes_written_to_guest,
                    latency_sample: Some(VirtioBlockRequestLatencySample::new(
                        VirtioBlockRequestType::In,
                        elapsed_microseconds_since(started_at),
                    )),
                })
            }
            VirtioBlockRequestType::Out => {
                let started_at = Instant::now();
                let bytes_written_to_guest =
                    self.execute_out(memory, backing).map_err(|error| {
                        VirtioBlockRequestSideEffectError::new(
                            error,
                            Some(VirtioBlockRequestLatencySample::new(
                                VirtioBlockRequestType::Out,
                                elapsed_microseconds_since(started_at),
                            )),
                        )
                    })?;
                Ok(VirtioBlockRequestSideEffect::Completed {
                    bytes_written_to_guest,
                    latency_sample: Some(VirtioBlockRequestLatencySample::new(
                        VirtioBlockRequestType::Out,
                        elapsed_microseconds_since(started_at),
                    )),
                })
            }
            VirtioBlockRequestType::Flush => Ok(VirtioBlockRequestSideEffect::Completed {
                bytes_written_to_guest: self
                    .execute_flush(backing)
                    .map_err(VirtioBlockRequestSideEffectError::without_latency)?,
                latency_sample: None,
            }),
            VirtioBlockRequestType::GetDeviceId => Ok(VirtioBlockRequestSideEffect::Completed {
                bytes_written_to_guest: self
                    .execute_get_device_id(memory, device_id)
                    .map_err(VirtioBlockRequestSideEffectError::without_latency)?,
                latency_sample: None,
            }),
            VirtioBlockRequestType::Unsupported(request_type) => {
                Ok(VirtioBlockRequestSideEffect::Unsupported { request_type })
            }
        }
    }

    fn execute_in(
        &self,
        memory: &mut GuestMemory,
        backing: &BlockFileBacking,
    ) -> Result<u32, VirtioBlockRequestExecutionError> {
        let data = self.required_data_descriptor()?;
        validate_guest_data_write(memory, self.request_type, data)?;
        let mut buffer = request_data_buffer(data.len())?;
        backing
            .read_at(self.byte_offset()?, &mut buffer)
            .map_err(|source| VirtioBlockRequestExecutionError::Backing {
                request_type: self.request_type,
                source,
            })?;
        memory
            .write_slice(&buffer, data.address())
            .map_err(
                |source| VirtioBlockRequestExecutionError::GuestMemoryWrite {
                    request_type: self.request_type,
                    address: data.address(),
                    len: data.len(),
                    source,
                },
            )?;
        Ok(data.len())
    }

    fn execute_out(
        &self,
        memory: &GuestMemory,
        backing: &BlockFileBacking,
    ) -> Result<u32, VirtioBlockRequestExecutionError> {
        let data = self.required_data_descriptor()?;
        let mut buffer = request_data_buffer(data.len())?;
        memory
            .read_slice(&mut buffer, data.address())
            .map_err(|source| VirtioBlockRequestExecutionError::GuestMemoryRead {
                request_type: self.request_type,
                address: data.address(),
                len: data.len(),
                source,
            })?;
        backing
            .write_at(self.byte_offset()?, &buffer)
            .map_err(|source| VirtioBlockRequestExecutionError::Backing {
                request_type: self.request_type,
                source,
            })?;
        Ok(0)
    }

    fn execute_flush(
        &self,
        backing: &BlockFileBacking,
    ) -> Result<u32, VirtioBlockRequestExecutionError> {
        backing
            .flush()
            .map_err(|source| VirtioBlockRequestExecutionError::Backing {
                request_type: self.request_type,
                source,
            })?;
        Ok(0)
    }

    fn execute_get_device_id(
        &self,
        memory: &mut GuestMemory,
        device_id: VirtioBlockDeviceId,
    ) -> Result<u32, VirtioBlockRequestExecutionError> {
        let data = self.required_data_descriptor()?;
        memory
            .write_slice(device_id.as_bytes(), data.address())
            .map_err(
                |source| VirtioBlockRequestExecutionError::GuestMemoryWrite {
                    request_type: self.request_type,
                    address: data.address(),
                    len: VIRTIO_BLOCK_ID_BYTES,
                    source,
                },
            )?;
        Ok(VIRTIO_BLOCK_ID_BYTES)
    }

    fn required_data_descriptor(
        &self,
    ) -> Result<VirtioBlockDataDescriptor, VirtioBlockRequestExecutionError> {
        self.data
            .ok_or(VirtioBlockRequestExecutionError::MissingDataDescriptor {
                request_type: self.request_type,
            })
    }

    fn byte_offset(&self) -> Result<u64, VirtioBlockRequestExecutionError> {
        self.sector.checked_mul(VIRTIO_BLOCK_SECTOR_SIZE).ok_or(
            VirtioBlockRequestExecutionError::SectorOffsetOverflow {
                sector: self.sector,
            },
        )
    }

    fn async_operation(
        &self,
    ) -> Result<Option<BlockAsyncOperation>, BlockAsyncOperationBuildError> {
        let identity =
            BlockAsyncRequestIdentity::new(0, self.descriptor_head, self.status.address());
        match self.request_type {
            VirtioBlockRequestType::In | VirtioBlockRequestType::Out => {
                let Some(data) = self.data else {
                    return Err(BlockAsyncOperationBuildError::RangeOverflow);
                };
                if data.is_empty() {
                    return Ok(None);
                }
                let host_offset = self
                    .sector
                    .checked_mul(VIRTIO_BLOCK_SECTOR_SIZE)
                    .ok_or(BlockAsyncOperationBuildError::RangeOverflow)?;
                if self.request_type == VirtioBlockRequestType::In {
                    BlockAsyncOperation::read(identity, data.address(), host_offset, data.len())
                        .map(Some)
                } else {
                    BlockAsyncOperation::write(identity, data.address(), host_offset, data.len())
                        .map(Some)
                }
            }
            VirtioBlockRequestType::Flush => Ok(Some(BlockAsyncOperation::flush(identity))),
            VirtioBlockRequestType::GetDeviceId | VirtioBlockRequestType::Unsupported(_) => {
                Ok(None)
            }
        }
    }

    fn finish_async_operation_error(
        &self,
        memory: &mut GuestMemory,
        source: BlockAsyncOperationBuildError,
    ) -> VirtioBlockRequestExecution {
        let latency_sample = matches!(
            self.request_type,
            VirtioBlockRequestType::In | VirtioBlockRequestType::Out
        )
        .then(|| VirtioBlockRequestLatencySample::new(self.request_type, 0));
        self.finish_execution(
            memory,
            VIRTIO_BLOCK_STATUS_IOERR,
            0,
            VirtioBlockRequestExecutionOutcome::IoError {
                error: VirtioBlockRequestExecutionError::AsyncOperation { source },
            },
            latency_sample,
        )
    }

    fn finish_execution(
        &self,
        memory: &mut GuestMemory,
        status_code: u8,
        bytes_written_to_guest: u32,
        outcome: VirtioBlockRequestExecutionOutcome,
        latency_sample: Option<VirtioBlockRequestLatencySample>,
    ) -> VirtioBlockRequestExecution {
        match write_request_status(memory, self.status, status_code) {
            Ok(()) => {
                let completion = VirtioBlockRequestCompletion::new(
                    self.descriptor_head,
                    bytes_written_to_guest + VIRTIO_BLOCK_STATUS_SIZE,
                );
                VirtioBlockRequestExecution::new_with_latency(
                    completion,
                    status_code,
                    outcome,
                    latency_sample,
                )
            }
            Err(source) => {
                let completion = VirtioBlockRequestCompletion::new(self.descriptor_head, 0);
                VirtioBlockRequestExecution::new_with_latency(
                    completion,
                    status_code,
                    VirtioBlockRequestExecutionOutcome::StatusWriteFailed {
                        status_code,
                        source,
                    },
                    latency_sample,
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioBlockRequestSideEffect {
    Completed {
        bytes_written_to_guest: u32,
        latency_sample: Option<VirtioBlockRequestLatencySample>,
    },
    Unsupported {
        request_type: u32,
    },
}

#[derive(Debug)]
struct VirtioBlockRequestSideEffectError {
    error: VirtioBlockRequestExecutionError,
    latency_sample: Option<VirtioBlockRequestLatencySample>,
}

impl VirtioBlockRequestSideEffectError {
    const fn new(
        error: VirtioBlockRequestExecutionError,
        latency_sample: Option<VirtioBlockRequestLatencySample>,
    ) -> Self {
        Self {
            error,
            latency_sample,
        }
    }

    const fn without_latency(error: VirtioBlockRequestExecutionError) -> Self {
        Self::new(error, None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockDeviceId {
    bytes: [u8; VIRTIO_BLOCK_ID_BYTES as usize],
}

impl VirtioBlockDeviceId {
    pub const fn new(bytes: [u8; VIRTIO_BLOCK_ID_BYTES as usize]) -> Self {
        Self { bytes }
    }

    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut id = [0; VIRTIO_BLOCK_ID_BYTES as usize];
        for (destination, source) in id.iter_mut().zip(bytes.iter().copied()) {
            *destination = source;
        }
        Self { bytes: id }
    }

    pub const fn as_bytes(&self) -> &[u8; VIRTIO_BLOCK_ID_BYTES as usize] {
        &self.bytes
    }
}

impl Default for VirtioBlockDeviceId {
    fn default() -> Self {
        Self {
            bytes: [0; VIRTIO_BLOCK_ID_BYTES as usize],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockRequestCompletion {
    descriptor_head: u16,
    bytes_written_to_guest: u32,
}

impl VirtioBlockRequestCompletion {
    pub const fn new(descriptor_head: u16, bytes_written_to_guest: u32) -> Self {
        Self {
            descriptor_head,
            bytes_written_to_guest,
        }
    }

    pub const fn descriptor_head(self) -> u16 {
        self.descriptor_head
    }

    pub const fn bytes_written_to_guest(self) -> u32 {
        self.bytes_written_to_guest
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioBlockLatencyAggregate {
    min_us: u64,
    max_us: u64,
    sum_us: u64,
    sample_count: u64,
}

impl VirtioBlockLatencyAggregate {
    pub(crate) const fn new(min_us: u64, max_us: u64, sum_us: u64, sample_count: u64) -> Self {
        if sample_count == 0 {
            return Self {
                min_us: 0,
                max_us: 0,
                sum_us: 0,
                sample_count: 0,
            };
        }

        Self {
            min_us,
            max_us,
            sum_us,
            sample_count,
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

    pub(crate) const fn sample_count(self) -> u64 {
        self.sample_count
    }

    pub const fn is_empty(self) -> bool {
        self.sample_count == 0
    }

    const fn from_sample(latency_us: u64) -> Self {
        Self {
            min_us: latency_us,
            max_us: latency_us,
            sum_us: latency_us,
            sample_count: 1,
        }
    }

    pub(crate) const fn merged_with(mut self, other: Self) -> Self {
        if other.is_empty() {
            return self;
        }

        if self.is_empty() || other.min_us < self.min_us {
            self.min_us = other.min_us;
        }
        if other.max_us > self.max_us {
            self.max_us = other.max_us;
        }
        self.sum_us = self.sum_us.saturating_add(other.sum_us);
        self.sample_count = self.sample_count.saturating_add(other.sample_count);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VirtioBlockRequestLatencySample {
    request_type: VirtioBlockRequestType,
    latency_us: u64,
}

impl VirtioBlockRequestLatencySample {
    const fn new(request_type: VirtioBlockRequestType, latency_us: u64) -> Self {
        Self {
            request_type,
            latency_us,
        }
    }

    const fn request_type(self) -> VirtioBlockRequestType {
        self.request_type
    }

    const fn aggregate(self) -> VirtioBlockLatencyAggregate {
        VirtioBlockLatencyAggregate::from_sample(self.latency_us)
    }
}

#[derive(Debug)]
pub struct VirtioBlockRequestExecution {
    completion: VirtioBlockRequestCompletion,
    status_code: u8,
    outcome: VirtioBlockRequestExecutionOutcome,
    latency_sample: Option<VirtioBlockRequestLatencySample>,
}

impl VirtioBlockRequestExecution {
    pub const fn new(
        completion: VirtioBlockRequestCompletion,
        status_code: u8,
        outcome: VirtioBlockRequestExecutionOutcome,
    ) -> Self {
        Self::new_with_latency(completion, status_code, outcome, None)
    }

    const fn new_with_latency(
        completion: VirtioBlockRequestCompletion,
        status_code: u8,
        outcome: VirtioBlockRequestExecutionOutcome,
        latency_sample: Option<VirtioBlockRequestLatencySample>,
    ) -> Self {
        Self {
            completion,
            status_code,
            outcome,
            latency_sample,
        }
    }

    pub const fn completion(&self) -> VirtioBlockRequestCompletion {
        self.completion
    }

    pub const fn status_code(&self) -> u8 {
        self.status_code
    }

    pub const fn outcome(&self) -> &VirtioBlockRequestExecutionOutcome {
        &self.outcome
    }

    const fn latency_sample(&self) -> Option<VirtioBlockRequestLatencySample> {
        self.latency_sample
    }
}

#[derive(Debug)]
pub enum VirtioBlockRequestExecutionOutcome {
    Ok,
    IoError {
        error: VirtioBlockRequestExecutionError,
    },
    Unsupported {
        request_type: u32,
    },
    StatusWriteFailed {
        status_code: u8,
        source: GuestMemoryAccessError,
    },
}

fn elapsed_microseconds_since(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_micros()).unwrap_or(u64::MAX)
}

#[derive(Debug)]
pub enum VirtioBlockRequestExecutionError {
    AsyncOperation {
        source: BlockAsyncOperationBuildError,
    },
    MissingDataDescriptor {
        request_type: VirtioBlockRequestType,
    },
    SectorOffsetOverflow {
        sector: u64,
    },
    DataLengthTooLarge {
        len: u32,
    },
    CompletionLengthOverflow {
        bytes_written_to_guest: u32,
    },
    BufferAllocationFailed {
        len: u32,
        source: TryReserveError,
    },
    GuestMemoryRead {
        request_type: VirtioBlockRequestType,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    GuestMemoryWrite {
        request_type: VirtioBlockRequestType,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    Backing {
        request_type: VirtioBlockRequestType,
        source: BlockFileBackingError,
    },
}

impl fmt::Display for VirtioBlockRequestExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AsyncOperation { source } => {
                write!(
                    f,
                    "failed to prepare asynchronous virtio-block request: {source}"
                )
            }
            Self::MissingDataDescriptor { request_type } => {
                write!(
                    f,
                    "virtio-block {request_type} request is missing a data descriptor"
                )
            }
            Self::SectorOffsetOverflow { sector } => {
                write!(
                    f,
                    "virtio-block request sector {sector} overflows byte offset"
                )
            }
            Self::DataLengthTooLarge { len } => {
                write!(f, "virtio-block request data length {len} is too large")
            }
            Self::CompletionLengthOverflow {
                bytes_written_to_guest,
            } => {
                write!(
                    f,
                    "virtio-block request completion length overflows after {bytes_written_to_guest} guest bytes"
                )
            }
            Self::BufferAllocationFailed { len, source } => {
                write!(
                    f,
                    "failed to allocate virtio-block request buffer of {len} bytes: {source}"
                )
            }
            Self::GuestMemoryRead {
                request_type,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "failed to read {len} bytes from guest memory at {address} for virtio-block {request_type} request: {source}"
                )
            }
            Self::GuestMemoryWrite {
                request_type,
                address,
                len,
                source,
            } => {
                write!(
                    f,
                    "failed to write {len} bytes to guest memory at {address} for virtio-block {request_type} request: {source}"
                )
            }
            Self::Backing {
                request_type,
                source,
            } => {
                write!(
                    f,
                    "failed to access block backing for virtio-block {request_type} request: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBlockRequestExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AsyncOperation { source } => Some(source),
            Self::BufferAllocationFailed { source, .. } => Some(source),
            Self::GuestMemoryRead { source, .. } | Self::GuestMemoryWrite { source, .. } => {
                Some(source)
            }
            Self::Backing { source, .. } => Some(source),
            Self::MissingDataDescriptor { .. }
            | Self::SectorOffsetOverflow { .. }
            | Self::DataLengthTooLarge { .. }
            | Self::CompletionLengthOverflow { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioBlockQueueBuildError {
    QueueNotReady,
    AvailableRing { source: VirtqueueAvailableRingError },
    UsedRing { source: VirtqueueUsedRingError },
}

impl fmt::Display for VirtioBlockQueueBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("virtio-block queue is not ready"),
            Self::AvailableRing { source } => {
                write!(
                    f,
                    "failed to build virtio-block available ring from queue state: {source}"
                )
            }
            Self::UsedRing { source } => {
                write!(
                    f,
                    "failed to build virtio-block used ring from queue state: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBlockQueueBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source } => Some(source),
            Self::QueueNotReady => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockQueueState {
    next_available: u16,
    next_used: u16,
}

impl VirtioBlockQueueState {
    pub const fn new(next_available: u16, next_used: u16) -> Self {
        Self {
            next_available,
            next_used,
        }
    }

    pub const fn next_available(self) -> u16 {
        self.next_available
    }

    pub const fn next_used(self) -> u16 {
        self.next_used
    }
}

impl fmt::Debug for VirtioBlockQueueState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtioBlockQueueState")
            .field("cursors", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VirtioBlockQueueSnapshotError {
    QueueNotReady,
    AvailableRingInvalid,
    UsedRingInvalid,
    QueueRangeInvalid,
    QueueRangesOverlap,
    UsedCursorMismatch,
    AvailableCursorOutOfBounds,
    RetryWithoutPendingDescriptor,
}

impl fmt::Display for VirtioBlockQueueSnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("persisted virtio-block queue is not ready"),
            Self::AvailableRingInvalid => {
                f.write_str("persisted virtio-block available ring is invalid")
            }
            Self::UsedRingInvalid => f.write_str("persisted virtio-block used ring is invalid"),
            Self::QueueRangeInvalid => f.write_str("persisted virtio-block queue range is invalid"),
            Self::QueueRangesOverlap => f.write_str("persisted virtio-block queue ranges overlap"),
            Self::UsedCursorMismatch => {
                f.write_str("persisted virtio-block used cursor does not match guest memory")
            }
            Self::AvailableCursorOutOfBounds => f.write_str(
                "persisted virtio-block available cursor is inconsistent with guest memory",
            ),
            Self::RetryWithoutPendingDescriptor => {
                f.write_str("persisted virtio-block retry has no pending available descriptor")
            }
        }
    }
}

impl std::error::Error for VirtioBlockQueueSnapshotError {}

#[derive(Clone, PartialEq, Eq)]
pub struct VirtioBlockRuntimeState {
    transport: VirtioMmioTransportState,
    active_queue: Option<VirtioBlockQueueState>,
    rate_limiter: VirtioBlockRateLimiterState,
}

/// Live backend kind, including terminal vhost-user frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBlockBackendKind {
    File,
    VhostUser,
}

/// Exact file-engine continuation state at a capture boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockCaptureIoEngine {
    Sync,
    Async(BlockAsyncGenerationCaptureState),
}

/// Detached, value-redacted block state retained for later persistence.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockDeviceCaptureState {
    config_space: VirtioBlockConfigSpace,
    device_id: VirtioBlockDeviceId,
    backing: BlockFileBackingIdentity,
    active_queue: Option<VirtioBlockQueueState>,
    rate_limiter: VirtioBlockRateLimiterState,
    io_engine: BlockCaptureIoEngine,
}

impl VirtioBlockDeviceCaptureState {
    pub const fn config_space(self) -> VirtioBlockConfigSpace {
        self.config_space
    }

    pub const fn device_id(self) -> VirtioBlockDeviceId {
        self.device_id
    }

    pub const fn backing(self) -> BlockFileBackingIdentity {
        self.backing
    }

    pub const fn active_queue(self) -> Option<VirtioBlockQueueState> {
        self.active_queue
    }

    pub const fn rate_limiter(self) -> VirtioBlockRateLimiterState {
        self.rate_limiter
    }

    pub const fn io_engine(self) -> BlockCaptureIoEngine {
        self.io_engine
    }
}

impl fmt::Debug for VirtioBlockDeviceCaptureState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VirtioBlockDeviceCaptureState")
            .field("state", &"<redacted>")
            .finish()
    }
}

/// Value-redacted failure while detaching live block state.
#[derive(Debug)]
pub enum VirtioBlockDeviceCaptureError {
    UnsupportedBackend,
    ConfigurationMismatch,
    BackingIdentity,
    RateLimiter,
    Async(BlockAsyncRuntimeError),
}

impl fmt::Display for VirtioBlockDeviceCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBackend => {
                formatter.write_str("vhost-user block capture is unsupported")
            }
            Self::ConfigurationMismatch => {
                formatter.write_str("live block state does not match its configuration")
            }
            Self::BackingIdentity => {
                formatter.write_str("live block backing identity is unavailable")
            }
            Self::RateLimiter => formatter.write_str("live block rate limiter state is invalid"),
            Self::Async(_) => formatter.write_str("live Async block state is not capture-ready"),
        }
    }
}

impl std::error::Error for VirtioBlockDeviceCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Async(source) => Some(source),
            Self::UnsupportedBackend
            | Self::ConfigurationMismatch
            | Self::BackingIdentity
            | Self::RateLimiter => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioBlockPciCaptureError {
    Device(VirtioBlockDeviceCaptureError),
    Endpoint(VirtioPciEndpointError),
}

impl fmt::Display for VirtioBlockPciCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Device(_) => formatter.write_str("PCI block device capture failed"),
            Self::Endpoint(_) => formatter.write_str("PCI block transport capture failed"),
        }
    }
}

impl std::error::Error for VirtioBlockPciCaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Device(source) => Some(source),
            Self::Endpoint(source) => Some(source),
        }
    }
}

pub(crate) struct VirtioBlockRuntimeRestoreInput<'a> {
    pub backing: BlockFileBacking,
    pub device_id: VirtioBlockDeviceId,
    pub config_space: VirtioBlockConfigSpace,
    pub rate_limiter_config: Option<DriveRateLimiterConfig>,
    pub memory: &'a GuestMemory,
    pub has_retry: bool,
    pub now: Instant,
}

impl VirtioBlockRuntimeState {
    pub fn new(
        transport: VirtioMmioTransportState,
        active_queue: Option<VirtioBlockQueueState>,
        rate_limiter: VirtioBlockRateLimiterState,
    ) -> Self {
        Self {
            transport,
            active_queue,
            rate_limiter,
        }
    }

    pub const fn transport(&self) -> &VirtioMmioTransportState {
        &self.transport
    }

    pub const fn active_queue(&self) -> Option<VirtioBlockQueueState> {
        self.active_queue
    }

    pub const fn rate_limiter(&self) -> VirtioBlockRateLimiterState {
        self.rate_limiter
    }

    pub fn validate_guest_memory(
        &self,
        memory: &GuestMemory,
        has_retry: bool,
    ) -> Result<(), VirtioBlockRuntimeStateError> {
        validate_native_v1_block_runtime_shape(self)?;
        if has_retry && self.active_queue.is_none() {
            return Err(VirtioBlockRuntimeStateError::RetryWithoutActiveQueue);
        }
        if has_retry && self.rate_limiter.is_empty() {
            return Err(VirtioBlockRuntimeStateError::RetryWithoutRateLimiter);
        }
        let Some(queue) = build_snapshot_block_queue(self)? else {
            return Ok(());
        };
        queue
            .validate_snapshot_state(memory, has_retry)
            .map_err(|_| VirtioBlockRuntimeStateError::Queue)
    }

    pub(crate) fn restore_handler(
        &self,
        input: VirtioBlockRuntimeRestoreInput<'_>,
    ) -> Result<VirtioBlockMmioHandler, VirtioBlockRuntimeStateError> {
        self.validate_guest_memory(input.memory, input.has_retry)?;
        let active_queue = build_snapshot_block_queue(self)?;
        let rate_limiter = VirtioBlockRateLimiter::from_persisted_state_at(
            input.rate_limiter_config,
            self.rate_limiter,
            input.now,
        )
        .map_err(|_| VirtioBlockRuntimeStateError::RateLimiter)?;
        let device = VirtioBlockDevice::from_snapshot_parts(
            input.backing,
            input.device_id,
            active_queue,
            rate_limiter,
        );
        let activation_is_active = device.is_activated();
        let mut handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_BLOCK_DEVICE_ID,
            input.config_space.available_features(),
            &VIRTIO_BLOCK_QUEUE_SIZES,
            input.config_space,
            device,
        )
        .map_err(|_| VirtioBlockRuntimeStateError::HandlerBuild)?;
        handler
            .restore_transport_state(&self.transport, activation_is_active)
            .map_err(|_| VirtioBlockRuntimeStateError::Transport)?;
        Ok(handler)
    }
}

impl fmt::Debug for VirtioBlockRuntimeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtioBlockRuntimeState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBlockRuntimeStateError {
    Transport,
    QueueProfile,
    PendingQueueNotification,
    ActivationMismatch,
    Queue,
    RateLimiter,
    HandlerBuild,
    RetryWithoutActiveQueue,
    RetryWithoutRateLimiter,
}

impl fmt::Display for VirtioBlockRuntimeStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport => f.write_str("persisted virtio-block transport state is invalid"),
            Self::QueueProfile => {
                f.write_str("persisted virtio-block queue profile is unsupported")
            }
            Self::PendingQueueNotification => {
                f.write_str("virtio-block queue notification work is not drained")
            }
            Self::ActivationMismatch => {
                f.write_str("persisted virtio-block activation state is inconsistent")
            }
            Self::Queue => f.write_str("persisted virtio-block queue state is invalid"),
            Self::RateLimiter => {
                f.write_str("persisted virtio-block rate limiter state is invalid")
            }
            Self::HandlerBuild => f.write_str("failed to build persisted virtio-block handler"),
            Self::RetryWithoutActiveQueue => {
                f.write_str("persisted virtio-block retry requires an active queue")
            }
            Self::RetryWithoutRateLimiter => {
                f.write_str("persisted virtio-block retry requires an active rate limiter")
            }
        }
    }
}

impl std::error::Error for VirtioBlockRuntimeStateError {}

fn validate_native_v1_block_runtime_shape(
    state: &VirtioBlockRuntimeState,
) -> Result<(), VirtioBlockRuntimeStateError> {
    let transport = state.transport();
    let queues = transport.queues();
    if transport.queue_select() != 0
        || queues.len() != VIRTIO_BLOCK_QUEUE_COUNT
        || queues.first().map(|queue| queue.max_size()) != Some(VIRTIO_BLOCK_QUEUE_SIZE)
    {
        return Err(VirtioBlockRuntimeStateError::QueueProfile);
    }
    if transport.pending_notifications().len() != VIRTIO_BLOCK_QUEUE_COUNT {
        return Err(VirtioBlockRuntimeStateError::QueueProfile);
    }
    if transport
        .pending_notifications()
        .iter()
        .copied()
        .any(|pending| pending)
    {
        return Err(VirtioBlockRuntimeStateError::PendingQueueNotification);
    }
    if transport.is_device_activated() != state.active_queue().is_some() {
        return Err(VirtioBlockRuntimeStateError::ActivationMismatch);
    }
    Ok(())
}

fn build_snapshot_block_queue(
    state: &VirtioBlockRuntimeState,
) -> Result<Option<VirtioBlockQueue>, VirtioBlockRuntimeStateError> {
    validate_native_v1_block_runtime_shape(state)?;
    let Some(queue_state) = state.active_queue() else {
        return Ok(None);
    };
    let queue = state
        .transport()
        .queues()
        .first()
        .ok_or(VirtioBlockRuntimeStateError::QueueProfile)?;
    let driver_features = state.transport().device_registers().driver_features();
    let event_idx_enabled = virtio_feature_enabled(driver_features, VIRTIO_RING_FEATURE_EVENT_IDX);
    let indirect_descriptors_enabled =
        virtio_feature_enabled(driver_features, VIRTIO_RING_FEATURE_INDIRECT_DESC);
    VirtioBlockQueue::from_snapshot_state(
        queue,
        queue_state,
        event_idx_enabled,
        indirect_descriptors_enabled,
    )
    .map(Some)
    .map_err(|_| VirtioBlockRuntimeStateError::Queue)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBlockQueue {
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
    event_idx_enabled: bool,
}

impl VirtioBlockQueue {
    pub const fn new(available: VirtqueueAvailableRing, used: VirtqueueUsedRing) -> Self {
        Self {
            available,
            used,
            event_idx_enabled: false,
        }
    }

    pub fn from_mmio_queue_state(
        queue: &VirtioMmioQueueState,
    ) -> Result<Self, VirtioBlockQueueBuildError> {
        Self::from_mmio_queue_state_with_event_idx(queue, false, false)
    }

    fn from_mmio_queue_state_with_event_idx(
        queue: &VirtioMmioQueueState,
        event_idx_enabled: bool,
        indirect_descriptors_enabled: bool,
    ) -> Result<Self, VirtioBlockQueueBuildError> {
        if !queue.ready() {
            return Err(VirtioBlockQueueBuildError::QueueNotReady);
        }

        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioBlockQueueBuildError::AvailableRing { source })?;
        let available = available.with_descriptor_chain_options(
            VirtqueueDescriptorChainOptions::new()
                .with_indirect_descriptors(indirect_descriptors_enabled),
        );
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioBlockQueueBuildError::UsedRing { source })?;

        Ok(Self {
            available,
            used,
            event_idx_enabled,
        })
    }

    pub(crate) fn from_snapshot_state(
        queue: &VirtioMmioQueueState,
        state: VirtioBlockQueueState,
        event_idx_enabled: bool,
        indirect_descriptors_enabled: bool,
    ) -> Result<Self, VirtioBlockQueueSnapshotError> {
        if !queue.ready() {
            return Err(VirtioBlockQueueSnapshotError::QueueNotReady);
        }

        let available = VirtqueueAvailableRing::with_next_avail(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
            state.next_available(),
        )
        .map_err(|_| VirtioBlockQueueSnapshotError::AvailableRingInvalid)?
        .with_descriptor_chain_options(
            VirtqueueDescriptorChainOptions::new()
                .with_indirect_descriptors(indirect_descriptors_enabled),
        );
        let used =
            VirtqueueUsedRing::with_next_used(queue.device_ring(), queue.size(), state.next_used())
                .map_err(|_| VirtioBlockQueueSnapshotError::UsedRingInvalid)?;

        Ok(Self {
            available,
            used,
            event_idx_enabled,
        })
    }

    pub const fn snapshot_state(&self) -> VirtioBlockQueueState {
        VirtioBlockQueueState::new(self.available.next_avail(), self.used.next_used())
    }

    pub(crate) fn validate_snapshot_state(
        &self,
        memory: &GuestMemory,
        has_retry: bool,
    ) -> Result<(), VirtioBlockQueueSnapshotError> {
        self.available
            .validate_mapped(memory)
            .map_err(|_| VirtioBlockQueueSnapshotError::AvailableRingInvalid)?;
        self.used
            .validate_mapped(memory)
            .map_err(|_| VirtioBlockQueueSnapshotError::UsedRingInvalid)?;

        let descriptor_range = self
            .available
            .descriptor_table_range()
            .map_err(|_| VirtioBlockQueueSnapshotError::QueueRangeInvalid)?;
        let available_range = self
            .available
            .available_ring_range()
            .map_err(|_| VirtioBlockQueueSnapshotError::QueueRangeInvalid)?;
        let used_range = self
            .used
            .used_ring_range()
            .map_err(|_| VirtioBlockQueueSnapshotError::QueueRangeInvalid)?;
        if descriptor_range.overlaps(available_range)
            || descriptor_range.overlaps(used_range)
            || available_range.overlaps(used_range)
        {
            return Err(VirtioBlockQueueSnapshotError::QueueRangesOverlap);
        }

        let used_index = self
            .used
            .used_index(memory)
            .map_err(|_| VirtioBlockQueueSnapshotError::UsedRingInvalid)?;
        if used_index != self.used.next_used() {
            return Err(VirtioBlockQueueSnapshotError::UsedCursorMismatch);
        }

        let available_index = self
            .available
            .available_index(memory)
            .map_err(|_| VirtioBlockQueueSnapshotError::AvailableRingInvalid)?;
        let pending = available_index.wrapping_sub(self.available.next_avail());
        if pending > self.available.queue_size() {
            return Err(VirtioBlockQueueSnapshotError::AvailableCursorOutOfBounds);
        }
        if has_retry && pending == 0 {
            return Err(VirtioBlockQueueSnapshotError::RetryWithoutPendingDescriptor);
        }

        Ok(())
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

    pub fn dispatch(
        &mut self,
        memory: &mut GuestMemory,
        backing: &BlockFileBacking,
        device_id: VirtioBlockDeviceId,
    ) -> Result<VirtioBlockQueueDispatch, VirtioBlockQueueDispatchError> {
        self.dispatch_with_rate_limiter(memory, backing, device_id, None)
    }

    fn dispatch_with_rate_limiter(
        &mut self,
        memory: &mut GuestMemory,
        backing: &BlockFileBacking,
        device_id: VirtioBlockDeviceId,
        rate_limiter: Option<&mut VirtioBlockRateLimiter>,
    ) -> Result<VirtioBlockQueueDispatch, VirtioBlockQueueDispatchError> {
        self.dispatch_with_rate_limiter_at(memory, backing, device_id, rate_limiter, Instant::now())
    }

    fn dispatch_with_rate_limiter_at(
        &mut self,
        memory: &mut GuestMemory,
        backing: &BlockFileBacking,
        device_id: VirtioBlockDeviceId,
        rate_limiter: Option<&mut VirtioBlockRateLimiter>,
        now: Instant,
    ) -> Result<VirtioBlockQueueDispatch, VirtioBlockQueueDispatchError> {
        let mut dispatch = VirtioBlockQueueDispatch::default();
        let capacity_sectors = backing.len() >> VIRTIO_BLOCK_SECTOR_SHIFT;
        let mut rate_limiter = rate_limiter;
        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioBlockQueueDispatchError::AvailableRing {
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioBlockQueueDispatchError::EmptyDescriptorChain {
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;
            let (completion, outcome) =
                match VirtioBlockRequest::parse(memory, &chain, capacity_sectors) {
                    Ok(request) => {
                        if let Some(limiter) = rate_limiter.as_deref_mut() {
                            match limiter.reduce_request_at(&request, now) {
                                VirtioBlockRateLimiterReduction::Allowed => {}
                                VirtioBlockRateLimiterReduction::Throttled { retry_after } => {
                                    if let Err(source) = self.available.undo_pop_descriptor_chain()
                                    {
                                        return Err(VirtioBlockQueueDispatchError::AvailableRing {
                                            completed_dispatch: Box::new(dispatch.clone()),
                                            source,
                                        });
                                    }
                                    dispatch.record_rate_limited_request(retry_after);
                                    break;
                                }
                            }
                        }
                        let execution = request.execute(memory, backing, device_id);
                        (
                            execution.completion(),
                            VirtioBlockQueueDispatchOutcome::from_request_execution(
                                &request, &execution,
                            ),
                        )
                    }
                    Err(source) => (
                        VirtioBlockRequestCompletion::new(descriptor_head, 0),
                        VirtioBlockQueueDispatchOutcome::ParseError(source),
                    ),
                };

            let notification_suppression =
                self.notification_suppression(memory).map_err(|source| {
                    VirtioBlockQueueDispatchError::AvailableRing {
                        completed_dispatch: Box::new(dispatch.clone()),
                        source,
                    }
                })?;
            let publication = self
                .used
                .publish_used_element_with_notification(
                    memory,
                    completion.descriptor_head(),
                    completion.bytes_written_to_guest(),
                    notification_suppression,
                )
                .map_err(|source| VirtioBlockQueueDispatchError::UsedRing {
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head: completion.descriptor_head(),
                    bytes_written_to_guest: completion.bytes_written_to_guest(),
                    source,
                })?;
            dispatch.record(outcome, publication);
        }

        Ok(dispatch)
    }

    fn dispatch_with_async_runtime(
        &mut self,
        memory: &mut GuestMemory,
        backing: &BlockFileBacking,
        device_id: VirtioBlockDeviceId,
        rate_limiter: Option<&mut VirtioBlockRateLimiter>,
        runtime: &SharedBlockAsyncRuntime,
        generation: BlockAsyncDriveGeneration,
    ) -> Result<VirtioBlockQueueDispatch, VirtioBlockQueueDispatchError> {
        let now = Instant::now();
        let mut dispatch = VirtioBlockQueueDispatch::default();
        runtime
            .service_available_for(generation, memory)
            .map_err(|source| VirtioBlockQueueDispatchError::AsyncRuntime {
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?;
        self.publish_async_completions(memory, runtime, generation, &mut dispatch)?;
        if runtime.take_pressure(generation).map_err(|source| {
            VirtioBlockQueueDispatchError::AsyncRuntime {
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            }
        })? {
            dispatch.record_io_engine_throttled_event();
            return Ok(dispatch);
        }

        let capacity_sectors = backing.len() >> VIRTIO_BLOCK_SECTOR_SHIFT;
        let mut rate_limiter = rate_limiter;
        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioBlockQueueDispatchError::AvailableRing {
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioBlockQueueDispatchError::EmptyDescriptorChain {
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;
            let parsed = VirtioBlockRequest::parse(memory, &chain, capacity_sectors);
            if let Ok(request) = &parsed {
                let operation = match request.async_operation() {
                    Ok(operation) => operation,
                    Err(source) => {
                        let execution = request.finish_async_operation_error(memory, source);
                        self.publish_completion(
                            memory,
                            execution.completion(),
                            VirtioBlockQueueDispatchOutcome::from_request_execution(
                                request, &execution,
                            ),
                            &mut dispatch,
                        )?;
                        continue;
                    }
                };
                if let Some(operation) = operation {
                    match runtime.preflight_operation(generation, operation) {
                        Ok(()) => {}
                        Err(BlockAsyncRuntimeError::Admission(
                            BlockAsyncAdmissionError::DriveFull,
                        )) => {
                            self.available
                                .undo_pop_descriptor_chain()
                                .map_err(|source| VirtioBlockQueueDispatchError::AvailableRing {
                                    completed_dispatch: Box::new(dispatch.clone()),
                                    source,
                                })?;
                            let _ = runtime.take_pressure(generation);
                            dispatch.record_io_engine_throttled_event();
                            break;
                        }
                        Err(source) => {
                            return Err(VirtioBlockQueueDispatchError::AsyncRuntime {
                                completed_dispatch: Box::new(dispatch),
                                source,
                            });
                        }
                    }
                    if let Some(limiter) = rate_limiter.as_deref_mut() {
                        match limiter.reduce_request_at(request, now) {
                            VirtioBlockRateLimiterReduction::Allowed => {}
                            VirtioBlockRateLimiterReduction::Throttled { retry_after } => {
                                self.available
                                    .undo_pop_descriptor_chain()
                                    .map_err(|source| {
                                        VirtioBlockQueueDispatchError::AvailableRing {
                                            completed_dispatch: Box::new(dispatch.clone()),
                                            source,
                                        }
                                    })?;
                                dispatch.record_rate_limited_request(retry_after);
                                break;
                            }
                        }
                    }
                    runtime
                        .admit_preflighted(generation, operation)
                        .map_err(|source| VirtioBlockQueueDispatchError::AsyncRuntime {
                            completed_dispatch: Box::new(dispatch.clone()),
                            source,
                        })?;
                    runtime
                        .service_available_for(generation, memory)
                        .map_err(|source| VirtioBlockQueueDispatchError::AsyncRuntime {
                            completed_dispatch: Box::new(dispatch.clone()),
                            source,
                        })?;
                    self.publish_async_completions(memory, runtime, generation, &mut dispatch)?;
                    if runtime.take_pressure(generation).map_err(|source| {
                        VirtioBlockQueueDispatchError::AsyncRuntime {
                            completed_dispatch: Box::new(dispatch.clone()),
                            source,
                        }
                    })? {
                        dispatch.record_io_engine_throttled_event();
                        break;
                    }
                    continue;
                }
            }

            let (completion, outcome) = match parsed {
                Ok(request) => {
                    let execution = request.execute(memory, backing, device_id);
                    (
                        execution.completion(),
                        VirtioBlockQueueDispatchOutcome::from_request_execution(
                            &request, &execution,
                        ),
                    )
                }
                Err(source) => (
                    VirtioBlockRequestCompletion::new(descriptor_head, 0),
                    VirtioBlockQueueDispatchOutcome::ParseError(source),
                ),
            };
            self.publish_completion(memory, completion, outcome, &mut dispatch)?;
        }
        Ok(dispatch)
    }

    fn publish_async_completions(
        &mut self,
        memory: &mut GuestMemory,
        runtime: &SharedBlockAsyncRuntime,
        generation: BlockAsyncDriveGeneration,
        dispatch: &mut VirtioBlockQueueDispatch,
    ) -> Result<(), VirtioBlockQueueDispatchError> {
        while let Some(completion) = runtime.pop_completion(generation).map_err(|source| {
            VirtioBlockQueueDispatchError::AsyncRuntime {
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            }
        })? {
            let identity = completion.identity();
            if identity.queue_index() != 0 {
                return Err(VirtioBlockQueueDispatchError::AsyncRuntime {
                    completed_dispatch: Box::new(dispatch.clone()),
                    source: BlockAsyncRuntimeError::ExecutorInvariant,
                });
            }
            let request_type = match completion.kind() {
                BlockAsyncOperationKind::Read => VirtioBlockRequestType::In,
                BlockAsyncOperationKind::Write => VirtioBlockRequestType::Out,
                BlockAsyncOperationKind::Flush => VirtioBlockRequestType::Flush,
            };
            let latency_sample = matches!(
                completion.kind(),
                BlockAsyncOperationKind::Read | BlockAsyncOperationKind::Write
            )
            .then(|| {
                VirtioBlockRequestLatencySample::new(request_type, completion.total_latency_us())
            });
            let (status_code, bytes_written_to_guest, outcome) = match completion.status() {
                BlockAsyncOperationStatus::Success => {
                    let data_len = u32::try_from(completion.bytes_transferred()).map_err(|_| {
                        VirtioBlockQueueDispatchError::AsyncRuntime {
                            completed_dispatch: Box::new(dispatch.clone()),
                            source: BlockAsyncRuntimeError::ExecutorInvariant,
                        }
                    })?;
                    let bytes_written_to_guest =
                        if completion.kind() == BlockAsyncOperationKind::Read {
                            data_len
                        } else {
                            0
                        };
                    (
                        VIRTIO_BLOCK_STATUS_OK,
                        bytes_written_to_guest,
                        VirtioBlockQueueDispatchOutcome::Ok {
                            request_type,
                            data_len,
                            latency_sample,
                        },
                    )
                }
                BlockAsyncOperationStatus::Failed(_) => (
                    VIRTIO_BLOCK_STATUS_IOERR,
                    0,
                    VirtioBlockQueueDispatchOutcome::IoError { latency_sample },
                ),
            };
            let (status_code, bytes_written_to_guest, outcome) = if bytes_written_to_guest
                .checked_add(VIRTIO_BLOCK_STATUS_SIZE)
                .is_some()
            {
                (status_code, bytes_written_to_guest, outcome)
            } else {
                (
                    VIRTIO_BLOCK_STATUS_IOERR,
                    0,
                    VirtioBlockQueueDispatchOutcome::IoError { latency_sample },
                )
            };
            let status = VirtioBlockStatusDescriptor {
                index: u16::MAX,
                address: identity.status_address(),
                len: VIRTIO_BLOCK_STATUS_SIZE,
            };
            let completion = match write_request_status(memory, status, status_code) {
                Ok(()) => VirtioBlockRequestCompletion::new(
                    identity.descriptor_head(),
                    bytes_written_to_guest + VIRTIO_BLOCK_STATUS_SIZE,
                ),
                Err(_) => {
                    let outcome =
                        VirtioBlockQueueDispatchOutcome::StatusWriteFailed { latency_sample };
                    self.publish_completion(
                        memory,
                        VirtioBlockRequestCompletion::new(identity.descriptor_head(), 0),
                        outcome,
                        dispatch,
                    )?;
                    continue;
                }
            };
            self.publish_completion(memory, completion, outcome, dispatch)?;
        }
        Ok(())
    }

    fn publish_completion(
        &mut self,
        memory: &mut GuestMemory,
        completion: VirtioBlockRequestCompletion,
        outcome: VirtioBlockQueueDispatchOutcome,
        dispatch: &mut VirtioBlockQueueDispatch,
    ) -> Result<(), VirtioBlockQueueDispatchError> {
        let notification_suppression = self.notification_suppression(memory).map_err(|source| {
            VirtioBlockQueueDispatchError::AvailableRing {
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            }
        })?;
        let publication = self
            .used
            .publish_used_element_with_notification(
                memory,
                completion.descriptor_head(),
                completion.bytes_written_to_guest(),
                notification_suppression,
            )
            .map_err(|source| VirtioBlockQueueDispatchError::UsedRing {
                completed_dispatch: Box::new(dispatch.clone()),
                descriptor_head: completion.descriptor_head(),
                bytes_written_to_guest: completion.bytes_written_to_guest(),
                source,
            })?;
        dispatch.record(outcome, publication);
        Ok(())
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtioBlockQueueDispatch {
    processed_requests: usize,
    successful_requests: usize,
    read_count: usize,
    write_count: usize,
    flush_count: usize,
    read_bytes: u64,
    write_bytes: u64,
    read_latency_aggregate: Option<VirtioBlockLatencyAggregate>,
    write_latency_aggregate: Option<VirtioBlockLatencyAggregate>,
    parse_failures: usize,
    io_errors: usize,
    unsupported_requests: usize,
    status_write_failures: usize,
    rate_limiter_throttled_requests: usize,
    io_engine_throttled_events: usize,
    rate_limiter_retry_after: Option<Duration>,
    first_parse_failure: Option<VirtioBlockRequestError>,
    needs_queue_interrupt: bool,
}

impl VirtioBlockQueueDispatch {
    pub const fn processed_requests(&self) -> usize {
        self.processed_requests
    }

    pub const fn successful_requests(&self) -> usize {
        self.successful_requests
    }

    pub const fn read_count(&self) -> usize {
        self.read_count
    }

    pub const fn write_count(&self) -> usize {
        self.write_count
    }

    pub const fn flush_count(&self) -> usize {
        self.flush_count
    }

    pub const fn read_bytes(&self) -> u64 {
        self.read_bytes
    }

    pub const fn write_bytes(&self) -> u64 {
        self.write_bytes
    }

    pub const fn read_latency_aggregate(&self) -> Option<VirtioBlockLatencyAggregate> {
        self.read_latency_aggregate
    }

    pub const fn write_latency_aggregate(&self) -> Option<VirtioBlockLatencyAggregate> {
        self.write_latency_aggregate
    }

    pub const fn parse_failures(&self) -> usize {
        self.parse_failures
    }

    pub const fn first_parse_failure(&self) -> Option<&VirtioBlockRequestError> {
        self.first_parse_failure.as_ref()
    }

    pub const fn io_errors(&self) -> usize {
        self.io_errors
    }

    pub const fn unsupported_requests(&self) -> usize {
        self.unsupported_requests
    }

    pub const fn status_write_failures(&self) -> usize {
        self.status_write_failures
    }

    pub const fn rate_limiter_throttled_requests(&self) -> usize {
        self.rate_limiter_throttled_requests
    }

    pub const fn io_engine_throttled_events(&self) -> usize {
        self.io_engine_throttled_events
    }

    pub const fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.needs_queue_interrupt
    }

    fn record(
        &mut self,
        outcome: VirtioBlockQueueDispatchOutcome,
        publication: VirtqueueUsedRingPublication,
    ) {
        self.processed_requests += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        if let Some(latency_sample) = outcome.latency_sample() {
            self.record_latency_sample(latency_sample);
        }
        match outcome {
            VirtioBlockQueueDispatchOutcome::Ok {
                request_type,
                data_len,
                ..
            } => {
                self.successful_requests += 1;
                match request_type {
                    VirtioBlockRequestType::In => {
                        self.read_count += 1;
                        self.read_bytes = self.read_bytes.saturating_add(u64::from(data_len));
                    }
                    VirtioBlockRequestType::Out => {
                        self.write_count += 1;
                        self.write_bytes = self.write_bytes.saturating_add(u64::from(data_len));
                    }
                    VirtioBlockRequestType::Flush => {
                        self.flush_count += 1;
                    }
                    VirtioBlockRequestType::GetDeviceId
                    | VirtioBlockRequestType::Unsupported(_) => {}
                }
            }
            VirtioBlockQueueDispatchOutcome::ParseError(source) => {
                self.parse_failures += 1;
                if self.first_parse_failure.is_none() {
                    self.first_parse_failure = Some(source);
                }
            }
            VirtioBlockQueueDispatchOutcome::IoError { .. } => {
                self.io_errors += 1;
            }
            VirtioBlockQueueDispatchOutcome::Unsupported => {
                self.unsupported_requests += 1;
            }
            VirtioBlockQueueDispatchOutcome::StatusWriteFailed { .. } => {
                self.status_write_failures += 1;
            }
        }
    }

    fn record_rate_limited_request(&mut self, retry_after: Duration) {
        self.rate_limiter_throttled_requests += 1;
        self.rate_limiter_retry_after = Some(match self.rate_limiter_retry_after {
            Some(existing) => existing.min(retry_after),
            None => retry_after,
        });
    }

    fn record_io_engine_throttled_event(&mut self) {
        self.io_engine_throttled_events = self.io_engine_throttled_events.saturating_add(1);
    }

    fn record_latency_sample(&mut self, sample: VirtioBlockRequestLatencySample) {
        match sample.request_type() {
            VirtioBlockRequestType::In => {
                self.read_latency_aggregate = Some(
                    self.read_latency_aggregate
                        .unwrap_or_default()
                        .merged_with(sample.aggregate()),
                );
            }
            VirtioBlockRequestType::Out => {
                self.write_latency_aggregate = Some(
                    self.write_latency_aggregate
                        .unwrap_or_default()
                        .merged_with(sample.aggregate()),
                );
            }
            VirtioBlockRequestType::Flush
            | VirtioBlockRequestType::GetDeviceId
            | VirtioBlockRequestType::Unsupported(_) => {}
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VirtioBlockQueueDispatchOutcome {
    Ok {
        request_type: VirtioBlockRequestType,
        data_len: u32,
        latency_sample: Option<VirtioBlockRequestLatencySample>,
    },
    ParseError(VirtioBlockRequestError),
    IoError {
        latency_sample: Option<VirtioBlockRequestLatencySample>,
    },
    Unsupported,
    StatusWriteFailed {
        latency_sample: Option<VirtioBlockRequestLatencySample>,
    },
}

impl VirtioBlockQueueDispatchOutcome {
    const fn from_request_execution(
        request: &VirtioBlockRequest,
        execution: &VirtioBlockRequestExecution,
    ) -> Self {
        match execution.outcome() {
            VirtioBlockRequestExecutionOutcome::Ok => Self::Ok {
                request_type: request.request_type(),
                data_len: match request.data() {
                    Some(data) => data.len(),
                    None => 0,
                },
                latency_sample: execution.latency_sample(),
            },
            VirtioBlockRequestExecutionOutcome::IoError { .. } => Self::IoError {
                latency_sample: execution.latency_sample(),
            },
            VirtioBlockRequestExecutionOutcome::Unsupported { .. } => Self::Unsupported,
            VirtioBlockRequestExecutionOutcome::StatusWriteFailed { .. } => {
                Self::StatusWriteFailed {
                    latency_sample: execution.latency_sample(),
                }
            }
        }
    }

    const fn latency_sample(&self) -> Option<VirtioBlockRequestLatencySample> {
        match self {
            Self::Ok { latency_sample, .. }
            | Self::IoError { latency_sample }
            | Self::StatusWriteFailed { latency_sample } => *latency_sample,
            Self::ParseError(_) | Self::Unsupported => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioBlockQueueDispatchError {
    AvailableRing {
        completed_dispatch: Box<VirtioBlockQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain {
        completed_dispatch: Box<VirtioBlockQueueDispatch>,
    },
    UsedRing {
        completed_dispatch: Box<VirtioBlockQueueDispatch>,
        descriptor_head: u16,
        bytes_written_to_guest: u32,
        source: VirtqueueUsedRingError,
    },
    AsyncRuntime {
        completed_dispatch: Box<VirtioBlockQueueDispatch>,
        source: BlockAsyncRuntimeError,
    },
}

impl fmt::Display for VirtioBlockQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AvailableRing { source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-block available descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain { .. } => {
                f.write_str("virtio-block queue produced an empty descriptor chain")
            }
            Self::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to publish virtio-block used descriptor head {descriptor_head} with length {bytes_written_to_guest}: {source}"
                )
            }
            Self::AsyncRuntime { .. } => f.write_str("asynchronous virtio-block runtime failed"),
        }
    }
}

impl std::error::Error for VirtioBlockQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::EmptyDescriptorChain { .. } => None,
            Self::AsyncRuntime { source, .. } => Some(source),
        }
    }
}

impl VirtioBlockQueueDispatchError {
    pub const fn completed_dispatch(&self) -> &VirtioBlockQueueDispatch {
        match self {
            Self::AvailableRing {
                completed_dispatch, ..
            }
            | Self::EmptyDescriptorChain {
                completed_dispatch, ..
            }
            | Self::UsedRing {
                completed_dispatch, ..
            }
            | Self::AsyncRuntime {
                completed_dispatch, ..
            } => completed_dispatch,
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

fn normalize_completion_status(
    status_code: u8,
    bytes_written_to_guest: u32,
    outcome: VirtioBlockRequestExecutionOutcome,
) -> (u8, u32, VirtioBlockRequestExecutionOutcome) {
    match bytes_written_to_guest.checked_add(VIRTIO_BLOCK_STATUS_SIZE) {
        Some(_) => (status_code, bytes_written_to_guest, outcome),
        None => (
            VIRTIO_BLOCK_STATUS_IOERR,
            0,
            VirtioBlockRequestExecutionOutcome::IoError {
                error: VirtioBlockRequestExecutionError::CompletionLengthOverflow {
                    bytes_written_to_guest,
                },
            },
        ),
    }
}

fn request_data_buffer(len: u32) -> Result<Vec<u8>, VirtioBlockRequestExecutionError> {
    let len_usize = usize::try_from(len)
        .map_err(|_| VirtioBlockRequestExecutionError::DataLengthTooLarge { len })?;
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(len_usize).map_err(|source| {
        VirtioBlockRequestExecutionError::BufferAllocationFailed { len, source }
    })?;
    buffer.resize(len_usize, 0);
    Ok(buffer)
}

fn validate_guest_data_write(
    memory: &GuestMemory,
    request_type: VirtioBlockRequestType,
    data: VirtioBlockDataDescriptor,
) -> Result<(), VirtioBlockRequestExecutionError> {
    if data.is_empty() {
        return Ok(());
    }

    let size = u64::from(data.len());
    let range = GuestMemoryRange::new(data.address(), size).map_err(|_| {
        VirtioBlockRequestExecutionError::GuestMemoryWrite {
            request_type,
            address: data.address(),
            len: data.len(),
            source: GuestMemoryAccessError::AddressOverflow {
                start: data.address(),
                size,
            },
        }
    })?;
    memory.validate_mapped_range(range).map_err(|source| {
        VirtioBlockRequestExecutionError::GuestMemoryWrite {
            request_type,
            address: data.address(),
            len: data.len(),
            source,
        }
    })
}

fn write_request_status(
    memory: &mut GuestMemory,
    status: VirtioBlockStatusDescriptor,
    status_code: u8,
) -> Result<(), GuestMemoryAccessError> {
    memory.write_slice(&[status_code], status.address())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VirtioBlockRequestHeader {
    request_type: u32,
    sector: u64,
}

fn descriptor_at(
    chain: &VirtqueueDescriptorChain,
    index: usize,
    expected: usize,
) -> Result<&VirtqueueDescriptor, VirtioBlockRequestError> {
    chain
        .descriptors()
        .get(index)
        .ok_or(VirtioBlockRequestError::DescriptorChainTooShort {
            expected,
            actual: chain.len(),
        })
}

fn validate_header_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<(), VirtioBlockRequestError> {
    if descriptor.is_write_only() {
        return Err(VirtioBlockRequestError::HeaderDescriptorWriteOnly {
            index: descriptor.index(),
        });
    }

    if descriptor.len() < VIRTIO_BLOCK_REQUEST_HEADER_SIZE {
        return Err(VirtioBlockRequestError::HeaderDescriptorTooSmall {
            index: descriptor.index(),
            len: descriptor.len(),
            min: VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
        });
    }

    Ok(())
}

fn read_virtio_block_request_header(
    memory: &GuestMemory,
    descriptor: &VirtqueueDescriptor,
) -> Result<VirtioBlockRequestHeader, VirtioBlockRequestError> {
    let mut bytes = [0; VIRTIO_BLOCK_REQUEST_HEADER_SIZE as usize];
    memory
        .read_slice(&mut bytes, descriptor.address())
        .map_err(|source| VirtioBlockRequestError::ReadHeader {
            address: descriptor.address(),
            source,
        })?;

    Ok(VirtioBlockRequestHeader {
        request_type: u32::from_le_bytes(header_field(&bytes, 0)?),
        sector: u64::from_le_bytes(header_field(&bytes, 8)?),
    })
}

fn header_field<const N: usize>(
    bytes: &[u8; VIRTIO_BLOCK_REQUEST_HEADER_SIZE as usize],
    offset: usize,
) -> Result<[u8; N], VirtioBlockRequestError> {
    let Some(end) = offset.checked_add(N) else {
        return Err(VirtioBlockRequestError::InvalidHeaderLayout);
    };
    let Some(source) = bytes.get(offset..end) else {
        return Err(VirtioBlockRequestError::InvalidHeaderLayout);
    };
    let mut field = [0; N];
    field.copy_from_slice(source);
    Ok(field)
}

fn split_virtio_block_request_descriptors(
    chain: &VirtqueueDescriptorChain,
    request_type: VirtioBlockRequestType,
) -> Result<(Option<VirtioBlockDataDescriptor>, &VirtqueueDescriptor), VirtioBlockRequestError> {
    let second = descriptor_at(chain, 1, 2)?;
    if request_type == VirtioBlockRequestType::Flush && chain.len() == 2 {
        return Ok((None, second));
    }

    let data = data_descriptor(second);
    let status = descriptor_at(chain, 2, 3)?;
    Ok((Some(data), status))
}

fn data_descriptor(descriptor: &VirtqueueDescriptor) -> VirtioBlockDataDescriptor {
    VirtioBlockDataDescriptor {
        index: descriptor.index(),
        address: descriptor.address(),
        len: descriptor.len(),
        is_write_only: descriptor.is_write_only(),
    }
}

fn validate_data_descriptor_direction(
    request_type: VirtioBlockRequestType,
    descriptor: VirtioBlockDataDescriptor,
) -> Result<(), VirtioBlockRequestError> {
    match request_type {
        VirtioBlockRequestType::Out if descriptor.is_write_only() => {
            Err(VirtioBlockRequestError::DataDescriptorWriteOnly {
                request_type,
                index: descriptor.index(),
            })
        }
        VirtioBlockRequestType::In | VirtioBlockRequestType::GetDeviceId
            if !descriptor.is_write_only() =>
        {
            Err(VirtioBlockRequestError::DataDescriptorReadOnly {
                request_type,
                index: descriptor.index(),
            })
        }
        VirtioBlockRequestType::In
        | VirtioBlockRequestType::Out
        | VirtioBlockRequestType::Flush
        | VirtioBlockRequestType::GetDeviceId
        | VirtioBlockRequestType::Unsupported(_) => Ok(()),
    }
}

fn validate_data_descriptor_length(
    request_type: VirtioBlockRequestType,
    descriptor: VirtioBlockDataDescriptor,
    sector: u64,
    capacity_sectors: u64,
) -> Result<(), VirtioBlockRequestError> {
    match request_type {
        VirtioBlockRequestType::In | VirtioBlockRequestType::Out => {
            if !descriptor
                .len()
                .is_multiple_of(VIRTIO_BLOCK_SECTOR_SIZE as u32)
            {
                return Err(VirtioBlockRequestError::InvalidDataLength {
                    request_type,
                    len: descriptor.len(),
                });
            }

            let sectors_len = u64::from(descriptor.len()) >> VIRTIO_BLOCK_SECTOR_SHIFT;
            let end = sector.checked_add(sectors_len).ok_or(
                VirtioBlockRequestError::SectorRangeOverflow {
                    sector,
                    sectors_len,
                },
            )?;
            if end > capacity_sectors {
                return Err(VirtioBlockRequestError::SectorRangeOutOfBounds {
                    sector,
                    sectors_len,
                    capacity_sectors,
                });
            }

            Ok(())
        }
        VirtioBlockRequestType::GetDeviceId if descriptor.len() < VIRTIO_BLOCK_ID_BYTES => {
            Err(VirtioBlockRequestError::InvalidDataLength {
                request_type,
                len: descriptor.len(),
            })
        }
        VirtioBlockRequestType::Flush
        | VirtioBlockRequestType::GetDeviceId
        | VirtioBlockRequestType::Unsupported(_) => Ok(()),
    }
}

fn validate_status_descriptor(
    descriptor: &VirtqueueDescriptor,
) -> Result<VirtioBlockStatusDescriptor, VirtioBlockRequestError> {
    if !descriptor.is_write_only() {
        return Err(VirtioBlockRequestError::StatusDescriptorReadOnly {
            index: descriptor.index(),
        });
    }

    if descriptor.len() < VIRTIO_BLOCK_STATUS_SIZE {
        return Err(VirtioBlockRequestError::StatusDescriptorTooSmall {
            index: descriptor.index(),
            len: descriptor.len(),
            min: VIRTIO_BLOCK_STATUS_SIZE,
        });
    }

    Ok(VirtioBlockStatusDescriptor {
        index: descriptor.index(),
        address: descriptor.address(),
        len: descriptor.len(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VirtioBlockRequestError {
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
    DataDescriptorWriteOnly {
        request_type: VirtioBlockRequestType,
        index: u16,
    },
    DataDescriptorReadOnly {
        request_type: VirtioBlockRequestType,
        index: u16,
    },
    InvalidDataLength {
        request_type: VirtioBlockRequestType,
        len: u32,
    },
    SectorRangeOverflow {
        sector: u64,
        sectors_len: u64,
    },
    SectorRangeOutOfBounds {
        sector: u64,
        sectors_len: u64,
        capacity_sectors: u64,
    },
    StatusDescriptorReadOnly {
        index: u16,
    },
    StatusDescriptorTooSmall {
        index: u16,
        len: u32,
        min: u32,
    },
}

impl fmt::Display for VirtioBlockRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DescriptorChainTooShort { expected, actual } => {
                write!(
                    f,
                    "virtio-block request descriptor chain has {actual} descriptors; expected at least {expected}"
                )
            }
            Self::HeaderDescriptorWriteOnly { index } => {
                write!(
                    f,
                    "virtio-block request header descriptor {index} is write-only"
                )
            }
            Self::HeaderDescriptorTooSmall { index, len, min } => {
                write!(
                    f,
                    "virtio-block request header descriptor {index} has length {len}; expected at least {min}"
                )
            }
            Self::InvalidHeaderLayout => {
                f.write_str("virtio-block request header layout is invalid")
            }
            Self::ReadHeader { address, source } => {
                write!(
                    f,
                    "failed to read virtio-block request header at {address}: {source}"
                )
            }
            Self::DataDescriptorWriteOnly {
                request_type,
                index,
            } => {
                write!(
                    f,
                    "virtio-block {request_type} request data descriptor {index} is write-only"
                )
            }
            Self::DataDescriptorReadOnly {
                request_type,
                index,
            } => {
                write!(
                    f,
                    "virtio-block {request_type} request data descriptor {index} is read-only"
                )
            }
            Self::InvalidDataLength { request_type, len } => {
                write!(
                    f,
                    "virtio-block {request_type} request data length {len} is invalid"
                )
            }
            Self::SectorRangeOverflow {
                sector,
                sectors_len,
            } => {
                write!(
                    f,
                    "virtio-block request sector range overflows: sector={sector}, sectors_len={sectors_len}"
                )
            }
            Self::SectorRangeOutOfBounds {
                sector,
                sectors_len,
                capacity_sectors,
            } => {
                write!(
                    f,
                    "virtio-block request sector range exceeds capacity: sector={sector}, sectors_len={sectors_len}, capacity_sectors={capacity_sectors}"
                )
            }
            Self::StatusDescriptorReadOnly { index } => {
                write!(
                    f,
                    "virtio-block request status descriptor {index} is read-only"
                )
            }
            Self::StatusDescriptorTooSmall { index, len, min } => {
                write!(
                    f,
                    "virtio-block request status descriptor {index} has length {len}; expected at least {min}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBlockRequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadHeader { source, .. } => Some(source),
            Self::DescriptorChainTooShort { .. }
            | Self::HeaderDescriptorWriteOnly { .. }
            | Self::HeaderDescriptorTooSmall { .. }
            | Self::InvalidHeaderLayout
            | Self::DataDescriptorWriteOnly { .. }
            | Self::DataDescriptorReadOnly { .. }
            | Self::InvalidDataLength { .. }
            | Self::SectorRangeOverflow { .. }
            | Self::SectorRangeOutOfBounds { .. }
            | Self::StatusDescriptorReadOnly { .. }
            | Self::StatusDescriptorTooSmall { .. } => None,
        }
    }
}

pub struct BlockFileBacking {
    file: File,
    len: u64,
    is_read_only: bool,
    origin: BlockFileBackingOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockFileBackingOrigin {
    Path,
    SuppliedFile,
}

impl fmt::Debug for BlockFileBacking {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BlockFileBacking")
            .field("file", &"<owned>")
            .field("len", &self.len)
            .field("is_read_only", &self.is_read_only)
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct BlockFileBackingIdentity {
    device: u64,
    inode: u64,
    len: u64,
    mode: u32,
    modified_seconds: i64,
    modified_nanos: u32,
    changed_seconds: i64,
    changed_nanos: u32,
}

impl BlockFileBackingIdentity {
    pub fn new(file: [u64; 3], mode: u32, modified: [i64; 2], changed: [i64; 2]) -> Option<Self> {
        let Ok(modified_nanos) = u32::try_from(modified[1]) else {
            return None;
        };
        let Ok(changed_nanos) = u32::try_from(changed[1]) else {
            return None;
        };
        if modified_nanos >= 1_000_000_000 || changed_nanos >= 1_000_000_000 {
            return None;
        }

        Some(Self {
            device: file[0],
            inode: file[1],
            len: file[2],
            mode,
            modified_seconds: modified[0],
            modified_nanos,
            changed_seconds: changed[0],
            changed_nanos,
        })
    }

    pub const fn device(self) -> u64 {
        self.device
    }

    pub const fn inode(self) -> u64 {
        self.inode
    }

    pub const fn len(self) -> u64 {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub const fn mode(self) -> u32 {
        self.mode
    }

    pub const fn modified_seconds(self) -> i64 {
        self.modified_seconds
    }

    pub const fn modified_nanos(self) -> u32 {
        self.modified_nanos
    }

    pub const fn changed_seconds(self) -> i64 {
        self.changed_seconds
    }

    pub const fn changed_nanos(self) -> u32 {
        self.changed_nanos
    }
}

impl fmt::Debug for BlockFileBackingIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BlockFileBackingIdentity")
            .field("identity", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotBlockFileBackingError {
    UnsupportedPlatform,
    Open,
    ReadMetadata,
    NonRegularFile,
    InvalidMetadata,
}

impl fmt::Display for SnapshotBlockFileBackingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedPlatform => {
                f.write_str("snapshot block backing is unsupported on this platform")
            }
            Self::Open => f.write_str("failed to open snapshot block backing"),
            Self::ReadMetadata => f.write_str("failed to read snapshot block backing metadata"),
            Self::NonRegularFile => f.write_str("snapshot block backing is not a regular file"),
            Self::InvalidMetadata => f.write_str("snapshot block backing metadata is invalid"),
        }
    }
}

impl std::error::Error for SnapshotBlockFileBackingError {}

impl BlockFileBacking {
    pub fn open(config: &DriveConfig) -> Result<Self, BlockFileBackingError> {
        let DriveBackendConfig::File {
            path_on_host,
            is_read_only,
            ..
        } = config.backend()
        else {
            return Err(BlockFileBackingError::UnsupportedBackend);
        };
        let file = open_block_file(path_on_host, *is_read_only)?;
        Self::from_file_with_origin(file, *is_read_only, BlockFileBackingOrigin::Path)
    }

    /// Adopts an already-opened block backing without resolving its configured path.
    pub fn from_file(file: File, is_read_only: bool) -> Result<Self, BlockFileBackingError> {
        Self::from_file_with_origin(file, is_read_only, BlockFileBackingOrigin::SuppliedFile)
    }

    fn from_file_with_origin(
        file: File,
        is_read_only: bool,
        origin: BlockFileBackingOrigin,
    ) -> Result<Self, BlockFileBackingError> {
        let metadata = file
            .metadata()
            .map_err(|source| BlockFileBackingError::ReadMetadata { source })?;

        if !metadata.file_type().is_file() {
            return Err(BlockFileBackingError::NonRegularFile);
        }

        Ok(Self {
            file,
            len: metadata.len(),
            is_read_only,
            origin,
        })
    }

    pub fn open_snapshot_read_only(
        path: &Path,
    ) -> Result<(Self, BlockFileBackingIdentity), SnapshotBlockFileBackingError> {
        #[cfg(unix)]
        {
            let mut options = OpenOptions::new();
            options
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC);
            let file = options
                .open(path)
                .map_err(|_| SnapshotBlockFileBackingError::Open)?;
            let metadata = file
                .metadata()
                .map_err(|_| SnapshotBlockFileBackingError::ReadMetadata)?;
            if !metadata.file_type().is_file() {
                return Err(SnapshotBlockFileBackingError::NonRegularFile);
            }
            let identity = snapshot_block_file_identity(&metadata)?;
            let backing = Self {
                file,
                len: metadata.len(),
                is_read_only: true,
                origin: BlockFileBackingOrigin::Path,
            };
            Ok((backing, identity))
        }

        #[cfg(not(unix))]
        {
            let _ = path;
            Err(SnapshotBlockFileBackingError::UnsupportedPlatform)
        }
    }

    /// Adopts an already-opened exact read-only snapshot backing.
    pub fn from_snapshot_read_only_file(
        file: File,
    ) -> Result<(Self, BlockFileBackingIdentity), SnapshotBlockFileBackingError> {
        #[cfg(unix)]
        {
            // SAFETY: F_GETFL only inspects the live owned descriptor.
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY {
                return Err(SnapshotBlockFileBackingError::InvalidMetadata);
            }
            let metadata = file
                .metadata()
                .map_err(|_| SnapshotBlockFileBackingError::ReadMetadata)?;
            if !metadata.file_type().is_file() {
                return Err(SnapshotBlockFileBackingError::NonRegularFile);
            }
            let identity = snapshot_block_file_identity(&metadata)?;
            let backing = Self {
                file,
                len: metadata.len(),
                is_read_only: true,
                origin: BlockFileBackingOrigin::SuppliedFile,
            };
            Ok((backing, identity))
        }

        #[cfg(not(unix))]
        {
            let _ = file;
            Err(SnapshotBlockFileBackingError::UnsupportedPlatform)
        }
    }

    pub fn snapshot_identity(
        &self,
    ) -> Result<BlockFileBackingIdentity, SnapshotBlockFileBackingError> {
        #[cfg(unix)]
        {
            let metadata = self
                .file
                .metadata()
                .map_err(|_| SnapshotBlockFileBackingError::ReadMetadata)?;
            if !metadata.file_type().is_file() {
                return Err(SnapshotBlockFileBackingError::NonRegularFile);
            }
            snapshot_block_file_identity(&metadata)
        }

        #[cfg(not(unix))]
        {
            Err(SnapshotBlockFileBackingError::UnsupportedPlatform)
        }
    }

    pub(crate) const fn uses_supplied_file(&self) -> bool {
        matches!(self.origin, BlockFileBackingOrigin::SuppliedFile)
    }

    pub const fn len(&self) -> u64 {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn is_read_only(&self) -> bool {
        self.is_read_only
    }

    pub fn read_at(&self, offset: u64, dst: &mut [u8]) -> Result<(), BlockFileBackingError> {
        validate_block_file_access(self.len, offset, dst.len())?;
        if dst.is_empty() {
            return Ok(());
        }

        self.file
            .read_exact_at(dst, offset)
            .map_err(|source| BlockFileBackingError::ReadFile { source })
    }

    pub fn write_at(&self, offset: u64, src: &[u8]) -> Result<(), BlockFileBackingError> {
        if self.is_read_only {
            return Err(BlockFileBackingError::ReadOnlyWrite);
        }

        validate_block_file_access(self.len, offset, src.len())?;
        if src.is_empty() {
            return Ok(());
        }

        self.file
            .write_all_at(src, offset)
            .map_err(|source| BlockFileBackingError::WriteFile { source })
    }

    pub fn flush(&self) -> Result<(), BlockFileBackingError> {
        self.file
            .sync_all()
            .map_err(|source| BlockFileBackingError::FlushFile { source })
    }
}

#[cfg(unix)]
fn snapshot_block_file_identity(
    metadata: &std::fs::Metadata,
) -> Result<BlockFileBackingIdentity, SnapshotBlockFileBackingError> {
    BlockFileBackingIdentity::new(
        [metadata.dev(), metadata.ino(), metadata.len()],
        metadata.mode(),
        [metadata.mtime(), metadata.mtime_nsec()],
        [metadata.ctime(), metadata.ctime_nsec()],
    )
    .ok_or(SnapshotBlockFileBackingError::InvalidMetadata)
}

fn open_block_file(path: &Path, is_read_only: bool) -> Result<File, BlockFileBackingError> {
    let mut options = OpenOptions::new();
    options.read(true).write(!is_read_only);

    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NONBLOCK);
    }

    options
        .open(path)
        .map_err(|source| BlockFileBackingError::OpenFile { source })
}

fn validate_block_file_access(
    file_len: u64,
    offset: u64,
    len: usize,
) -> Result<(), BlockFileBackingError> {
    let len_u64 =
        u64::try_from(len).map_err(|_| BlockFileBackingError::AccessLengthTooLarge { len })?;
    let end = offset
        .checked_add(len_u64)
        .ok_or(BlockFileBackingError::AccessOverflow { offset, len })?;

    if offset > file_len || end > file_len {
        return Err(BlockFileBackingError::AccessOutOfBounds {
            offset,
            len,
            file_len,
        });
    }

    Ok(())
}

#[derive(Debug)]
pub enum BlockFileBackingError {
    UnsupportedBackend,
    OpenFile {
        source: io::Error,
    },
    ReadMetadata {
        source: io::Error,
    },
    NonRegularFile,
    AccessLengthTooLarge {
        len: usize,
    },
    AccessOverflow {
        offset: u64,
        len: usize,
    },
    AccessOutOfBounds {
        offset: u64,
        len: usize,
        file_len: u64,
    },
    ReadOnlyWrite,
    ReadFile {
        source: io::Error,
    },
    WriteFile {
        source: io::Error,
    },
    FlushFile {
        source: io::Error,
    },
}

impl fmt::Display for BlockFileBackingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBackend => {
                f.write_str("vhost-user drive does not have a local block backing file")
            }
            Self::OpenFile { source } => write!(f, "failed to open block backing file: {source}"),
            Self::ReadMetadata { source } => {
                write!(f, "failed to read block backing file metadata: {source}")
            }
            Self::NonRegularFile => {
                f.write_str("block backing path does not reference a regular file")
            }
            Self::AccessLengthTooLarge { len } => {
                write!(f, "block backing access length {len} is too large")
            }
            Self::AccessOverflow { offset, len } => write!(
                f,
                "block backing access at offset {offset} with length {len} overflows"
            ),
            Self::AccessOutOfBounds {
                offset,
                len,
                file_len,
            } => write!(
                f,
                "block backing access at offset {offset} with length {len} exceeds file length {file_len}"
            ),
            Self::ReadOnlyWrite => f.write_str("block backing file is read-only"),
            Self::ReadFile { source } => write!(f, "failed to read block backing file: {source}"),
            Self::WriteFile { source } => write!(f, "failed to write block backing file: {source}"),
            Self::FlushFile { source } => {
                write!(f, "failed to flush block backing file: {source}")
            }
        }
    }
}

impl std::error::Error for BlockFileBackingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OpenFile { source }
            | Self::ReadMetadata { source }
            | Self::ReadFile { source }
            | Self::WriteFile { source }
            | Self::FlushFile { source } => Some(source),
            Self::UnsupportedBackend
            | Self::NonRegularFile
            | Self::AccessLengthTooLarge { .. }
            | Self::AccessOverflow { .. }
            | Self::AccessOutOfBounds { .. }
            | Self::ReadOnlyWrite => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockTokenBucketState {
    config: DriveTokenBucketConfig,
    budget: u64,
    remaining_burst: u64,
    age_nanos: u64,
}

impl VirtioBlockTokenBucketState {
    pub const fn new(
        config: DriveTokenBucketConfig,
        budget: u64,
        remaining_burst: u64,
        age_nanos: u64,
    ) -> Self {
        Self {
            config,
            budget,
            remaining_burst,
            age_nanos,
        }
    }

    pub const fn config(self) -> DriveTokenBucketConfig {
        self.config
    }

    pub const fn budget(self) -> u64 {
        self.budget
    }

    pub const fn remaining_burst(self) -> u64 {
        self.remaining_burst
    }

    pub const fn age_nanos(self) -> u64 {
        self.age_nanos
    }

    const fn from_persisted(state: PersistedTokenBucketState) -> Self {
        let config = state.config();
        Self::new(
            DriveTokenBucketConfig::new(
                config.size(),
                config.one_time_burst(),
                config.refill_time(),
            ),
            state.budget(),
            state.one_time_burst(),
            state.age_nanos(),
        )
    }

    const fn into_persisted(self) -> PersistedTokenBucketState {
        PersistedTokenBucketState::new(
            self.config.token_bucket_config(),
            self.budget,
            self.remaining_burst,
            self.age_nanos,
        )
    }
}

impl fmt::Debug for VirtioBlockTokenBucketState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtioBlockTokenBucketState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockRateLimiterState {
    bandwidth: Option<VirtioBlockTokenBucketState>,
    ops: Option<VirtioBlockTokenBucketState>,
}

impl VirtioBlockRateLimiterState {
    pub const fn new(
        bandwidth: Option<VirtioBlockTokenBucketState>,
        ops: Option<VirtioBlockTokenBucketState>,
    ) -> Self {
        Self { bandwidth, ops }
    }

    pub const fn bandwidth(self) -> Option<VirtioBlockTokenBucketState> {
        self.bandwidth
    }

    pub const fn ops(self) -> Option<VirtioBlockTokenBucketState> {
        self.ops
    }

    pub const fn is_empty(self) -> bool {
        self.bandwidth.is_none() && self.ops.is_none()
    }
}

impl fmt::Debug for VirtioBlockRateLimiterState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VirtioBlockRateLimiterState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBlockRateLimiterStateError {
    UnsupportedBackend,
    MissingBandwidthBucket,
    UnexpectedBandwidthBucket,
    InvalidBandwidthBucket,
    MissingOpsBucket,
    UnexpectedOpsBucket,
    InvalidOpsBucket,
    UnexpectedRateLimiter,
}

impl fmt::Display for VirtioBlockRateLimiterStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBackend => {
                f.write_str("vhost-user block has no local rate limiter state")
            }
            Self::MissingBandwidthBucket => {
                f.write_str("persisted block bandwidth bucket is missing")
            }
            Self::UnexpectedBandwidthBucket => {
                f.write_str("persisted block bandwidth bucket is unexpected")
            }
            Self::InvalidBandwidthBucket => {
                f.write_str("persisted block bandwidth bucket is invalid")
            }
            Self::MissingOpsBucket => f.write_str("persisted block ops bucket is missing"),
            Self::UnexpectedOpsBucket => f.write_str("persisted block ops bucket is unexpected"),
            Self::InvalidOpsBucket => f.write_str("persisted block ops bucket is invalid"),
            Self::UnexpectedRateLimiter => {
                f.write_str("persisted block rate limiter does not match configuration")
            }
        }
    }
}

impl std::error::Error for VirtioBlockRateLimiterStateError {}

#[derive(Debug, Clone)]
pub struct VirtioBlockRateLimiter {
    bandwidth: Option<TokenBucket>,
    ops: Option<TokenBucket>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioBlockRateLimiterReduction {
    Allowed,
    Throttled { retry_after: Duration },
}

impl VirtioBlockRateLimiter {
    pub(crate) fn new(config: DriveRateLimiterConfig) -> Option<Self> {
        Self::new_at(config, Instant::now())
    }

    pub(crate) fn new_at(config: DriveRateLimiterConfig, now: Instant) -> Option<Self> {
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

    fn persisted_state_at(
        config: Option<DriveRateLimiterConfig>,
        limiter: Option<&Self>,
        now: Instant,
    ) -> Result<VirtioBlockRateLimiterState, VirtioBlockRateLimiterStateError> {
        if config.is_none() && limiter.is_some() {
            return Err(VirtioBlockRateLimiterStateError::UnexpectedRateLimiter);
        }

        let bandwidth_config = config.and_then(DriveRateLimiterConfig::bandwidth);
        let bandwidth = capture_block_bucket_state(
            bandwidth_config,
            limiter.and_then(|limiter| limiter.bandwidth.as_ref()),
            now,
            VirtioBlockRateLimiterStateError::MissingBandwidthBucket,
            VirtioBlockRateLimiterStateError::UnexpectedBandwidthBucket,
            VirtioBlockRateLimiterStateError::InvalidBandwidthBucket,
        )?;
        let ops_config = config.and_then(DriveRateLimiterConfig::ops);
        let ops = capture_block_bucket_state(
            ops_config,
            limiter.and_then(|limiter| limiter.ops.as_ref()),
            now,
            VirtioBlockRateLimiterStateError::MissingOpsBucket,
            VirtioBlockRateLimiterStateError::UnexpectedOpsBucket,
            VirtioBlockRateLimiterStateError::InvalidOpsBucket,
        )?;

        Ok(VirtioBlockRateLimiterState::new(bandwidth, ops))
    }

    pub(crate) fn from_persisted_state_at(
        config: Option<DriveRateLimiterConfig>,
        state: VirtioBlockRateLimiterState,
        now: Instant,
    ) -> Result<Option<Self>, VirtioBlockRateLimiterStateError> {
        let bandwidth = restore_block_bucket_state(
            config.and_then(DriveRateLimiterConfig::bandwidth),
            state.bandwidth(),
            now,
            VirtioBlockRateLimiterStateError::MissingBandwidthBucket,
            VirtioBlockRateLimiterStateError::UnexpectedBandwidthBucket,
            VirtioBlockRateLimiterStateError::InvalidBandwidthBucket,
        )?;
        let ops = restore_block_bucket_state(
            config.and_then(DriveRateLimiterConfig::ops),
            state.ops(),
            now,
            VirtioBlockRateLimiterStateError::MissingOpsBucket,
            VirtioBlockRateLimiterStateError::UnexpectedOpsBucket,
            VirtioBlockRateLimiterStateError::InvalidOpsBucket,
        )?;

        if bandwidth.is_none() && ops.is_none() {
            Ok(None)
        } else {
            Ok(Some(Self { bandwidth, ops }))
        }
    }

    fn reduce_request_at(
        &mut self,
        request: &VirtioBlockRequest,
        now: Instant,
    ) -> VirtioBlockRateLimiterReduction {
        let ops_snapshot = self.ops.as_ref().map(TokenBucket::snapshot);
        if let Some(ops) = self.ops.as_mut()
            && let Some(retry_after) = ops.reduce_with_retry_at(1, now).retry_after()
        {
            return VirtioBlockRateLimiterReduction::Throttled { retry_after };
        }

        let bandwidth_bytes = request.rate_limiter_bandwidth_bytes();
        if let Some(bandwidth) = self.bandwidth.as_mut()
            && let Some(retry_after) = bandwidth
                .reduce_allow_overconsumption_with_retry_at(bandwidth_bytes, now)
                .retry_after()
        {
            if let (Some(ops), Some(snapshot)) = (self.ops.as_mut(), ops_snapshot) {
                ops.restore(snapshot);
            }
            return VirtioBlockRateLimiterReduction::Throttled { retry_after };
        }

        VirtioBlockRateLimiterReduction::Allowed
    }

    fn apply_update(&mut self, update: DriveRateLimiterConfig) {
        update_runtime_token_bucket(&mut self.bandwidth, update.bandwidth());
        update_runtime_token_bucket(&mut self.ops, update.ops());
    }

    fn is_empty(&self) -> bool {
        self.bandwidth.is_none() && self.ops.is_none()
    }
}

fn capture_block_bucket_state(
    config: Option<DriveTokenBucketConfig>,
    bucket: Option<&TokenBucket>,
    now: Instant,
    missing: VirtioBlockRateLimiterStateError,
    unexpected: VirtioBlockRateLimiterStateError,
    invalid: VirtioBlockRateLimiterStateError,
) -> Result<Option<VirtioBlockTokenBucketState>, VirtioBlockRateLimiterStateError> {
    match (config, bucket) {
        (Some(config), Some(bucket)) if config.is_enabled() => bucket
            .persisted_state_at(config.token_bucket_config(), now)
            .map(VirtioBlockTokenBucketState::from_persisted)
            .map(Some)
            .map_err(|_| invalid),
        (Some(config), None) if config.is_enabled() => Err(missing),
        (Some(config), Some(_)) if !config.is_enabled() => Err(unexpected),
        (Some(_), None) | (None, None) => Ok(None),
        (None, Some(_)) => Err(unexpected),
        (Some(_), Some(_)) => Err(invalid),
    }
}

fn restore_block_bucket_state(
    config: Option<DriveTokenBucketConfig>,
    state: Option<VirtioBlockTokenBucketState>,
    now: Instant,
    missing: VirtioBlockRateLimiterStateError,
    unexpected: VirtioBlockRateLimiterStateError,
    invalid: VirtioBlockRateLimiterStateError,
) -> Result<Option<TokenBucket>, VirtioBlockRateLimiterStateError> {
    match (config, state) {
        (Some(config), Some(state)) if config.is_enabled() && state.config() == config => {
            TokenBucket::from_persisted_state_at(state.into_persisted(), now)
                .map(Some)
                .map_err(|_: PersistedTokenBucketStateError| invalid)
        }
        (Some(config), None) if config.is_enabled() => Err(missing),
        (Some(config), Some(_)) if !config.is_enabled() => Err(unexpected),
        (Some(_), None) | (None, None) => Ok(None),
        (None, Some(_)) => Err(unexpected),
        (Some(_), Some(_)) => Err(invalid),
    }
}

fn update_runtime_token_bucket(
    bucket: &mut Option<TokenBucket>,
    update: Option<DriveTokenBucketConfig>,
) {
    if let Some(config) = update {
        *bucket = TokenBucket::new(config.token_bucket_config());
    }
}

struct VhostUserBlockMemoryRegion {
    guest_base: u64,
    len: u64,
    userspace_base: u64,
    mmap_offset: u64,
    backing: GuestMemorySharedBacking,
}

impl fmt::Debug for VhostUserBlockMemoryRegion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("VhostUserBlockMemoryRegion")
            .field(&"<redacted>")
            .finish()
    }
}

impl VhostUserBlockMemoryRegion {
    fn preflight_guest_memory(
        memory: &GuestMemory,
    ) -> Result<(), PreparedVhostUserBlockMemoryError> {
        if memory.regions().is_empty() {
            return Err(PreparedVhostUserBlockMemoryError::EmptyMemory);
        }
        for region in memory.regions() {
            if !region
                .validate_shared_backing()
                .map_err(PreparedVhostUserBlockMemoryError::SharedBacking)?
            {
                return Err(PreparedVhostUserBlockMemoryError::AnonymousMemory);
            }
            Self::validated_geometry(region)?;
        }
        Ok(())
    }

    fn from_guest_memory(
        memory: &GuestMemory,
    ) -> Result<Vec<Self>, PreparedVhostUserBlockMemoryError> {
        if memory.regions().is_empty() {
            return Err(PreparedVhostUserBlockMemoryError::EmptyMemory);
        }
        let mut regions = Vec::new();
        regions
            .try_reserve_exact(memory.regions().len())
            .map_err(PreparedVhostUserBlockMemoryError::AllocateRegions)?;
        for region in memory.regions() {
            let backing = region
                .try_clone_shared_backing()
                .map_err(PreparedVhostUserBlockMemoryError::SharedBacking)?
                .ok_or(PreparedVhostUserBlockMemoryError::AnonymousMemory)?;
            let (guest_base, len, userspace_base) = Self::validated_geometry(region)?;
            if backing.is_empty() || backing.len() != len {
                return Err(PreparedVhostUserBlockMemoryError::InvalidRegion);
            }
            regions.push(Self {
                guest_base,
                len,
                userspace_base,
                mmap_offset: backing.offset(),
                backing,
            });
        }
        Ok(regions)
    }

    fn validated_geometry(
        region: &crate::memory::GuestMemoryRegion,
    ) -> Result<(u64, u64, u64), PreparedVhostUserBlockMemoryError> {
        let range = region.range();
        if range.size() == 0 || u64::try_from(region.host_size()).ok() != Some(range.size()) {
            return Err(PreparedVhostUserBlockMemoryError::InvalidRegion);
        }
        let userspace_base = u64::try_from(region.host_address().as_ptr().addr())
            .map_err(|_| PreparedVhostUserBlockMemoryError::InvalidRegion)?;
        Ok((range.start().raw_value(), range.size(), userspace_base))
    }

    fn protocol_region(&self) -> Result<VhostUserMemoryRegion<'_>, VhostUserError> {
        VhostUserMemoryRegion::new(
            self.guest_base,
            self.len,
            self.userspace_base,
            self.mmap_offset,
            self.backing.as_fd(),
        )
    }

    fn translate_range(&self, range: GuestMemoryRange) -> Option<u64> {
        let range_start = range.start().raw_value();
        let range_end = range.end_exclusive().raw_value();
        let region_end = self.guest_base.checked_add(self.len)?;
        if range_start < self.guest_base || range_end > region_end {
            return None;
        }
        self.userspace_base
            .checked_add(range_start.checked_sub(self.guest_base)?)
    }
}

/// Redacted failure while exporting the complete current guest-memory table.
#[derive(Debug)]
pub enum PreparedVhostUserBlockMemoryError {
    AllocateRegions(TryReserveError),
    SharedBacking(GuestMemorySharedBackingError),
    AnonymousMemory,
    EmptyMemory,
    InvalidRegion,
}

impl fmt::Display for PreparedVhostUserBlockMemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocateRegions(_) => {
                formatter.write_str("failed to allocate vhost-user guest-memory exports")
            }
            Self::SharedBacking(_) => {
                formatter.write_str("failed to validate vhost-user guest-memory backing")
            }
            Self::AnonymousMemory => {
                formatter.write_str("vhost-user block requires shared guest memory")
            }
            Self::EmptyMemory => formatter.write_str("vhost-user guest memory is empty"),
            Self::InvalidRegion => formatter.write_str("vhost-user guest-memory export is invalid"),
        }
    }
}

impl std::error::Error for PreparedVhostUserBlockMemoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateRegions(source) => Some(source),
            Self::SharedBacking(source) => Some(source),
            Self::AnonymousMemory | Self::EmptyMemory | Self::InvalidRegion => None,
        }
    }
}

#[derive(Debug)]
enum VirtioBlockBackend {
    File {
        backing: Arc<BlockFileBacking>,
        active_queue: Option<VirtioBlockQueue>,
        rate_limiter: Option<VirtioBlockRateLimiter>,
        io_engine: VirtioBlockFileIoEngine,
    },
    VhostUser(VhostUserBlockBackend),
}

#[derive(Debug)]
enum VirtioBlockFileIoEngine {
    Sync,
    Async {
        runtime: SharedBlockAsyncRuntime,
        generation: BlockAsyncDriveGeneration,
        cache_type: DriveCacheType,
        terminal: bool,
    },
}

#[derive(Debug)]
struct VhostUserBlockBackend {
    frontend: Option<VhostUserFrontend>,
    available_features: u64,
    memory_regions: Vec<VhostUserBlockMemoryRegion>,
    state: VhostUserBlockState,
}

#[derive(Debug)]
enum VhostUserBlockState {
    Prepared {
        kick: KickNotifier,
        backend_kick: BackendKickEndpoint,
        call: CallNotifier,
        backend_call: BackendCallEndpoint,
    },
    Active {
        kick: KickNotifier,
        call: CallNotifier,
    },
    Terminal {
        was_active: bool,
    },
}

#[derive(Debug)]
pub struct VirtioBlockDevice {
    backend: VirtioBlockBackend,
    device_id: VirtioBlockDeviceId,
}

impl VirtioBlockDevice {
    pub fn new(backing: BlockFileBacking, device_id: VirtioBlockDeviceId) -> Self {
        Self {
            backend: VirtioBlockBackend::File {
                backing: Arc::new(backing),
                active_queue: None,
                rate_limiter: None,
                io_engine: VirtioBlockFileIoEngine::Sync,
            },
            device_id,
        }
    }

    fn new_vhost_user(
        frontend: VhostUserFrontend,
        available_features: u64,
        memory_regions: Vec<VhostUserBlockMemoryRegion>,
        device_id: VirtioBlockDeviceId,
    ) -> Result<Self, VhostUserNotifierError> {
        let (kick, backend_kick) = create_kick_notifier()?;
        let (call, backend_call) = create_call_notifier()?;
        Ok(Self {
            backend: VirtioBlockBackend::VhostUser(VhostUserBlockBackend {
                frontend: Some(frontend),
                available_features,
                memory_regions,
                state: VhostUserBlockState::Prepared {
                    kick,
                    backend_kick,
                    call,
                    backend_call,
                },
            }),
            device_id,
        })
    }

    pub fn with_rate_limiter(mut self, rate_limiter: VirtioBlockRateLimiter) -> Self {
        if let VirtioBlockBackend::File {
            rate_limiter: configured,
            ..
        } = &mut self.backend
        {
            *configured = Some(rate_limiter);
        }
        self
    }

    pub(crate) fn from_snapshot_parts(
        backing: BlockFileBacking,
        device_id: VirtioBlockDeviceId,
        active_queue: Option<VirtioBlockQueue>,
        rate_limiter: Option<VirtioBlockRateLimiter>,
    ) -> Self {
        Self {
            backend: VirtioBlockBackend::File {
                backing: Arc::new(backing),
                active_queue,
                rate_limiter,
                io_engine: VirtioBlockFileIoEngine::Sync,
            },
            device_id,
        }
    }

    pub fn backing(&self) -> Option<&BlockFileBacking> {
        match &self.backend {
            VirtioBlockBackend::File { backing, .. } => Some(backing.as_ref()),
            VirtioBlockBackend::VhostUser(_) => None,
        }
    }

    pub const fn backend_kind(&self) -> VirtioBlockBackendKind {
        match self.backend {
            VirtioBlockBackend::File { .. } => VirtioBlockBackendKind::File,
            VirtioBlockBackend::VhostUser(_) => VirtioBlockBackendKind::VhostUser,
        }
    }

    pub fn io_engine(&self) -> Option<DriveIoEngine> {
        match &self.backend {
            VirtioBlockBackend::File { io_engine, .. } => Some(match io_engine {
                VirtioBlockFileIoEngine::Sync => DriveIoEngine::Sync,
                VirtioBlockFileIoEngine::Async { .. } => DriveIoEngine::Async,
            }),
            VirtioBlockBackend::VhostUser(_) => None,
        }
    }

    pub fn bind_async_runtime(
        &mut self,
        cache_type: DriveCacheType,
        runtime: SharedBlockAsyncRuntime,
    ) -> Result<BlockAsyncDriveGeneration, BlockAsyncRuntimeError> {
        let VirtioBlockBackend::File {
            backing, io_engine, ..
        } = &mut self.backend
        else {
            return Err(BlockAsyncRuntimeError::ExecutorInvariant);
        };
        if matches!(io_engine, VirtioBlockFileIoEngine::Async { .. }) {
            return Err(BlockAsyncRuntimeError::ExecutorInvariant);
        }
        let generation = runtime.bind_drive(Arc::clone(backing), cache_type)?;
        *io_engine = VirtioBlockFileIoEngine::Async {
            runtime,
            generation,
            cache_type,
            terminal: false,
        };
        Ok(generation)
    }

    pub fn async_binding(&self) -> Option<(SharedBlockAsyncRuntime, BlockAsyncDriveGeneration)> {
        match &self.backend {
            VirtioBlockBackend::File {
                io_engine:
                    VirtioBlockFileIoEngine::Async {
                        runtime,
                        generation,
                        terminal: false,
                        ..
                    },
                ..
            } => Some((runtime.clone(), *generation)),
            VirtioBlockBackend::File { .. } | VirtioBlockBackend::VhostUser(_) => None,
        }
    }

    /// Captures a regular-file device after any Async generation is drained.
    pub fn capture_state_at(
        &self,
        config_space: VirtioBlockConfigSpace,
        config: &DriveConfig,
        now: Instant,
    ) -> Result<VirtioBlockDeviceCaptureState, VirtioBlockDeviceCaptureError> {
        let VirtioBlockBackend::File {
            backing,
            active_queue,
            rate_limiter,
            io_engine,
        } = &self.backend
        else {
            return Err(VirtioBlockDeviceCaptureError::UnsupportedBackend);
        };
        if VirtioBlockDeviceId::from_bytes(config.drive_id().as_bytes()) != self.device_id
            || config.is_read_only() != Some(backing.is_read_only())
            || config_space != VirtioBlockConfigSpace::from_backing(backing, config.cache_type())
        {
            return Err(VirtioBlockDeviceCaptureError::ConfigurationMismatch);
        }
        let backing = backing
            .snapshot_identity()
            .map_err(|_| VirtioBlockDeviceCaptureError::BackingIdentity)?;
        let rate_limiter = VirtioBlockRateLimiter::persisted_state_at(
            config.rate_limiter(),
            rate_limiter.as_ref(),
            now,
        )
        .map_err(|_| VirtioBlockDeviceCaptureError::RateLimiter)?;
        let io_engine = match io_engine {
            VirtioBlockFileIoEngine::Sync if config.io_engine() == Some(DriveIoEngine::Sync) => {
                BlockCaptureIoEngine::Sync
            }
            VirtioBlockFileIoEngine::Async {
                runtime,
                generation,
                cache_type,
                terminal: false,
            } if config.io_engine() == Some(DriveIoEngine::Async)
                && *cache_type == config.cache_type() =>
            {
                BlockCaptureIoEngine::Async(
                    runtime
                        .capture_quiesced_generation(*generation)
                        .map_err(VirtioBlockDeviceCaptureError::Async)?,
                )
            }
            VirtioBlockFileIoEngine::Sync | VirtioBlockFileIoEngine::Async { .. } => {
                return Err(VirtioBlockDeviceCaptureError::ConfigurationMismatch);
            }
        };
        Ok(VirtioBlockDeviceCaptureState {
            config_space,
            device_id: self.device_id,
            backing,
            active_queue: active_queue.as_ref().map(VirtioBlockQueue::snapshot_state),
            rate_limiter,
            io_engine,
        })
    }

    /// Publishes only already-drained Async completions without consuming pressure.
    pub fn publish_quiesced_async_completions(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBlockQueueDispatch, VirtioBlockQueueDispatchError> {
        let VirtioBlockBackend::File {
            active_queue,
            io_engine:
                VirtioBlockFileIoEngine::Async {
                    runtime,
                    generation,
                    terminal: false,
                    ..
                },
            ..
        } = &mut self.backend
        else {
            return Err(VirtioBlockQueueDispatchError::AsyncRuntime {
                completed_dispatch: Box::default(),
                source: BlockAsyncRuntimeError::ExecutorInvariant,
            });
        };
        let mut dispatch = VirtioBlockQueueDispatch::default();
        if let Some(queue) = active_queue.as_mut() {
            queue.publish_async_completions(memory, runtime, *generation, &mut dispatch)?;
        }
        Ok(dispatch)
    }

    pub fn refresh_backing(
        &mut self,
        backing: BlockFileBacking,
    ) -> Result<(), VirtioBlockBackendOperationError> {
        match &mut self.backend {
            VirtioBlockBackend::File {
                backing: current,
                io_engine: VirtioBlockFileIoEngine::Sync,
                ..
            } => {
                *current = Arc::new(backing);
                Ok(())
            }
            VirtioBlockBackend::File {
                io_engine: VirtioBlockFileIoEngine::Async { .. },
                ..
            }
            | VirtioBlockBackend::VhostUser(_) => {
                Err(VirtioBlockBackendOperationError::UnsupportedBackend)
            }
        }
    }

    pub fn update_rate_limiter(
        &mut self,
        update: DriveRateLimiterConfig,
    ) -> Result<(), VirtioBlockBackendOperationError> {
        let VirtioBlockBackend::File { rate_limiter, .. } = &mut self.backend else {
            return Err(VirtioBlockBackendOperationError::UnsupportedBackend);
        };
        if let Some(configured) = rate_limiter.as_mut() {
            configured.apply_update(update);
            if configured.is_empty() {
                *rate_limiter = None;
            }
        } else {
            *rate_limiter = VirtioBlockRateLimiter::new(update);
        }
        Ok(())
    }

    pub fn update_file_backend_with_opened(
        &mut self,
        memory: &mut GuestMemory,
        config: &DriveConfig,
        replacement_backing: Option<BlockFileBacking>,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
        mode: DriveLiveUpdateMode,
        shared_runtime: &SharedBlockAsyncRuntime,
    ) -> Result<VirtioBlockQueueDispatch, VirtioBlockLiveUpdateError> {
        let desired_engine = config
            .io_engine()
            .ok_or(VirtioBlockLiveUpdateError::UnsupportedBackend)?;
        let current_engine = self
            .io_engine()
            .ok_or(VirtioBlockLiveUpdateError::UnsupportedBackend)?;
        let replace_file_state = replacement_backing.is_some()
            || desired_engine != current_engine
            || mode == DriveLiveUpdateMode::Replacement;
        if !replace_file_state {
            if let Some(update) = rate_limiter_update {
                self.update_rate_limiter(update)
                    .map_err(|_| VirtioBlockLiveUpdateError::UnsupportedBackend)?;
            }
            return Ok(VirtioBlockQueueDispatch::default());
        }

        let replacement_backing =
            replacement_backing.ok_or(VirtioBlockLiveUpdateError::MissingReplacementBacking)?;
        if config.is_read_only() != Some(replacement_backing.is_read_only()) {
            return Err(VirtioBlockLiveUpdateError::UnsupportedBackend);
        }
        let replacement_backing = Arc::new(replacement_backing);
        let prospective_async = if desired_engine == DriveIoEngine::Async {
            Some((
                shared_runtime.clone(),
                shared_runtime
                    .bind_drive(Arc::clone(&replacement_backing), config.cache_type())
                    .map_err(VirtioBlockLiveUpdateError::AsyncPreparation)?,
            ))
        } else {
            None
        };

        let VirtioBlockBackend::File {
            backing,
            active_queue,
            rate_limiter,
            io_engine,
        } = &mut self.backend
        else {
            if let Some((runtime, generation)) = prospective_async {
                let _ = runtime.discard_generation_without_guest_memory(generation);
            }
            return Err(VirtioBlockLiveUpdateError::UnsupportedBackend);
        };
        let old_async = match io_engine {
            VirtioBlockFileIoEngine::Async {
                runtime,
                generation,
                terminal: false,
                ..
            } => Some((runtime.clone(), *generation)),
            VirtioBlockFileIoEngine::Async { terminal: true, .. } => {
                if let Some((runtime, generation)) = prospective_async {
                    let _ = runtime.discard_generation_without_guest_memory(generation);
                }
                return Err(VirtioBlockLiveUpdateError::AsyncTerminal(
                    BlockAsyncRuntimeError::ExecutorInvariant,
                ));
            }
            VirtioBlockFileIoEngine::Sync => None,
        };

        let mut completed_dispatch = VirtioBlockQueueDispatch::default();
        if let Some((runtime, generation)) = old_async {
            if let Err(source) = runtime.quiesce_generation(generation, memory) {
                mark_async_engine_terminal(io_engine);
                cancel_prospective_async(prospective_async);
                return Err(VirtioBlockLiveUpdateError::AsyncTerminal(source));
            }
            if let Some(queue) = active_queue.as_mut()
                && let Err(source) = queue.publish_async_completions(
                    memory,
                    &runtime,
                    generation,
                    &mut completed_dispatch,
                )
            {
                mark_async_engine_terminal(io_engine);
                cancel_prospective_async(prospective_async);
                return Err(VirtioBlockLiveUpdateError::Queue(source));
            }
            if let Err(source) = runtime.unbind_quiesced(generation) {
                mark_async_engine_terminal(io_engine);
                cancel_prospective_async(prospective_async);
                return Err(VirtioBlockLiveUpdateError::AsyncTerminal(source));
            }
        }

        *backing = replacement_backing;
        *io_engine = match prospective_async {
            Some((runtime, generation)) => VirtioBlockFileIoEngine::Async {
                runtime,
                generation,
                cache_type: config.cache_type(),
                terminal: false,
            },
            None => VirtioBlockFileIoEngine::Sync,
        };
        match mode {
            DriveLiveUpdateMode::Patch => {
                if let Some(update) = rate_limiter_update {
                    if let Some(configured) = rate_limiter.as_mut() {
                        configured.apply_update(update);
                        if configured.is_empty() {
                            *rate_limiter = None;
                        }
                    } else {
                        *rate_limiter = VirtioBlockRateLimiter::new(update);
                    }
                }
            }
            DriveLiveUpdateMode::Replacement => {
                *rate_limiter = config.rate_limiter().and_then(VirtioBlockRateLimiter::new);
            }
        }
        Ok(completed_dispatch)
    }

    fn retire_async(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBlockQueueDispatch, VirtioBlockLiveUpdateError> {
        let VirtioBlockBackend::File {
            active_queue,
            io_engine,
            ..
        } = &mut self.backend
        else {
            return Ok(VirtioBlockQueueDispatch::default());
        };
        let (runtime, generation) = match io_engine {
            VirtioBlockFileIoEngine::Sync => return Ok(VirtioBlockQueueDispatch::default()),
            VirtioBlockFileIoEngine::Async { terminal: true, .. } => {
                return Err(VirtioBlockLiveUpdateError::AsyncTerminal(
                    BlockAsyncRuntimeError::ExecutorInvariant,
                ));
            }
            VirtioBlockFileIoEngine::Async {
                runtime,
                generation,
                terminal: false,
                ..
            } => (runtime.clone(), *generation),
        };

        if let Err(source) = runtime.quiesce_generation(generation, memory) {
            mark_async_engine_terminal(io_engine);
            return Err(VirtioBlockLiveUpdateError::AsyncTerminal(source));
        }
        let mut dispatch = VirtioBlockQueueDispatch::default();
        if let Some(queue) = active_queue.as_mut()
            && let Err(source) =
                queue.publish_async_completions(memory, &runtime, generation, &mut dispatch)
        {
            mark_async_engine_terminal(io_engine);
            return Err(VirtioBlockLiveUpdateError::Queue(source));
        }
        if let Err(source) = runtime.unbind_quiesced(generation) {
            mark_async_engine_terminal(io_engine);
            return Err(VirtioBlockLiveUpdateError::AsyncTerminal(source));
        }
        mark_async_engine_terminal(io_engine);
        Ok(dispatch)
    }

    pub fn device_id(&self) -> VirtioBlockDeviceId {
        self.device_id
    }

    pub fn rate_limiter(&self) -> Option<&VirtioBlockRateLimiter> {
        match &self.backend {
            VirtioBlockBackend::File { rate_limiter, .. } => rate_limiter.as_ref(),
            VirtioBlockBackend::VhostUser(_) => None,
        }
    }

    pub fn snapshot_rate_limiter_state_at(
        &self,
        config: Option<DriveRateLimiterConfig>,
        now: Instant,
    ) -> Result<VirtioBlockRateLimiterState, VirtioBlockRateLimiterStateError> {
        let VirtioBlockBackend::File { rate_limiter, .. } = &self.backend else {
            return Err(VirtioBlockRateLimiterStateError::UnsupportedBackend);
        };
        VirtioBlockRateLimiter::persisted_state_at(config, rate_limiter.as_ref(), now)
    }

    pub fn is_activated(&self) -> bool {
        match &self.backend {
            VirtioBlockBackend::File { active_queue, .. } => active_queue.is_some(),
            VirtioBlockBackend::VhostUser(backend) => matches!(
                backend.state,
                VhostUserBlockState::Active { .. }
                    | VhostUserBlockState::Terminal { was_active: true }
            ),
        }
    }

    pub fn active_queue(&self) -> Option<&VirtioBlockQueue> {
        match &self.backend {
            VirtioBlockBackend::File { active_queue, .. } => active_queue.as_ref(),
            VirtioBlockBackend::VhostUser(_) => None,
        }
    }

    pub fn active_queue_mut(&mut self) -> Option<&mut VirtioBlockQueue> {
        match &mut self.backend {
            VirtioBlockBackend::File { active_queue, .. } => active_queue.as_mut(),
            VirtioBlockBackend::VhostUser(_) => None,
        }
    }

    pub fn vhost_user_call_fd(&self) -> Option<i32> {
        match &self.backend {
            VirtioBlockBackend::VhostUser(VhostUserBlockBackend {
                state:
                    VhostUserBlockState::Prepared { call, .. }
                    | VhostUserBlockState::Active { call, .. },
                ..
            }) => Some(call.as_fd().as_raw_fd()),
            VirtioBlockBackend::File { .. } | VirtioBlockBackend::VhostUser(_) => None,
        }
    }

    fn refreshed_vhost_user_config(
        &mut self,
        cache_type: DriveCacheType,
    ) -> Result<VirtioBlockConfigSpace, VhostUserBlockConfigRefreshError> {
        let VirtioBlockBackend::VhostUser(backend) = &mut self.backend else {
            return Err(VhostUserBlockConfigRefreshError::UnsupportedBackend);
        };
        match backend.state {
            VhostUserBlockState::Prepared { .. } => {
                return Err(VhostUserBlockConfigRefreshError::Inactive);
            }
            VhostUserBlockState::Terminal { .. } => {
                return Err(VhostUserBlockConfigRefreshError::Terminal);
            }
            VhostUserBlockState::Active { .. } => {}
        }
        let Some(frontend) = backend.frontend.as_mut() else {
            return Err(VhostUserBlockConfigRefreshError::Terminal);
        };
        let config = match frontend.get_config(
            0,
            u32::try_from(VIRTIO_BLOCK_CONFIG_SIZE)
                .map_err(|_| VhostUserBlockConfigRefreshError::InvalidConfig)?,
            VhostUserConfigFlags::Writable,
        ) {
            Ok(config) => config,
            Err(source) => {
                backend.frontend = None;
                backend.state = VhostUserBlockState::Terminal { was_active: true };
                return Err(VhostUserBlockConfigRefreshError::Frontend(source));
            }
        };
        let config_bytes = config
            .as_bytes()
            .try_into()
            .map_err(|_| VhostUserBlockConfigRefreshError::InvalidConfig)?;
        Ok(VirtioBlockConfigSpace::from_vhost_user(
            config_bytes,
            backend.available_features,
            cache_type,
        ))
    }

    fn dispatch_drained_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
    ) -> Result<VirtioBlockDeviceNotificationDispatch, VirtioBlockDeviceNotificationError> {
        if let Some(queue_index) = drained_notifications
            .iter()
            .copied()
            .find(|queue_index| *queue_index != 0)
        {
            return Err(VirtioBlockDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        match &mut self.backend {
            VirtioBlockBackend::File {
                backing,
                active_queue,
                rate_limiter,
                io_engine,
            } => {
                if drained_notifications.is_empty()
                    && matches!(io_engine, VirtioBlockFileIoEngine::Sync)
                {
                    return Ok(VirtioBlockDeviceNotificationDispatch::new(
                        drained_notifications,
                        None,
                        0,
                        0,
                    ));
                }
                let Some(queue) = active_queue.as_mut() else {
                    return Err(VirtioBlockDeviceNotificationError::Inactive {
                        drained_notifications,
                    });
                };
                let dispatch = match io_engine {
                    VirtioBlockFileIoEngine::Sync => queue.dispatch_with_rate_limiter(
                        memory,
                        backing.as_ref(),
                        self.device_id,
                        rate_limiter.as_mut(),
                    ),
                    VirtioBlockFileIoEngine::Async {
                        runtime,
                        generation,
                        terminal,
                        ..
                    } => {
                        if *terminal {
                            Err(VirtioBlockQueueDispatchError::AsyncRuntime {
                                completed_dispatch: Box::default(),
                                source: BlockAsyncRuntimeError::ExecutorInvariant,
                            })
                        } else {
                            queue.dispatch_with_async_runtime(
                                memory,
                                backing.as_ref(),
                                self.device_id,
                                rate_limiter.as_mut(),
                                runtime,
                                *generation,
                            )
                        }
                    }
                };
                match dispatch {
                    Ok(dispatch) => Ok(VirtioBlockDeviceNotificationDispatch::new(
                        drained_notifications,
                        Some(dispatch),
                        0,
                        0,
                    )),
                    Err(source) => Err(VirtioBlockDeviceNotificationError::QueueDispatch {
                        drained_notifications,
                        source,
                    }),
                }
            }
            VirtioBlockBackend::VhostUser(backend) => {
                backend.dispatch_notifications(drained_notifications)
            }
        }
    }

    pub fn activate_block(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioBlockDeviceActivationError> {
        let event_idx_enabled =
            virtio_feature_enabled(activation.driver_features(), VIRTIO_RING_FEATURE_EVENT_IDX);
        let indirect_descriptors_enabled = virtio_feature_enabled(
            activation.driver_features(),
            VIRTIO_RING_FEATURE_INDIRECT_DESC,
        );
        let queue_index = 0;
        let queue = activation
            .queue(queue_index)
            .map_err(|source| VirtioBlockDeviceActivationError::QueueMetadata {
                queue_index,
                source,
            })
            .and_then(|queue| {
                VirtioBlockQueue::from_mmio_queue_state_with_event_idx(
                    queue,
                    event_idx_enabled,
                    indirect_descriptors_enabled,
                )
                .map_err(|source| VirtioBlockDeviceActivationError::QueueBuild {
                    queue_index,
                    source,
                })
            })?;
        match &mut self.backend {
            VirtioBlockBackend::File {
                active_queue,
                io_engine,
                ..
            } => {
                if active_queue.is_some() {
                    return Err(VirtioBlockDeviceActivationError::AlreadyActive);
                }
                if let VirtioBlockFileIoEngine::Async { terminal, .. } = io_engine
                    && *terminal
                {
                    return Err(VirtioBlockDeviceActivationError::Terminal);
                }
                *active_queue = Some(queue);
                Ok(())
            }
            VirtioBlockBackend::VhostUser(backend) => {
                backend.activate(activation.driver_features(), queue)
            }
        }
    }

    pub fn reset(&mut self) {
        match &mut self.backend {
            VirtioBlockBackend::File {
                backing,
                active_queue,
                io_engine,
                ..
            } => {
                *active_queue = None;
                if let VirtioBlockFileIoEngine::Async {
                    runtime,
                    generation,
                    cache_type,
                    terminal,
                } = io_engine
                {
                    let rebound = runtime
                        .discard_generation_without_guest_memory(*generation)
                        .and_then(|()| runtime.bind_drive(Arc::clone(backing), *cache_type));
                    match rebound {
                        Ok(rebound) => *generation = rebound,
                        Err(_) => *terminal = true,
                    }
                }
            }
            VirtioBlockBackend::VhostUser(backend) => {
                if matches!(&backend.state, VhostUserBlockState::Active { .. }) {
                    backend.frontend = None;
                    backend.state = VhostUserBlockState::Terminal { was_active: true };
                }
            }
        }
    }
}

fn mark_async_engine_terminal(io_engine: &mut VirtioBlockFileIoEngine) {
    if let VirtioBlockFileIoEngine::Async { terminal, .. } = io_engine {
        *terminal = true;
    }
}

fn cancel_prospective_async(
    prospective: Option<(SharedBlockAsyncRuntime, BlockAsyncDriveGeneration)>,
) {
    if let Some((runtime, generation)) = prospective {
        let _ = runtime.discard_generation_without_guest_memory(generation);
    }
}

impl VhostUserBlockBackend {
    fn activate(
        &mut self,
        driver_features: u64,
        queue: VirtioBlockQueue,
    ) -> Result<(), VirtioBlockDeviceActivationError> {
        match &self.state {
            VhostUserBlockState::Prepared { .. } => {}
            VhostUserBlockState::Active { .. } => {
                return Err(VirtioBlockDeviceActivationError::AlreadyActive);
            }
            VhostUserBlockState::Terminal { .. } => {
                return Err(VirtioBlockDeviceActivationError::Terminal);
            }
        }
        if driver_features & VIRTIO_F_VERSION_1 == 0
            || driver_features & !self.available_features != 0
        {
            return Err(VirtioBlockDeviceActivationError::UnsupportedGuestFeatures);
        }

        let descriptor_range = queue
            .available
            .descriptor_table_range()
            .map_err(|_| VirtioBlockDeviceActivationError::QueueRange)?;
        let available_range = queue
            .available
            .available_ring_range()
            .map_err(|_| VirtioBlockDeviceActivationError::QueueRange)?;
        let used_range = queue
            .used
            .used_ring_range()
            .map_err(|_| VirtioBlockDeviceActivationError::QueueRange)?;
        let descriptor = self.translate_range(descriptor_range)?;
        let available = self.translate_range(available_range)?;
        let used = self.translate_range(used_range)?;
        let address = VhostUserVringAddress::new(descriptor, used, available)
            .map_err(VirtioBlockDeviceActivationError::Frontend)?;
        let available_index_address = available
            .checked_add(2)
            .ok_or(VirtioBlockDeviceActivationError::QueueRange)?;
        let available_index_pointer = usize::try_from(available_index_address)
            .map_err(|_| VirtioBlockDeviceActivationError::QueueRange)?
            as *const u16;
        // SAFETY: `available_range` was checked wholly inside one retained live
        // mapping, the virtqueue constructor checked two-byte alignment, and
        // the vCPU is stopped at its activation register write.
        let available_index = u16::from_le(unsafe { available_index_pointer.read_volatile() });

        let mut protocol_regions = Vec::new();
        protocol_regions
            .try_reserve_exact(self.memory_regions.len())
            .map_err(|_| VirtioBlockDeviceActivationError::AllocateMemoryTable)?;
        for region in &self.memory_regions {
            protocol_regions.push(
                region
                    .protocol_region()
                    .map_err(VirtioBlockDeviceActivationError::Frontend)?,
            );
        }
        let queue_size = queue.available.queue_size();
        let Some(frontend) = self.frontend.as_mut() else {
            return Err(VirtioBlockDeviceActivationError::Terminal);
        };
        let previous_state = std::mem::replace(
            &mut self.state,
            VhostUserBlockState::Terminal { was_active: false },
        );
        let (kick, backend_kick, call, backend_call) = match previous_state {
            VhostUserBlockState::Prepared {
                kick,
                backend_kick,
                call,
                backend_call,
            } => (kick, backend_kick, call, backend_call),
            VhostUserBlockState::Active { kick, call } => {
                self.state = VhostUserBlockState::Active { kick, call };
                return Err(VirtioBlockDeviceActivationError::AlreadyActive);
            }
            VhostUserBlockState::Terminal { was_active } => {
                self.state = VhostUserBlockState::Terminal { was_active };
                return Err(VirtioBlockDeviceActivationError::Terminal);
            }
        };
        // Firecracker pre-acknowledges this vhost-user-only bit before guest
        // negotiation. Linux does not know about it, so preserve it when
        // committing the guest-selected standard virtio features.
        let backend_features = driver_features | VHOST_USER_F_PROTOCOL_FEATURES;
        let result = (|| {
            frontend.set_features(backend_features)?;
            frontend.set_memory_table(&protocol_regions)?;
            frontend.set_vring_num(0, queue_size)?;
            frontend.set_vring_addr(0, address)?;
            frontend.set_vring_base(0, available_index)?;
            frontend.set_vring_call(0, &backend_call)?;
            frontend.set_vring_kick(0, &backend_kick)?;
            frontend.set_vring_enable(0, true)?;
            Ok::<(), VhostUserError>(())
        })();
        drop(protocol_regions);
        match result {
            Ok(()) => {
                self.state = VhostUserBlockState::Active { kick, call };
                Ok(())
            }
            Err(source) => {
                self.frontend = None;
                self.state = VhostUserBlockState::Terminal { was_active: false };
                Err(VirtioBlockDeviceActivationError::Frontend(source))
            }
        }
    }

    fn translate_range(
        &self,
        range: GuestMemoryRange,
    ) -> Result<u64, VirtioBlockDeviceActivationError> {
        self.memory_regions
            .iter()
            .find_map(|region| region.translate_range(range))
            .ok_or(VirtioBlockDeviceActivationError::QueueRange)
    }

    fn dispatch_notifications(
        &mut self,
        drained_notifications: Vec<usize>,
    ) -> Result<VirtioBlockDeviceNotificationDispatch, VirtioBlockDeviceNotificationError> {
        let (kick, call) = match &self.state {
            VhostUserBlockState::Prepared { .. } => {
                if drained_notifications.is_empty() {
                    return Ok(VirtioBlockDeviceNotificationDispatch::new(
                        drained_notifications,
                        None,
                        0,
                        0,
                    ));
                }
                return Err(VirtioBlockDeviceNotificationError::Inactive {
                    drained_notifications,
                });
            }
            VhostUserBlockState::Terminal { .. } => {
                return Err(VirtioBlockDeviceNotificationError::VhostUser {
                    drained_notifications,
                    calls: 0,
                    source: VhostUserBlockNotificationError::Terminal,
                });
            }
            VhostUserBlockState::Active { kick, call } => (kick, call),
        };
        let mut kicks = 0_u64;
        for _ in &drained_notifications {
            if let Err(source) = kick.signal() {
                self.frontend = None;
                self.state = VhostUserBlockState::Terminal { was_active: true };
                return Err(VirtioBlockDeviceNotificationError::VhostUser {
                    drained_notifications,
                    calls: 0,
                    source: VhostUserBlockNotificationError::Kick(source),
                });
            }
            kicks = kicks.saturating_add(1);
        }
        match call.drain() {
            Ok(CallDrainOutcome::WouldBlock) => Ok(VirtioBlockDeviceNotificationDispatch::new(
                drained_notifications,
                None,
                kicks,
                0,
            )),
            Ok(CallDrainOutcome::Drained(calls)) => Ok(VirtioBlockDeviceNotificationDispatch::new(
                drained_notifications,
                None,
                kicks,
                calls,
            )),
            Ok(CallDrainOutcome::Closed(calls)) => {
                self.frontend = None;
                self.state = VhostUserBlockState::Terminal { was_active: true };
                Err(VirtioBlockDeviceNotificationError::VhostUser {
                    drained_notifications,
                    calls,
                    source: VhostUserBlockNotificationError::Closed,
                })
            }
            Err(source) => {
                self.frontend = None;
                self.state = VhostUserBlockState::Terminal { was_active: true };
                Err(VirtioBlockDeviceNotificationError::VhostUser {
                    drained_notifications,
                    calls: 0,
                    source: VhostUserBlockNotificationError::Call(source),
                })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VhostUserBlockConfigRefreshError {
    UnsupportedBackend,
    Inactive,
    Terminal,
    Frontend(VhostUserError),
    InvalidConfig,
}

impl fmt::Display for VhostUserBlockConfigRefreshError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBackend => {
                formatter.write_str("block device is not backed by vhost-user")
            }
            Self::Inactive => formatter.write_str("vhost-user block frontend is not active"),
            Self::Terminal => formatter.write_str("vhost-user block frontend is terminal"),
            Self::Frontend(source) => {
                write!(formatter, "vhost-user config refresh failed: {source}")
            }
            Self::InvalidConfig => {
                formatter.write_str("vhost-user backend returned invalid block configuration")
            }
        }
    }
}

impl std::error::Error for VhostUserBlockConfigRefreshError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Frontend(source) => Some(source),
            Self::UnsupportedBackend | Self::Inactive | Self::Terminal | Self::InvalidConfig => {
                None
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Failure to publish a refreshed vhost-user block configuration to the guest.
///
/// `delivery_ambiguous` distinguishes a confirmed pre-delivery failure, which
/// can be rolled back, from a failure after interrupt delivery may have begun.
pub struct VhostUserBlockConfigSignalError {
    message: String,
    delivery_ambiguous: bool,
}

impl VhostUserBlockConfigSignalError {
    /// Creates a signaling failure with its interrupt-delivery classification.
    pub fn new(message: impl Into<String>, delivery_ambiguous: bool) -> Self {
        Self {
            message: message.into(),
            delivery_ambiguous,
        }
    }

    /// Returns whether the guest may already have observed the interrupt.
    pub const fn delivery_ambiguous(&self) -> bool {
        self.delivery_ambiguous
    }
}

impl fmt::Display for VhostUserBlockConfigSignalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for VhostUserBlockConfigSignalError {}

/// Backend-specific operations unavailable for a vhost-user block device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBlockBackendOperationError {
    UnsupportedBackend,
}

impl fmt::Display for VirtioBlockBackendOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("operation is unsupported for a vhost-user block device")
    }
}

impl std::error::Error for VirtioBlockBackendOperationError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveLiveUpdateMode {
    Patch,
    Replacement,
}

#[derive(Debug)]
pub enum VirtioBlockLiveUpdateError {
    UnsupportedBackend,
    MissingReplacementBacking,
    AsyncPreparation(BlockAsyncRuntimeError),
    AsyncTerminal(BlockAsyncRuntimeError),
    Queue(VirtioBlockQueueDispatchError),
}

impl VirtioBlockLiveUpdateError {
    pub const fn terminal(&self) -> bool {
        matches!(self, Self::AsyncTerminal(_) | Self::Queue(_))
    }
}

impl fmt::Display for VirtioBlockLiveUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedBackend => {
                formatter.write_str("live block update requires a regular-file backend")
            }
            Self::MissingReplacementBacking => {
                formatter.write_str("live block replacement is missing its prepared backing")
            }
            Self::AsyncPreparation(_) => {
                formatter.write_str("live block update failed to prepare Async I/O")
            }
            Self::AsyncTerminal(_) => {
                formatter.write_str("live block update entered terminal Async quiescence")
            }
            Self::Queue(_) => {
                formatter.write_str("live block update failed to publish completed requests")
            }
        }
    }
}

impl std::error::Error for VirtioBlockLiveUpdateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AsyncPreparation(source) | Self::AsyncTerminal(source) => Some(source),
            Self::Queue(source) => Some(source),
            Self::UnsupportedBackend | Self::MissingReplacementBacking => None,
        }
    }
}

#[derive(Debug)]
pub struct PreparedBlockDevice {
    drive_id: String,
    is_root_device: bool,
    io_engine: Option<DriveIoEngine>,
    cache_type: DriveCacheType,
    config_space: VirtioBlockConfigSpace,
    device: VirtioBlockDevice,
}

impl PreparedBlockDevice {
    /// Validates current vhost-user memory exports without cloning descriptors.
    pub fn preflight_vhost_user_memory(
        memory: &GuestMemory,
    ) -> Result<(), PreparedVhostUserBlockMemoryError> {
        VhostUserBlockMemoryRegion::preflight_guest_memory(memory)
    }

    pub fn from_config_with_backing(
        config: &DriveConfig,
        backing: Option<BlockFileBacking>,
    ) -> Result<Self, PreparedBlockDeviceError> {
        let DriveBackendConfig::File {
            is_read_only,
            io_engine,
            ..
        } = config.backend()
        else {
            return Err(PreparedBlockDeviceError::UnsupportedBackend {
                drive_id: config.drive_id().to_string(),
            });
        };
        let backing = match backing {
            Some(backing) => {
                if backing.is_read_only() != *is_read_only {
                    return Err(PreparedBlockDeviceError::BackingModeMismatch {
                        drive_id: config.drive_id().to_string(),
                    });
                }
                backing
            }
            None => BlockFileBacking::open(config).map_err(|source| {
                PreparedBlockDeviceError::OpenBacking {
                    drive_id: config.drive_id().to_string(),
                    source,
                }
            })?,
        };
        let config_space = VirtioBlockConfigSpace::from_backing(&backing, config.cache_type());
        let device_id = VirtioBlockDeviceId::from_bytes(config.drive_id().as_bytes());
        let mut device = VirtioBlockDevice::new(backing, device_id);
        if let Some(rate_limiter) = config.rate_limiter().and_then(VirtioBlockRateLimiter::new) {
            device = device.with_rate_limiter(rate_limiter);
        }

        Ok(Self {
            drive_id: config.drive_id().to_string(),
            is_root_device: config.is_root_device(),
            io_engine: Some(*io_engine),
            cache_type: config.cache_type(),
            config_space,
            device,
        })
    }

    pub fn from_config_with_vhost_user(
        config: &DriveConfig,
        frontend: PreparedVhostUserBlockFrontend,
        memory: &GuestMemory,
    ) -> Result<Self, PreparedBlockDeviceError> {
        if !matches!(config.backend(), DriveBackendConfig::VhostUser { .. }) {
            return Err(PreparedBlockDeviceError::ResourceKindMismatch {
                drive_id: config.drive_id().to_string(),
            });
        }
        let memory_regions =
            VhostUserBlockMemoryRegion::from_guest_memory(memory).map_err(|source| {
                PreparedBlockDeviceError::PrepareVhostMemory {
                    drive_id: config.drive_id().to_string(),
                    source,
                }
            })?;
        let (frontend, available_features, config_bytes) = frontend.into_parts();
        let config_space = VirtioBlockConfigSpace::from_vhost_user(
            config_bytes,
            available_features,
            config.cache_type(),
        );
        let device_id = VirtioBlockDeviceId::from_bytes(config.drive_id().as_bytes());
        let device = VirtioBlockDevice::new_vhost_user(
            frontend,
            available_features,
            memory_regions,
            device_id,
        )
        .map_err(|source| PreparedBlockDeviceError::PrepareVhostNotifier {
            drive_id: config.drive_id().to_string(),
            source,
        })?;
        Ok(Self {
            drive_id: config.drive_id().to_string(),
            is_root_device: config.is_root_device(),
            io_engine: None,
            cache_type: config.cache_type(),
            config_space,
            device,
        })
    }

    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root_device
    }

    pub const fn config_space(&self) -> VirtioBlockConfigSpace {
        self.config_space
    }

    pub fn device(&self) -> &VirtioBlockDevice {
        &self.device
    }

    pub fn device_mut(&mut self) -> &mut VirtioBlockDevice {
        &mut self.device
    }

    pub fn bind_async_runtime(
        &mut self,
        runtime: SharedBlockAsyncRuntime,
    ) -> Result<Option<BlockAsyncDriveGeneration>, BlockAsyncRuntimeError> {
        if self.io_engine != Some(DriveIoEngine::Async) {
            return Ok(None);
        }
        self.device
            .bind_async_runtime(self.cache_type, runtime)
            .map(Some)
    }

    pub fn into_parts(self) -> (String, bool, VirtioBlockConfigSpace, VirtioBlockDevice) {
        (
            self.drive_id,
            self.is_root_device,
            self.config_space,
            self.device,
        )
    }
}

/// Move-only runtime block insertion resource consumed by the VM owner.
pub enum RuntimeBlockDeviceResource {
    /// A file-backed device fully materialized on the process thread.
    Prepared(PreparedBlockDevice),
    /// A discovered vhost-user frontend awaiting live-memory materialization.
    VhostUser {
        config: DriveConfig,
        frontend: PreparedVhostUserBlockFrontend,
    },
}

impl RuntimeBlockDeviceResource {
    /// Wraps one fully prepared file-backed device.
    pub fn prepared(device: PreparedBlockDevice) -> Self {
        Self::Prepared(device)
    }

    /// Wraps one validated vhost-user configuration and discovered frontend.
    pub fn vhost_user(config: DriveConfig, frontend: PreparedVhostUserBlockFrontend) -> Self {
        Self::VhostUser { config, frontend }
    }

    /// Returns the stable drive identity without exposing owned resources.
    pub fn drive_id(&self) -> &str {
        match self {
            Self::Prepared(device) => device.drive_id(),
            Self::VhostUser { config, .. } => config.drive_id(),
        }
    }

    /// Returns whether owner-side live-memory materialization is required.
    pub const fn is_vhost_user(&self) -> bool {
        matches!(self, Self::VhostUser { .. })
    }
}

impl fmt::Debug for RuntimeBlockDeviceResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeBlockDeviceResource")
            .field(
                "kind",
                &if self.is_vhost_user() {
                    "vhost-user"
                } else {
                    "prepared"
                },
            )
            .field("resources", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Default)]
pub struct PreparedBlockDevices {
    devices: Vec<PreparedBlockDevice>,
}

impl PreparedBlockDevices {
    pub fn from_configs(configs: &DriveConfigs) -> Result<Self, PreparedBlockDeviceError> {
        Self::from_config_slice(configs.as_slice())
    }

    pub(crate) fn from_config_slice(
        configs: &[DriveConfig],
    ) -> Result<Self, PreparedBlockDeviceError> {
        Self::from_config_slice_with_backings(configs, BTreeMap::new())
    }

    pub(crate) fn from_config_slice_with_backings(
        configs: &[DriveConfig],
        mut backings: BTreeMap<String, BlockFileBacking>,
    ) -> Result<Self, PreparedBlockDeviceError> {
        if backings
            .keys()
            .any(|drive_id| !configs.iter().any(|config| config.drive_id() == drive_id))
        {
            return Err(PreparedBlockDeviceError::UnexpectedBacking);
        }

        let mut devices = Vec::new();
        devices
            .try_reserve_exact(configs.len())
            .map_err(|source| PreparedBlockDeviceError::AllocateDevices { source })?;

        for config in configs {
            let backing = backings.remove(config.drive_id());
            devices.push(PreparedBlockDevice::from_config_with_backing(
                config, backing,
            )?);
        }
        debug_assert!(backings.is_empty());

        Ok(Self { devices })
    }

    pub(crate) fn from_config_slice_with_resources(
        configs: &[DriveConfig],
        memory: &GuestMemory,
        mut backings: BTreeMap<String, BlockFileBacking>,
        mut frontends: BTreeMap<String, PreparedVhostUserBlockFrontend>,
    ) -> Result<Self, PreparedBlockDeviceError> {
        if backings
            .keys()
            .chain(frontends.keys())
            .any(|drive_id| !configs.iter().any(|config| config.drive_id() == drive_id))
        {
            return Err(PreparedBlockDeviceError::UnexpectedResource);
        }
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(configs.len())
            .map_err(|source| PreparedBlockDeviceError::AllocateDevices { source })?;
        for config in configs {
            let backing = backings.remove(config.drive_id());
            let frontend = frontends.remove(config.drive_id());
            let prepared = match config.backend() {
                DriveBackendConfig::File { .. } => {
                    if frontend.is_some() {
                        return Err(PreparedBlockDeviceError::ResourceKindMismatch {
                            drive_id: config.drive_id().to_string(),
                        });
                    }
                    PreparedBlockDevice::from_config_with_backing(config, backing)?
                }
                DriveBackendConfig::VhostUser { .. } => {
                    if backing.is_some() {
                        return Err(PreparedBlockDeviceError::ResourceKindMismatch {
                            drive_id: config.drive_id().to_string(),
                        });
                    }
                    let frontend =
                        frontend.ok_or_else(|| PreparedBlockDeviceError::MissingVhostFrontend {
                            drive_id: config.drive_id().to_string(),
                        })?;
                    PreparedBlockDevice::from_config_with_vhost_user(config, frontend, memory)?
                }
            };
            devices.push(prepared);
        }
        debug_assert!(backings.is_empty());
        debug_assert!(frontends.is_empty());
        Ok(Self { devices })
    }

    pub fn as_slice(&self) -> &[PreparedBlockDevice] {
        &self.devices
    }

    pub fn bind_async_runtime(
        &mut self,
        runtime: &SharedBlockAsyncRuntime,
    ) -> Result<(), BlockAsyncRuntimeError> {
        for device in &mut self.devices {
            let _ = device.bind_async_runtime(runtime.clone())?;
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn into_vec(self) -> Vec<PreparedBlockDevice> {
        self.devices
    }

    pub fn register_mmio(
        self,
        layout: BlockMmioLayout,
    ) -> Result<BlockMmioDevices, BlockMmioRegistrationError> {
        BlockMmioDevices::from_prepared(self, layout)
    }
}

#[derive(Debug)]
pub enum PreparedBlockDeviceError {
    AllocateDevices {
        source: TryReserveError,
    },
    OpenBacking {
        drive_id: String,
        source: BlockFileBackingError,
    },
    BackingModeMismatch {
        drive_id: String,
    },
    PrepareVhostMemory {
        drive_id: String,
        source: PreparedVhostUserBlockMemoryError,
    },
    PrepareVhostNotifier {
        drive_id: String,
        source: VhostUserNotifierError,
    },
    MissingVhostFrontend {
        drive_id: String,
    },
    ResourceKindMismatch {
        drive_id: String,
    },
    UnsupportedBackend {
        drive_id: String,
    },
    UnexpectedBacking,
    UnexpectedResource,
}

impl fmt::Display for PreparedBlockDeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocateDevices { source } => {
                write!(f, "failed to allocate prepared block devices: {source}")
            }
            Self::OpenBacking { source, .. } => {
                write!(f, "failed to prepare block device: {source}")
            }
            Self::BackingModeMismatch { .. } => {
                f.write_str("provided block backing mode does not match drive")
            }
            Self::PrepareVhostMemory { source, .. } => {
                write!(f, "failed to prepare vhost-user block memory: {source}")
            }
            Self::PrepareVhostNotifier { source, .. } => {
                write!(f, "failed to prepare vhost-user block notifier: {source}")
            }
            Self::MissingVhostFrontend { .. } => {
                f.write_str("configured vhost-user drive has no prepared frontend")
            }
            Self::ResourceKindMismatch { .. } => {
                f.write_str("provided block startup resource has the wrong backend kind")
            }
            Self::UnsupportedBackend { .. } => {
                f.write_str("vhost-user drive requires a prepared frontend")
            }
            Self::UnexpectedBacking => {
                f.write_str("provided block backing does not match a configured drive")
            }
            Self::UnexpectedResource => {
                f.write_str("provided block startup resource does not match a configured drive")
            }
        }
    }
}

impl std::error::Error for PreparedBlockDeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateDevices { source } => Some(source),
            Self::OpenBacking { source, .. } => Some(source),
            Self::PrepareVhostMemory { source, .. } => Some(source),
            Self::PrepareVhostNotifier { source, .. } => Some(source),
            Self::BackingModeMismatch { .. }
            | Self::MissingVhostFrontend { .. }
            | Self::ResourceKindMismatch { .. }
            | Self::UnsupportedBackend { .. }
            | Self::UnexpectedBacking
            | Self::UnexpectedResource => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockMmioLayout {
    base_address: GuestAddress,
    base_region_id: MmioRegionId,
    address_stride: u64,
    region_id_stride: u64,
}

impl BlockMmioLayout {
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

    fn validate(self) -> Result<(), BlockMmioRegistrationError> {
        if self.address_stride < VIRTIO_MMIO_DEVICE_WINDOW_SIZE {
            return Err(BlockMmioRegistrationError::AddressStrideTooSmall {
                stride: self.address_stride,
                minimum: VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            });
        }

        if self.region_id_stride == 0 {
            return Err(BlockMmioRegistrationError::DuplicateRegionIdStride {
                region_id: self.base_region_id,
            });
        }

        Ok(())
    }

    fn placement(
        self,
        index: usize,
    ) -> Result<BlockMmioDevicePlacement, BlockMmioRegistrationError> {
        let device_index = u64::try_from(index)
            .map_err(|_| BlockMmioRegistrationError::DeviceIndexTooLarge { index })?;
        let address_offset = device_index.checked_mul(self.address_stride).ok_or(
            BlockMmioRegistrationError::AddressOffsetOverflow {
                device_index,
                stride: self.address_stride,
            },
        )?;
        let address = self.base_address.checked_add(address_offset).ok_or(
            BlockMmioRegistrationError::AddressOverflow {
                base_address: self.base_address,
                offset: address_offset,
            },
        )?;
        let region_id_offset = device_index.checked_mul(self.region_id_stride).ok_or(
            BlockMmioRegistrationError::RegionIdOffsetOverflow {
                device_index,
                stride: self.region_id_stride,
            },
        )?;
        let region_id = self
            .base_region_id
            .raw_value()
            .checked_add(region_id_offset)
            .map(MmioRegionId::new)
            .ok_or(BlockMmioRegistrationError::RegionIdOverflow {
                base_region_id: self.base_region_id,
                offset: region_id_offset,
            })?;
        let region = MmioRegion::new(region_id, address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE).map_err(
            |source| BlockMmioRegistrationError::InvalidRegion {
                region_id,
                address,
                source,
            },
        )?;

        Ok(BlockMmioDevicePlacement {
            index,
            address,
            region_id,
            region,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BlockMmioDevicePlacement {
    index: usize,
    address: GuestAddress,
    region_id: MmioRegionId,
    region: MmioRegion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockMmioDeviceRegistration {
    index: usize,
    drive_id: String,
    region: MmioRegion,
}

impl BlockMmioDeviceRegistration {
    pub(crate) fn from_restored(index: usize, drive_id: String, region: MmioRegion) -> Self {
        Self {
            index,
            drive_id,
            region,
        }
    }

    pub const fn index(&self) -> usize {
        self.index
    }

    pub fn drive_id(&self) -> &str {
        &self.drive_id
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
pub struct BlockMmioDevices {
    dispatcher: MmioDispatcher,
    registrations: Vec<BlockMmioDeviceRegistration>,
}

impl BlockMmioDevices {
    pub fn from_prepared(
        prepared: PreparedBlockDevices,
        layout: BlockMmioLayout,
    ) -> Result<Self, BlockMmioRegistrationError> {
        layout.validate()?;

        let prepared_devices = prepared.into_vec();
        let mut registrations = Vec::new();
        registrations
            .try_reserve_exact(prepared_devices.len())
            .map_err(|source| BlockMmioRegistrationError::AllocateRegistrations { source })?;
        let mut placements = Vec::new();
        placements
            .try_reserve_exact(prepared_devices.len())
            .map_err(|source| BlockMmioRegistrationError::AllocatePlacements { source })?;
        for index in 0..prepared_devices.len() {
            placements.push(layout.placement(index)?);
        }

        let mut dispatcher = MmioDispatcher::new();
        for (prepared_device, placement) in prepared_devices.into_iter().zip(placements) {
            let (drive_id, _is_root_device, config_space, device) = prepared_device.into_parts();
            let handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
                VIRTIO_BLOCK_DEVICE_ID,
                config_space.available_features(),
                &VIRTIO_BLOCK_QUEUE_SIZES,
                config_space,
                device,
            )
            .map_err(|source| BlockMmioRegistrationError::BuildHandler {
                drive_id: drive_id.clone(),
                region_id: placement.region_id,
                source,
            })?;
            let region = dispatcher
                .insert_region(
                    placement.region_id,
                    placement.address,
                    VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
                )
                .map_err(|source| BlockMmioRegistrationError::InsertRegion {
                    drive_id: drive_id.clone(),
                    region_id: placement.region_id,
                    address: placement.address,
                    source,
                })?;
            dispatcher
                .register_handler(placement.region_id, handler)
                .map_err(|source| BlockMmioRegistrationError::RegisterHandler {
                    drive_id: drive_id.clone(),
                    region_id: placement.region_id,
                    source,
                })?;
            debug_assert_eq!(region, placement.region);
            registrations.push(BlockMmioDeviceRegistration {
                index: placement.index,
                drive_id,
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

    pub fn registrations(&self) -> &[BlockMmioDeviceRegistration] {
        &self.registrations
    }

    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }

    pub fn into_parts(self) -> (MmioDispatcher, Vec<BlockMmioDeviceRegistration>) {
        (self.dispatcher, self.registrations)
    }
}

#[derive(Debug)]
pub enum BlockMmioRegistrationError {
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
        drive_id: String,
        region_id: MmioRegionId,
        source: VirtioMmioRegisterHandlerError,
    },
    InsertRegion {
        drive_id: String,
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        drive_id: String,
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for BlockMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AddressStrideTooSmall { stride, minimum } => {
                write!(
                    f,
                    "block MMIO address stride {stride} is smaller than the required device window size {minimum}"
                )
            }
            Self::DuplicateRegionIdStride { region_id } => {
                write!(
                    f,
                    "block MMIO region id stride cannot be 0 because it would duplicate region id={region_id}"
                )
            }
            Self::DeviceIndexTooLarge { index } => {
                write!(f, "block MMIO device index {index} does not fit in u64")
            }
            Self::AddressOffsetOverflow {
                device_index,
                stride,
            } => {
                write!(
                    f,
                    "block MMIO address offset overflows for device index {device_index} with stride {stride}"
                )
            }
            Self::AddressOverflow {
                base_address,
                offset,
            } => {
                write!(
                    f,
                    "block MMIO address overflows from base {base_address} with offset {offset}"
                )
            }
            Self::RegionIdOffsetOverflow {
                device_index,
                stride,
            } => {
                write!(
                    f,
                    "block MMIO region id offset overflows for device index {device_index} with stride {stride}"
                )
            }
            Self::RegionIdOverflow {
                base_region_id,
                offset,
            } => {
                write!(
                    f,
                    "block MMIO region id overflows from base id={base_region_id} with offset {offset}"
                )
            }
            Self::AllocateRegistrations { source } => {
                write!(f, "failed to allocate block MMIO registrations: {source}")
            }
            Self::AllocatePlacements { source } => {
                write!(f, "failed to allocate block MMIO placements: {source}")
            }
            Self::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "invalid block MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::BuildHandler {
                drive_id,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to build block MMIO handler for drive {drive_id} region id={region_id}: {source}"
                )
            }
            Self::InsertRegion {
                drive_id,
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "failed to insert block MMIO region for drive {drive_id} region id={region_id} at {address}: {source}"
                )
            }
            Self::RegisterHandler {
                drive_id,
                region_id,
                source,
            } => {
                write!(
                    f,
                    "failed to register block MMIO handler for drive {drive_id} region id={region_id}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for BlockMmioRegistrationError {
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

impl<C: VirtioMmioDeviceConfigHandler> VirtioMmioRegisterHandler<C, VirtioBlockDevice> {
    pub fn snapshot_block_runtime_state_at(
        &self,
        rate_limiter_config: Option<DriveRateLimiterConfig>,
        now: Instant,
    ) -> Result<VirtioBlockRuntimeState, VirtioBlockRuntimeStateError> {
        let transport = self.transport_state();
        let activation = self.activation_handler();
        self.validate_transport_state(&transport, activation.is_activated())
            .map_err(|_: VirtioMmioTransportStateError| VirtioBlockRuntimeStateError::Transport)?;
        let active_queue = activation
            .active_queue()
            .map(VirtioBlockQueue::snapshot_state);
        let rate_limiter = activation
            .snapshot_rate_limiter_state_at(rate_limiter_config, now)
            .map_err(|_| VirtioBlockRuntimeStateError::RateLimiter)?;
        let state = VirtioBlockRuntimeState::new(transport, active_queue, rate_limiter);
        validate_native_v1_block_runtime_shape(&state)?;
        Ok(state)
    }

    pub fn dispatch_block_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBlockDeviceNotificationDispatch, VirtioBlockDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        let dispatch = self
            .activation_handler_mut()
            .dispatch_drained_queue_notifications(memory, drained_notifications);
        let needs_queue_interrupt = match &dispatch {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) => error.needs_queue_interrupt(),
        };
        if needs_queue_interrupt {
            self.mark_queue_interrupt_pending(0);
        }

        dispatch
    }

    pub fn publish_quiesced_async_completions(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBlockQueueDispatch, VirtioBlockQueueDispatchError> {
        let dispatch = self
            .activation_handler_mut()
            .publish_quiesced_async_completions(memory);
        let needs_queue_interrupt = match &dispatch {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) => error.completed_dispatch().needs_queue_interrupt(),
        };
        if needs_queue_interrupt {
            self.mark_queue_interrupt_pending(0);
        }
        dispatch
    }
}

impl VirtioBlockMmioHandler {
    pub fn block_backend_kind(&self) -> VirtioBlockBackendKind {
        self.activation_handler().backend_kind()
    }

    pub fn block_async_binding(
        &self,
    ) -> Option<(SharedBlockAsyncRuntime, BlockAsyncDriveGeneration)> {
        self.activation_handler().async_binding()
    }

    pub fn capture_block_device_state_at(
        &self,
        config: &DriveConfig,
        now: Instant,
    ) -> Result<VirtioBlockDeviceCaptureState, VirtioBlockDeviceCaptureError> {
        self.activation_handler()
            .capture_state_at(*self.device_config_handler(), config, now)
    }
}

impl VirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice> {
    pub fn block_backend_kind(&self) -> Result<VirtioBlockBackendKind, VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| core.activation.backend_kind())
    }

    pub fn block_async_binding(
        &self,
    ) -> Result<Option<(SharedBlockAsyncRuntime, BlockAsyncDriveGeneration)>, VirtioPciEndpointError>
    {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| core.activation.async_binding())
    }

    pub fn capture_block_device_state_at(
        &self,
        config: &DriveConfig,
        now: Instant,
    ) -> Result<(VirtioBlockDeviceCaptureState, VirtioPciTransportState), VirtioBlockPciCaptureError>
    {
        let work = self
            .admit_device_work()
            .map_err(VirtioBlockPciCaptureError::Endpoint)?;
        let device = work
            .with_core_mut(|core| {
                core.activation
                    .capture_state_at(core.device_config, config, now)
            })
            .map_err(VirtioBlockPciCaptureError::Endpoint)?
            .map_err(VirtioBlockPciCaptureError::Device)?;
        drop(work);
        let transport = self
            .transport_state()
            .map_err(VirtioBlockPciCaptureError::Endpoint)?;
        Ok((device, transport))
    }

    pub fn publish_quiesced_async_completions(
        &self,
        memory: &mut GuestMemory,
    ) -> Result<
        VirtioBlockQueueDispatch,
        VirtioPciDeviceOperationError<VirtioBlockQueueDispatchError, VirtioBlockQueueDispatch>,
    > {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let dispatch = work
            .with_core_mut(|core| {
                let dispatch = core.activation.publish_quiesced_async_completions(memory);
                let needs_queue_interrupt = match &dispatch {
                    Ok(dispatch) => dispatch.needs_queue_interrupt(),
                    Err(error) => error.completed_dispatch().needs_queue_interrupt(),
                };
                if needs_queue_interrupt {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index: 0 });
                }
                dispatch
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let endpoint = work.drain_interrupt_intents();
        VirtioPciDeviceOperationError::combine(dispatch, endpoint)
    }

    pub fn vhost_user_call_fd(&self) -> Result<Option<i32>, VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| core.activation.vhost_user_call_fd())
    }

    pub fn has_pending_block_queue_work(&self) -> Result<bool, VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| {
            !core
                .queue_notifications
                .pending_queue_notifications()
                .is_empty()
        })
    }

    pub fn dispatch_block_queue_notifications(
        &self,
        memory: &mut GuestMemory,
    ) -> Result<
        VirtioBlockDeviceNotificationDispatch,
        VirtioPciDeviceOperationError<
            VirtioBlockDeviceNotificationError,
            VirtioBlockDeviceNotificationDispatch,
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
                    .dispatch_drained_queue_notifications(memory, drained_notifications);
                let needs_queue_interrupt = match &dispatch {
                    Ok(dispatch) => dispatch.needs_queue_interrupt(),
                    Err(error) => error.needs_queue_interrupt(),
                };
                if needs_queue_interrupt {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index: 0 });
                }
                dispatch
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let endpoint = work.drain_interrupt_intents();
        VirtioPciDeviceOperationError::combine(dispatch, endpoint)
    }

    pub fn update_block_device_with_opened(
        &self,
        config: &DriveConfig,
        backing: Option<BlockFileBacking>,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
    ) -> Result<(), VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        let backing_changed = backing.is_some();
        work.with_core_mut(|core| {
            if let Some(backing) = backing {
                let config_space =
                    VirtioBlockConfigSpace::from_backing(&backing, config.cache_type());
                core.activation
                    .refresh_backing(backing)
                    .map_err(|_| VirtioPciEndpointError::UnsupportedDeviceOperation)?;
                core.device_config = config_space;
                core.device.increment_config_generation();
                core.record_interrupt_intent(VirtioInterruptIntent::Configuration);
            }
            if let Some(rate_limiter) = rate_limiter_update {
                core.activation
                    .update_rate_limiter(rate_limiter)
                    .map_err(|_| VirtioPciEndpointError::UnsupportedDeviceOperation)?;
            }
            Ok::<(), VirtioPciEndpointError>(())
        })??;
        if backing_changed {
            work.drain_interrupt_intents()?;
        }
        Ok(())
    }

    pub fn update_live_block_device_with_opened(
        &self,
        memory: &mut GuestMemory,
        config: &DriveConfig,
        backing: Option<BlockFileBacking>,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
        mode: DriveLiveUpdateMode,
        shared_runtime: &SharedBlockAsyncRuntime,
    ) -> Result<
        VirtioBlockQueueDispatch,
        VirtioPciDeviceOperationError<VirtioBlockLiveUpdateError, VirtioBlockQueueDispatch>,
    > {
        let work = self
            .admit_device_work()
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let config_space = backing
            .as_ref()
            .map(|backing| VirtioBlockConfigSpace::from_backing(backing, config.cache_type()));
        let file_state_changed = config_space.is_some() || mode == DriveLiveUpdateMode::Replacement;
        let update = work
            .with_core_mut(|core| {
                let dispatch = core.activation.update_file_backend_with_opened(
                    memory,
                    config,
                    backing,
                    rate_limiter_update,
                    mode,
                    shared_runtime,
                )?;
                if dispatch.needs_queue_interrupt() {
                    core.record_interrupt_intent(VirtioInterruptIntent::Queue { queue_index: 0 });
                }
                if let Some(config_space) = config_space {
                    core.device_config = config_space;
                    core.device.increment_config_generation();
                    core.record_interrupt_intent(VirtioInterruptIntent::Configuration);
                }
                Ok::<_, VirtioBlockLiveUpdateError>(dispatch)
            })
            .map_err(VirtioPciDeviceOperationError::Endpoint)?;
        let endpoint =
            if file_state_changed || update.as_ref().is_ok_and(|d| d.needs_queue_interrupt()) {
                work.drain_interrupt_intents()
            } else {
                Ok(())
            };
        VirtioPciDeviceOperationError::combine(update, endpoint)
    }

    /// Drains and detaches Async work after PCI teardown has suspended all
    /// guest-visible paths and ordinary endpoint work admission.
    pub fn retire_quiesced_async(
        &self,
        memory: &mut GuestMemory,
    ) -> Result<
        VirtioBlockQueueDispatch,
        VirtioPciDeviceOperationError<VirtioBlockLiveUpdateError, VirtioBlockQueueDispatch>,
    > {
        self.with_quiesced_core_mut(|core| core.activation.retire_async(memory))
            .map_err(VirtioPciDeviceOperationError::Endpoint)?
            .map_err(|source| VirtioPciDeviceOperationError::Device(Box::new(source)))
    }

    pub fn refresh_vhost_user_block_config(
        &self,
        cache_type: DriveCacheType,
    ) -> Result<(), DriveUpdateError> {
        let work =
            self.admit_device_work()
                .map_err(|source| DriveUpdateError::ActiveSessionCommand {
                    message: source.to_string(),
                })?;
        let snapshot = work
            .with_core_mut(|core| {
                let config_space = core
                    .activation
                    .refreshed_vhost_user_config(cache_type)
                    .map_err(|source| DriveUpdateError::ActiveSessionCommand {
                        message: source.to_string(),
                    })?;
                let snapshot = (core.device, core.device_config);
                core.device_config = config_space;
                core.device.increment_config_generation();
                core.record_interrupt_intent(VirtioInterruptIntent::Configuration);
                Ok::<_, DriveUpdateError>(snapshot)
            })
            .map_err(|source| DriveUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            })??;

        if let Err(source) = work.drain_interrupt_intents() {
            let message = source.to_string();
            if source.delivery_ambiguous() {
                return Err(DriveUpdateError::TerminalActiveSessionCommand { message });
            }
            work.with_core_mut(|core| {
                core.device = snapshot.0;
                core.device_config = snapshot.1;
            })
            .map_err(|rollback| DriveUpdateError::TerminalActiveSessionCommand {
                message: format!(
                    "configuration interrupt failed before delivery ({message}); rollback failed: {rollback}"
                ),
            })?;
            return Err(DriveUpdateError::ActiveSessionCommand { message });
        }
        Ok(())
    }

    pub fn update_block_rate_limiter(
        &self,
        rate_limiter: DriveRateLimiterConfig,
    ) -> Result<(), VirtioPciEndpointError> {
        let work = self.admit_device_work()?;
        work.with_core_mut(|core| {
            core.activation
                .update_rate_limiter(rate_limiter)
                .map_err(|_| VirtioPciEndpointError::UnsupportedDeviceOperation)
        })?
    }
}

impl VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice> {
    /// Refreshes the vhost-user configuration and records a pending interrupt.
    pub fn refresh_vhost_user_block_config(
        &mut self,
        cache_type: DriveCacheType,
    ) -> Result<(), DriveUpdateError> {
        self.refresh_vhost_user_block_config_with_signal(cache_type, || Ok(()))
    }

    /// Refreshes the vhost-user configuration and atomically signals the guest.
    ///
    /// A confirmed signaling failure restores the previous configuration and
    /// transport state. An ambiguous delivery failure leaves the new state in
    /// place and returns a terminal session error.
    pub fn refresh_vhost_user_block_config_with_signal(
        &mut self,
        cache_type: DriveCacheType,
        signal: impl FnOnce() -> Result<(), VhostUserBlockConfigSignalError>,
    ) -> Result<(), DriveUpdateError> {
        let config_space = self
            .activation_handler_mut()
            .refreshed_vhost_user_config(cache_type)
            .map_err(|source| DriveUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            })?;
        let transport_snapshot = self.transport_state();
        let config_snapshot = *self.device_config_handler();
        *self.device_config_handler_mut() = config_space;
        self.increment_config_generation();
        self.mark_config_interrupt_pending();
        if let Err(source) = signal() {
            let message = source.to_string();
            if source.delivery_ambiguous() {
                return Err(DriveUpdateError::TerminalActiveSessionCommand { message });
            }
            *self.device_config_handler_mut() = config_snapshot;
            let activation_is_active = self.activation_handler().is_activated();
            self.restore_transport_state(&transport_snapshot, activation_is_active)
                .map_err(|rollback| DriveUpdateError::TerminalActiveSessionCommand {
                    message: format!(
                        "configuration interrupt failed before delivery ({message}); rollback failed: {rollback}"
                    ),
                })?;
            return Err(DriveUpdateError::ActiveSessionCommand { message });
        }
        Ok(())
    }

    pub fn refresh_block_backing(&mut self, config: &DriveConfig) -> Result<(), DriveUpdateError> {
        let backing =
            BlockFileBacking::open(config).map_err(|source| DriveUpdateError::OpenBacking {
                drive_id: config.drive_id().to_string(),
                message: source.to_string(),
            })?;

        self.refresh_block_backing_with_opened(config, backing)?;

        Ok(())
    }

    pub fn refresh_block_backing_with_opened(
        &mut self,
        config: &DriveConfig,
        backing: BlockFileBacking,
    ) -> Result<(), DriveUpdateError> {
        let config_space = VirtioBlockConfigSpace::from_backing(&backing, config.cache_type());

        self.activation_handler_mut()
            .refresh_backing(backing)
            .map_err(|_| DriveUpdateError::UnsupportedBackend)?;
        *self.device_config_handler_mut() = config_space;
        self.increment_config_generation();
        self.mark_config_interrupt_pending();
        Ok(())
    }

    pub fn update_block_rate_limiter(
        &mut self,
        rate_limiter: DriveRateLimiterConfig,
    ) -> Result<(), DriveUpdateError> {
        self.activation_handler_mut()
            .update_rate_limiter(rate_limiter)
            .map_err(|_| DriveUpdateError::UnsupportedBackend)
    }

    pub fn update_live_block_device_with_opened(
        &mut self,
        memory: &mut GuestMemory,
        config: &DriveConfig,
        backing: Option<BlockFileBacking>,
        rate_limiter_update: Option<DriveRateLimiterConfig>,
        mode: DriveLiveUpdateMode,
        shared_runtime: &SharedBlockAsyncRuntime,
    ) -> Result<VirtioBlockQueueDispatch, VirtioBlockLiveUpdateError> {
        let config_space = backing
            .as_ref()
            .map(|backing| VirtioBlockConfigSpace::from_backing(backing, config.cache_type()));
        let dispatch = self
            .activation_handler_mut()
            .update_file_backend_with_opened(
                memory,
                config,
                backing,
                rate_limiter_update,
                mode,
                shared_runtime,
            )?;
        if dispatch.needs_queue_interrupt() {
            self.mark_queue_interrupt_pending(0);
        }
        if let Some(config_space) = config_space {
            *self.device_config_handler_mut() = config_space;
            self.increment_config_generation();
            self.mark_config_interrupt_pending();
        }
        Ok(dispatch)
    }
}

impl VirtioMmioDeviceActivationHandler for VirtioBlockDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        self.activate_block(activation).map_err(Into::into)
    }

    fn reset(&mut self) {
        VirtioBlockDevice::reset(self);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBlockDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
    queue_dispatch: Option<VirtioBlockQueueDispatch>,
    vhost_user_kicks: u64,
    vhost_user_calls: u64,
}

impl VirtioBlockDeviceNotificationDispatch {
    const fn new(
        drained_notifications: Vec<usize>,
        queue_dispatch: Option<VirtioBlockQueueDispatch>,
        vhost_user_kicks: u64,
        vhost_user_calls: u64,
    ) -> Self {
        Self {
            drained_notifications,
            queue_dispatch,
            vhost_user_kicks,
            vhost_user_calls,
        }
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub fn queue_dispatch(&self) -> Option<&VirtioBlockQueueDispatch> {
        self.queue_dispatch.as_ref()
    }

    pub const fn vhost_user_kicks(&self) -> u64 {
        self.vhost_user_kicks
    }

    pub const fn vhost_user_calls(&self) -> u64 {
        self.vhost_user_calls
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.vhost_user_calls != 0
            || self
                .queue_dispatch
                .as_ref()
                .is_some_and(VirtioBlockQueueDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub enum VirtioBlockDeviceNotificationError {
    Inactive {
        drained_notifications: Vec<usize>,
    },
    UnsupportedQueue {
        drained_notifications: Vec<usize>,
        queue_index: usize,
    },
    QueueDispatch {
        drained_notifications: Vec<usize>,
        source: VirtioBlockQueueDispatchError,
    },
    VhostUser {
        drained_notifications: Vec<usize>,
        calls: u64,
        source: VhostUserBlockNotificationError,
    },
}

impl VirtioBlockDeviceNotificationError {
    pub fn drained_notifications(&self) -> &[usize] {
        match self {
            Self::Inactive {
                drained_notifications,
            }
            | Self::UnsupportedQueue {
                drained_notifications,
                ..
            }
            | Self::QueueDispatch {
                drained_notifications,
                ..
            }
            | Self::VhostUser {
                drained_notifications,
                ..
            } => drained_notifications,
        }
    }

    pub const fn completed_dispatch(&self) -> Option<&VirtioBlockQueueDispatch> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source.completed_dispatch()),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } | Self::VhostUser { .. } => None,
        }
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        matches!(self, Self::VhostUser { calls, .. } if *calls != 0)
            || matches!(self, Self::QueueDispatch { source, .. } if source.completed_dispatch().needs_queue_interrupt())
    }
}

impl fmt::Display for VirtioBlockDeviceNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive { .. } => f.write_str(
                "virtio-block queue notification cannot be dispatched before activation",
            ),
            Self::UnsupportedQueue { queue_index, .. } => {
                write!(
                    f,
                    "virtio-block queue notification for unsupported queue {queue_index}"
                )
            }
            Self::QueueDispatch { source, .. } => {
                write!(
                    f,
                    "failed to dispatch virtio-block queue notification: {source}"
                )
            }
            Self::VhostUser { source, .. } => {
                write!(
                    f,
                    "failed to dispatch vhost-user block notification: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBlockDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source),
            Self::VhostUser { source, .. } => Some(source),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VhostUserBlockNotificationError {
    Kick(VhostUserNotifierError),
    Call(VhostUserNotifierError),
    Closed,
    Terminal,
}

impl fmt::Display for VhostUserBlockNotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kick(_) => formatter.write_str("vhost-user block kick failed"),
            Self::Call(_) => formatter.write_str("vhost-user block call failed"),
            Self::Closed => formatter.write_str("vhost-user block backend closed"),
            Self::Terminal => formatter.write_str("vhost-user block device is terminal"),
        }
    }
}

impl std::error::Error for VhostUserBlockNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Kick(source) | Self::Call(source) => Some(source),
            Self::Closed | Self::Terminal => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioBlockDeviceActivationError {
    AlreadyActive,
    Terminal,
    UnsupportedGuestFeatures,
    QueueRange,
    AllocateMemoryTable,
    Frontend(VhostUserError),
    Notifier(VhostUserNotifierError),
    QueueMetadata {
        queue_index: u32,
        source: VirtioMmioQueueRegisterError,
    },
    QueueBuild {
        queue_index: u32,
        source: VirtioBlockQueueBuildError,
    },
}

impl fmt::Display for VirtioBlockDeviceActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("virtio-block device is already active"),
            Self::Terminal => f.write_str("vhost-user block device is terminal"),
            Self::UnsupportedGuestFeatures => {
                f.write_str("guest vhost-user block features are unsupported")
            }
            Self::QueueRange => f.write_str("vhost-user block queue range is invalid"),
            Self::AllocateMemoryTable => {
                f.write_str("failed to allocate vhost-user block memory table")
            }
            Self::Frontend(source) => write!(f, "vhost-user block activation failed: {source}"),
            Self::Notifier(source) => {
                write!(f, "vhost-user block notifier creation failed: {source}")
            }
            Self::QueueMetadata {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to read virtio-block queue {queue_index} activation metadata: {source}"
                )
            }
            Self::QueueBuild {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to activate virtio-block queue {queue_index}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBlockDeviceActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueMetadata { source, .. } => Some(source),
            Self::QueueBuild { source, .. } => Some(source),
            Self::Frontend(source) => Some(source),
            Self::Notifier(source) => Some(source),
            Self::AlreadyActive
            | Self::Terminal
            | Self::UnsupportedGuestFeatures
            | Self::QueueRange
            | Self::AllocateMemoryTable => None,
        }
    }
}

impl From<VirtioBlockDeviceActivationError> for VirtioMmioDeviceActivationError {
    fn from(source: VirtioBlockDeviceActivationError) -> Self {
        MmioHandlerError::new(source.to_string()).into()
    }
}

fn validate_drive_id(source: DriveIdSource, drive_id: &str) -> Result<(), DriveConfigError> {
    if drive_id.is_empty() {
        return Err(DriveConfigError::EmptyDriveId { source });
    }

    if !drive_id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(DriveConfigError::InvalidDriveId {
            source,
            drive_id: drive_id.to_string(),
        });
    }

    Ok(())
}

fn validate_drive_update_id(source: DriveIdSource, drive_id: &str) -> Result<(), DriveUpdateError> {
    if drive_id.is_empty() {
        return Err(DriveUpdateError::EmptyDriveId { source });
    }

    if !drive_id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(DriveUpdateError::InvalidDriveId {
            source,
            drive_id: drive_id.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::error::Error as _;
    use std::ffi::CString;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::mem::{MaybeUninit, size_of};
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use bangbang_vhost_user::{
        SUPPORTED_VIRTIO_FEATURES, VHOST_USER_F_PROTOCOL_FEATURES, VHOST_USER_PROTOCOL_F_CONFIG,
        VHOST_USER_PROTOCOL_F_REPLY_ACK, VIRTIO_BLK_F_FLUSH, VIRTIO_F_VERSION_1,
    };

    use super::async_executor::{
        BlockAsyncExecutorConfig, BlockAsyncHostIo, BlockAsyncRuntimeError,
        BlockAsyncTransferResult, SharedBlockAsyncRuntime,
    };

    use crate::interrupt::DeviceInterruptKind;
    use crate::memory::{
        GuestAddress, GuestMemory, GuestMemoryBacking, GuestMemoryLayout, GuestMemoryRange,
    };
    use crate::message_interrupt::{
        GuestMessage, GuestMessageInterrupt, GuestMessageInterruptRegistry,
        GuestMessageInterruptSignalError,
    };
    use crate::metrics::{BlockDeviceMetrics, SharedBlockDeviceMetrics};
    use crate::mmio::{
        MmioAccess, MmioAccessBytes, MmioBus, MmioDispatchOutcome, MmioHandler, MmioOperation,
        MmioRegionId,
    };
    use crate::pci::{PciBarAddressSpace, PciBarAllocator};
    use crate::virtio::VirtioDeviceType;
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_MAGIC_VALUE, VirtioMmioDeviceActivation,
        VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
        VirtioMmioDeviceRegisters, VirtioMmioQueueRegisters, VirtioMmioRegister,
        VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
    };
    use crate::virtio_pci::{
        VIRTIO_PCI_CAPABILITY_BAR_SIZE, VIRTIO_PCI_MSIX_TABLE_OFFSET, VirtioPciEndpoint,
        VirtioPciIdentity,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_INDIRECT, VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE,
        VIRTQUEUE_DESCRIPTOR_SIZE, VirtqueueAvailableRing, VirtqueueAvailableRingError,
        VirtqueueDescriptorChain, VirtqueueDescriptorChainOptions, VirtqueueUsedRing,
        VirtqueueUsedRingError, read_descriptor_chain,
    };

    use super::{
        BlockCaptureIoEngine, BlockFileBacking, BlockFileBackingError, BlockMmioDevices,
        BlockMmioLayout, BlockMmioRegistrationError, DriveCacheType, DriveConfig, DriveConfigError,
        DriveConfigInput, DriveConfigs, DriveIdSource, DriveIoEngine, DriveLiveUpdateMode,
        DriveRateLimiterConfig, DriveReplacementIdentityField, DriveRuntimeMutationError,
        DriveTokenBucketConfig, DriveUpdateError, DriveUpdateInput, PreparedBlockDevice,
        PreparedBlockDeviceError, PreparedBlockDevices, PreparedVhostUserBlockFrontend,
        PreparedVhostUserBlockFrontendError, PreparedVhostUserBlockMemoryError,
        RuntimeBlockDeviceResource, SnapshotBlockFileBackingError,
        VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE, VIRTIO_BLOCK_CONFIG_SIZE, VIRTIO_BLOCK_DEVICE_ID,
        VIRTIO_BLOCK_FEATURE_FLUSH, VIRTIO_BLOCK_FEATURE_READ_ONLY, VIRTIO_BLOCK_ID_BYTES,
        VIRTIO_BLOCK_QUEUE_COUNT, VIRTIO_BLOCK_QUEUE_SIZE, VIRTIO_BLOCK_QUEUE_SIZES,
        VIRTIO_BLOCK_REQUEST_HEADER_SIZE, VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
        VIRTIO_BLOCK_REQUEST_TYPE_GET_ID, VIRTIO_BLOCK_REQUEST_TYPE_IN,
        VIRTIO_BLOCK_REQUEST_TYPE_OUT, VIRTIO_BLOCK_SECTOR_SHIFT, VIRTIO_BLOCK_SECTOR_SIZE,
        VIRTIO_BLOCK_STATUS_IOERR, VIRTIO_BLOCK_STATUS_OK, VIRTIO_BLOCK_STATUS_SIZE,
        VIRTIO_BLOCK_STATUS_UNSUPPORTED, VIRTIO_FEATURE_VERSION_1, VIRTIO_RING_FEATURE_EVENT_IDX,
        VIRTIO_RING_FEATURE_INDIRECT_DESC, VhostUserBlockConfigRefreshError,
        VhostUserBlockConfigSignalError, VhostUserBlockNotificationError, VhostUserBlockState,
        VirtioBlockBackend, VirtioBlockBackendKind, VirtioBlockConfigSpace, VirtioBlockDevice,
        VirtioBlockDeviceActivationError, VirtioBlockDeviceCaptureError, VirtioBlockDeviceId,
        VirtioBlockDeviceNotificationError, VirtioBlockQueue, VirtioBlockQueueBuildError,
        VirtioBlockQueueDispatch, VirtioBlockQueueDispatchError, VirtioBlockQueueSnapshotError,
        VirtioBlockRateLimiter, VirtioBlockRequest, VirtioBlockRequestCompletion,
        VirtioBlockRequestError, VirtioBlockRequestExecutionError,
        VirtioBlockRequestExecutionOutcome, VirtioBlockRequestType, normalize_completion_status,
    };

    static NEXT_TEMP_PATH_ID: AtomicUsize = AtomicUsize::new(0);
    const TEST_MMIO_BASE: u64 = 0x1000_0000;
    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x1000);
    const TEST_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x5000);
    const TEST_USED_RING: GuestAddress = GuestAddress::new(0x6000);
    const TEST_INDIRECT_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x7000);
    const TEST_QUEUE_SIZE: u16 = 8;
    const TEST_MEMORY_SIZE: u64 = 0x10_000;
    const TEST_AVAILABLE_RING_IDX_OFFSET: u64 = 2;
    const TEST_AVAILABLE_RING_RING_OFFSET: u64 = 4;
    const TEST_AVAILABLE_RING_ENTRY_SIZE: u64 = 2;
    const TEST_USED_RING_IDX_OFFSET: u64 = 2;
    const TEST_USED_RING_RING_OFFSET: u64 = 4;
    const TEST_USED_RING_ELEMENT_SIZE: u64 = 8;
    const EVENT_IDX_DRIVER_FEATURE: u32 = 1_u32 << VIRTIO_RING_FEATURE_EVENT_IDX;
    const INDIRECT_DESC_DRIVER_FEATURE: u32 = 1_u32 << VIRTIO_RING_FEATURE_INDIRECT_DESC;
    const HEADER_ADDR: GuestAddress = GuestAddress::new(0x2000);
    const DATA_ADDR: GuestAddress = GuestAddress::new(0x3000);
    const STATUS_ADDR: GuestAddress = GuestAddress::new(0x4000);
    const TEST_DEVICE_ID: VirtioBlockDeviceId = VirtioBlockDeviceId::new(*b"bangbang-test-id-000");
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    const VHOST_USER_HEADER_SIZE: usize = 12;
    const VHOST_USER_VERSION: u32 = 1;
    const VHOST_USER_REPLY: u32 = 1 << 2;
    const VHOST_USER_NEED_REPLY: u32 = 1 << 3;

    struct TestVhostUserRequest {
        code: u32,
        need_reply: bool,
        body: Vec<u8>,
        descriptors: Vec<OwnedFd>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct TestVhostUserObservation {
        guest_features: u64,
        memory_region_count: u32,
        queue_size: u32,
    }

    fn receive_test_vhost_user_request(stream: &mut UnixStream) -> TestVhostUserRequest {
        let mut header = [0_u8; VHOST_USER_HEADER_SIZE];
        let mut descriptors = receive_test_vhost_user_exact(stream, &mut header);
        let code = test_vhost_user_u32(&header, 0);
        let flags = test_vhost_user_u32(&header, 4);
        let body_size = usize::try_from(test_vhost_user_u32(&header, 8))
            .expect("test vhost-user body size should fit usize");
        assert_eq!(flags & 0x3, VHOST_USER_VERSION);
        assert_eq!(flags & VHOST_USER_REPLY, 0);
        assert_eq!(flags & !(0x3 | VHOST_USER_REPLY | VHOST_USER_NEED_REPLY), 0);
        assert!(body_size <= 0x1000);
        let mut body = vec![0_u8; body_size];
        descriptors.extend(receive_test_vhost_user_exact(stream, &mut body));
        TestVhostUserRequest {
            code,
            need_reply: flags & VHOST_USER_NEED_REPLY != 0,
            body,
            descriptors,
        }
    }

    fn receive_test_vhost_user_exact(stream: &UnixStream, bytes: &mut [u8]) -> Vec<OwnedFd> {
        let mut received = 0_usize;
        let mut descriptors = Vec::new();
        while received < bytes.len() {
            let (count, mut attempt_descriptors) =
                receive_test_vhost_user_once(stream.as_raw_fd(), &mut bytes[received..]);
            assert_ne!(count, 0, "test vhost-user peer reached EOF");
            received = received
                .checked_add(count)
                .expect("test receive count should not overflow");
            descriptors.append(&mut attempt_descriptors);
        }
        descriptors
    }

    fn receive_test_vhost_user_once(descriptor: i32, bytes: &mut [u8]) -> (usize, Vec<OwnedFd>) {
        let mut iovec = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut control = [0_usize; 32];
        // SAFETY: An all-zero message header is valid. It receives into the
        // live byte slice and aligned control buffer installed immediately below.
        let mut message: libc::msghdr = unsafe { MaybeUninit::zeroed().assume_init() };
        message.msg_iov = &raw mut iovec;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = (control.len() * size_of::<usize>())
            .try_into()
            .expect("test control buffer size should fit platform type");
        // SAFETY: `message` points only to live writable buffers for this call.
        let result = unsafe { libc::recvmsg(descriptor, &raw mut message, 0) };
        assert!(result >= 0, "test vhost-user receive should succeed");
        assert_eq!(message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC), 0);
        let received = usize::try_from(result).expect("receive count should fit usize");
        assert!(received <= bytes.len());

        let mut descriptors = Vec::new();
        // SAFETY: The kernel initialized the returned control region. Each
        // header and descriptor lies within the fixed aligned buffer, and every
        // nonnegative received descriptor is adopted exactly once.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&raw const message);
            while !header.is_null() {
                assert_eq!((*header).cmsg_level, libc::SOL_SOCKET);
                assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
                let header_size = usize::try_from(libc::CMSG_LEN(0))
                    .expect("control header size should fit usize");
                let declared =
                    usize::try_from((*header).cmsg_len).expect("control length should fit usize");
                assert!(declared >= header_size);
                let data_size = declared - header_size;
                assert_ne!(data_size, 0);
                assert_eq!(data_size % size_of::<i32>(), 0);
                for index in 0..data_size / size_of::<i32>() {
                    let offset = index * size_of::<i32>();
                    let raw =
                        std::ptr::read_unaligned(libc::CMSG_DATA(header).add(offset).cast::<i32>());
                    assert!(raw >= 0);
                    descriptors.push(OwnedFd::from_raw_fd(raw));
                }
                header = libc::CMSG_NXTHDR(&raw const message, header);
            }
        }
        for descriptor in &descriptors {
            // SAFETY: F_GETFD/F_SETFD inspect and update only this live owned fd.
            let flags = unsafe { libc::fcntl(descriptor.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0);
            if flags & libc::FD_CLOEXEC == 0 {
                // SAFETY: The descriptor remains live and the flags came from F_GETFD.
                let result = unsafe {
                    libc::fcntl(
                        descriptor.as_raw_fd(),
                        libc::F_SETFD,
                        flags | libc::FD_CLOEXEC,
                    )
                };
                assert_eq!(result, 0);
            }
        }
        (received, descriptors)
    }

    fn expect_test_vhost_user_request(
        stream: &mut UnixStream,
        code: u32,
        need_reply: bool,
        descriptor_count: usize,
    ) -> TestVhostUserRequest {
        let request = receive_test_vhost_user_request(stream);
        assert_eq!(request.code, code);
        assert_eq!(request.need_reply, need_reply);
        assert_eq!(request.descriptors.len(), descriptor_count);
        request
    }

    fn send_test_vhost_user_reply(stream: &mut UnixStream, code: u32, body: &[u8]) {
        let mut frame = Vec::with_capacity(VHOST_USER_HEADER_SIZE + body.len());
        frame.extend_from_slice(&code.to_ne_bytes());
        frame.extend_from_slice(&(VHOST_USER_VERSION | VHOST_USER_REPLY).to_ne_bytes());
        frame.extend_from_slice(
            &u32::try_from(body.len())
                .expect("test reply size should fit u32")
                .to_ne_bytes(),
        );
        frame.extend_from_slice(body);
        stream
            .write_all(&frame)
            .expect("test vhost-user reply should send");
    }

    fn acknowledge_test_vhost_user_request(
        stream: &mut UnixStream,
        request: &TestVhostUserRequest,
    ) {
        assert!(request.need_reply);
        send_test_vhost_user_reply(stream, request.code, &0_u64.to_ne_bytes());
    }

    fn test_vhost_user_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_ne_bytes(
            bytes[offset..offset + 4]
                .try_into()
                .expect("test vhost-user u32 should decode"),
        )
    }

    fn test_vhost_user_u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_ne_bytes(
            bytes[offset..offset + 8]
                .try_into()
                .expect("test vhost-user u64 should decode"),
        )
    }

    fn write_test_vhost_user_pipe(descriptor: &OwnedFd) {
        let notification = [0_u8; 8];
        // SAFETY: The received call fd is a live pipe writer and the complete
        // fixed notification buffer is readable for this synchronous write.
        let written = unsafe {
            libc::write(
                descriptor.as_raw_fd(),
                notification.as_ptr().cast(),
                notification.len(),
            )
        };
        assert_eq!(written, 8);
    }

    fn read_test_vhost_user_pipe(descriptor: &OwnedFd) {
        let mut poll_descriptor = libc::pollfd {
            fd: descriptor.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: One initialized poll entry is writable for this bounded wait.
        assert_eq!(unsafe { libc::poll(&raw mut poll_descriptor, 1, 2_000) }, 1);
        let mut notification = [1_u8; 8];
        // SAFETY: The received kick fd is a live pipe reader and the complete
        // fixed notification buffer is writable for this synchronous read.
        let read = unsafe {
            libc::read(
                descriptor.as_raw_fd(),
                notification.as_mut_ptr().cast(),
                notification.len(),
            )
        };
        assert_eq!(read, 8);
        assert_eq!(notification, [0; 8]);
    }

    fn spawn_complete_test_vhost_user_peer(
        mut stream: UnixStream,
        features: u64,
        config: [u8; VIRTIO_BLOCK_CONFIG_SIZE],
    ) -> thread::JoinHandle<TestVhostUserObservation> {
        thread::spawn(move || {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("peer read timeout should set");
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .expect("peer write timeout should set");

            let owner = expect_test_vhost_user_request(&mut stream, 3, false, 0);
            assert!(owner.body.is_empty());
            let get_features = expect_test_vhost_user_request(&mut stream, 1, false, 0);
            assert!(get_features.body.is_empty());
            send_test_vhost_user_reply(&mut stream, 1, &features.to_ne_bytes());
            let get_protocol = expect_test_vhost_user_request(&mut stream, 15, false, 0);
            assert!(get_protocol.body.is_empty());
            let protocol_features = VHOST_USER_PROTOCOL_F_CONFIG | VHOST_USER_PROTOCOL_F_REPLY_ACK;
            send_test_vhost_user_reply(&mut stream, 15, &protocol_features.to_ne_bytes());
            let set_protocol = expect_test_vhost_user_request(&mut stream, 16, false, 0);
            assert_eq!(
                test_vhost_user_u64(&set_protocol.body, 0),
                protocol_features
            );
            let get_config = expect_test_vhost_user_request(&mut stream, 24, false, 0);
            assert_eq!(get_config.body.len(), 12 + VIRTIO_BLOCK_CONFIG_SIZE);
            assert_eq!(test_vhost_user_u32(&get_config.body, 0), 0);
            assert_eq!(
                test_vhost_user_u32(&get_config.body, 4),
                VIRTIO_BLOCK_CONFIG_SIZE as u32
            );
            assert_eq!(test_vhost_user_u32(&get_config.body, 8), 1);
            assert!(get_config.body[12..].iter().all(|byte| *byte == 0));
            let mut config_reply = Vec::with_capacity(12 + VIRTIO_BLOCK_CONFIG_SIZE);
            config_reply.extend_from_slice(&0_u32.to_ne_bytes());
            config_reply.extend_from_slice(&(VIRTIO_BLOCK_CONFIG_SIZE as u32).to_ne_bytes());
            config_reply.extend_from_slice(&1_u32.to_ne_bytes());
            config_reply.extend_from_slice(&config);
            send_test_vhost_user_reply(&mut stream, 24, &config_reply);

            let set_features = expect_test_vhost_user_request(&mut stream, 2, true, 0);
            let guest_features = test_vhost_user_u64(&set_features.body, 0);
            assert_eq!(guest_features & !features, 0);
            assert_eq!(
                guest_features & (VIRTIO_F_VERSION_1 | VHOST_USER_F_PROTOCOL_FEATURES),
                VIRTIO_F_VERSION_1 | VHOST_USER_F_PROTOCOL_FEATURES
            );
            acknowledge_test_vhost_user_request(&mut stream, &set_features);

            let memory = receive_test_vhost_user_request(&mut stream);
            assert_eq!(memory.code, 5);
            assert!(memory.need_reply);
            let memory_region_count = test_vhost_user_u32(&memory.body, 0);
            assert_ne!(memory_region_count, 0);
            assert_eq!(test_vhost_user_u32(&memory.body, 4), 0);
            assert_eq!(memory.descriptors.len(), memory_region_count as usize);
            assert_eq!(memory.body.len(), 8 + memory.descriptors.len() * 32);
            acknowledge_test_vhost_user_request(&mut stream, &memory);

            let number = expect_test_vhost_user_request(&mut stream, 8, true, 0);
            assert_eq!(test_vhost_user_u32(&number.body, 0), 0);
            let queue_size = test_vhost_user_u32(&number.body, 4);
            acknowledge_test_vhost_user_request(&mut stream, &number);
            let address = expect_test_vhost_user_request(&mut stream, 9, true, 0);
            assert_eq!(address.body.len(), 40);
            assert_eq!(test_vhost_user_u32(&address.body, 0), 0);
            assert_eq!(test_vhost_user_u32(&address.body, 4), 0);
            for offset in [8, 16, 24] {
                assert_ne!(test_vhost_user_u64(&address.body, offset), 0);
            }
            assert_eq!(test_vhost_user_u64(&address.body, 32), 0);
            acknowledge_test_vhost_user_request(&mut stream, &address);
            let base = expect_test_vhost_user_request(&mut stream, 10, true, 0);
            assert_eq!(test_vhost_user_u32(&base.body, 0), 0);
            assert_eq!(test_vhost_user_u32(&base.body, 4), 0);
            acknowledge_test_vhost_user_request(&mut stream, &base);
            let call = expect_test_vhost_user_request(&mut stream, 13, true, 1);
            assert_eq!(test_vhost_user_u64(&call.body, 0), 0);
            acknowledge_test_vhost_user_request(&mut stream, &call);
            let kick = expect_test_vhost_user_request(&mut stream, 12, true, 1);
            assert_eq!(test_vhost_user_u64(&kick.body, 0), 0);
            acknowledge_test_vhost_user_request(&mut stream, &kick);
            let enable = expect_test_vhost_user_request(&mut stream, 18, true, 0);
            assert_eq!(test_vhost_user_u32(&enable.body, 0), 0);
            assert_eq!(test_vhost_user_u32(&enable.body, 4), 1);
            write_test_vhost_user_pipe(&call.descriptors[0]);
            acknowledge_test_vhost_user_request(&mut stream, &enable);
            read_test_vhost_user_pipe(&kick.descriptors[0]);

            TestVhostUserObservation {
                guest_features,
                memory_region_count,
                queue_size,
            }
        })
    }

    #[derive(Clone, Copy)]
    enum TestVhostUserConfigReply {
        Exact([u8; VIRTIO_BLOCK_CONFIG_SIZE]),
        BackendFailure,
        Malformed,
    }

    fn spawn_test_vhost_user_discovery_peer(
        mut stream: UnixStream,
        features: u64,
        protocol_features: u64,
        config_reply: TestVhostUserConfigReply,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("peer read timeout should set");
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .expect("peer write timeout should set");
            expect_test_vhost_user_request(&mut stream, 3, false, 0);
            expect_test_vhost_user_request(&mut stream, 1, false, 0);
            send_test_vhost_user_reply(&mut stream, 1, &features.to_ne_bytes());
            if features & (VIRTIO_F_VERSION_1 | VHOST_USER_F_PROTOCOL_FEATURES)
                != VIRTIO_F_VERSION_1 | VHOST_USER_F_PROTOCOL_FEATURES
            {
                return;
            }
            expect_test_vhost_user_request(&mut stream, 15, false, 0);
            send_test_vhost_user_reply(&mut stream, 15, &protocol_features.to_ne_bytes());
            if protocol_features & VHOST_USER_PROTOCOL_F_CONFIG == 0 {
                return;
            }
            let set_protocol = expect_test_vhost_user_request(&mut stream, 16, false, 0);
            assert_eq!(
                test_vhost_user_u64(&set_protocol.body, 0),
                VHOST_USER_PROTOCOL_F_CONFIG
                    | (protocol_features & VHOST_USER_PROTOCOL_F_REPLY_ACK)
            );
            expect_test_vhost_user_request(&mut stream, 24, false, 0);
            let mut reply = Vec::new();
            reply.extend_from_slice(&0_u32.to_ne_bytes());
            match config_reply {
                TestVhostUserConfigReply::Exact(bytes) => {
                    reply.extend_from_slice(&(VIRTIO_BLOCK_CONFIG_SIZE as u32).to_ne_bytes());
                    reply.extend_from_slice(&1_u32.to_ne_bytes());
                    reply.extend_from_slice(&bytes);
                }
                TestVhostUserConfigReply::BackendFailure => {
                    reply.extend_from_slice(&0_u32.to_ne_bytes());
                    reply.extend_from_slice(&1_u32.to_ne_bytes());
                }
                TestVhostUserConfigReply::Malformed => {
                    reply.extend_from_slice(&((VIRTIO_BLOCK_CONFIG_SIZE - 1) as u32).to_ne_bytes());
                    reply.extend_from_slice(&1_u32.to_ne_bytes());
                    reply.resize(12 + VIRTIO_BLOCK_CONFIG_SIZE - 1, 0x5a);
                }
            }
            send_test_vhost_user_reply(&mut stream, 24, &reply);
        })
    }

    fn spawn_test_vhost_user_refresh_peer(
        mut stream: UnixStream,
        initial_config: [u8; VIRTIO_BLOCK_CONFIG_SIZE],
        refreshed_config: TestVhostUserConfigReply,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("peer read timeout should set");
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .expect("peer write timeout should set");
            expect_test_vhost_user_request(&mut stream, 3, false, 0);
            expect_test_vhost_user_request(&mut stream, 1, false, 0);
            send_test_vhost_user_reply(&mut stream, 1, &SUPPORTED_VIRTIO_FEATURES.to_ne_bytes());
            expect_test_vhost_user_request(&mut stream, 15, false, 0);
            let protocol_features = VHOST_USER_PROTOCOL_F_CONFIG | VHOST_USER_PROTOCOL_F_REPLY_ACK;
            send_test_vhost_user_reply(&mut stream, 15, &protocol_features.to_ne_bytes());
            let set_protocol = expect_test_vhost_user_request(&mut stream, 16, false, 0);
            assert_eq!(
                test_vhost_user_u64(&set_protocol.body, 0),
                protocol_features
            );
            expect_test_vhost_user_request(&mut stream, 24, false, 0);
            let mut initial_reply = Vec::with_capacity(12 + VIRTIO_BLOCK_CONFIG_SIZE);
            initial_reply.extend_from_slice(&0_u32.to_ne_bytes());
            initial_reply.extend_from_slice(&(VIRTIO_BLOCK_CONFIG_SIZE as u32).to_ne_bytes());
            initial_reply.extend_from_slice(&1_u32.to_ne_bytes());
            initial_reply.extend_from_slice(&initial_config);
            send_test_vhost_user_reply(&mut stream, 24, &initial_reply);

            let set_features = expect_test_vhost_user_request(&mut stream, 2, true, 0);
            assert_eq!(
                test_vhost_user_u64(&set_features.body, 0) & !SUPPORTED_VIRTIO_FEATURES,
                0
            );
            acknowledge_test_vhost_user_request(&mut stream, &set_features);
            let memory = receive_test_vhost_user_request(&mut stream);
            assert_eq!(memory.code, 5);
            assert!(memory.need_reply);
            let memory_region_count = test_vhost_user_u32(&memory.body, 0);
            assert_ne!(memory_region_count, 0);
            assert_eq!(memory.descriptors.len(), memory_region_count as usize);
            acknowledge_test_vhost_user_request(&mut stream, &memory);
            for code in [8, 9, 10] {
                let request = expect_test_vhost_user_request(&mut stream, code, true, 0);
                acknowledge_test_vhost_user_request(&mut stream, &request);
            }
            for code in [13, 12] {
                let request = expect_test_vhost_user_request(&mut stream, code, true, 1);
                acknowledge_test_vhost_user_request(&mut stream, &request);
            }
            let enable = expect_test_vhost_user_request(&mut stream, 18, true, 0);
            assert_eq!(test_vhost_user_u32(&enable.body, 4), 1);
            acknowledge_test_vhost_user_request(&mut stream, &enable);

            expect_test_vhost_user_request(&mut stream, 24, false, 0);
            let mut refresh_reply = Vec::new();
            refresh_reply.extend_from_slice(&0_u32.to_ne_bytes());
            match refreshed_config {
                TestVhostUserConfigReply::Exact(bytes) => {
                    refresh_reply
                        .extend_from_slice(&(VIRTIO_BLOCK_CONFIG_SIZE as u32).to_ne_bytes());
                    refresh_reply.extend_from_slice(&1_u32.to_ne_bytes());
                    refresh_reply.extend_from_slice(&bytes);
                }
                TestVhostUserConfigReply::BackendFailure => {
                    refresh_reply.extend_from_slice(&0_u32.to_ne_bytes());
                    refresh_reply.extend_from_slice(&1_u32.to_ne_bytes());
                }
                TestVhostUserConfigReply::Malformed => {
                    refresh_reply
                        .extend_from_slice(&((VIRTIO_BLOCK_CONFIG_SIZE - 1) as u32).to_ne_bytes());
                    refresh_reply.extend_from_slice(&1_u32.to_ne_bytes());
                    refresh_reply.resize(12 + VIRTIO_BLOCK_CONFIG_SIZE - 1, 0x5a);
                }
            }
            send_test_vhost_user_reply(&mut stream, 24, &refresh_reply);
        })
    }

    fn vhost_user_refresh_device(
        refreshed_config: TestVhostUserConfigReply,
    ) -> (
        VirtioBlockConfigSpace,
        VirtioBlockDevice,
        GuestMemory,
        thread::JoinHandle<()>,
    ) {
        let (frontend_stream, peer_stream) =
            UnixStream::pair().expect("vhost-user stream pair should open");
        let mut initial_config = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        initial_config[..8].copy_from_slice(&1_u64.to_le_bytes());
        let peer =
            spawn_test_vhost_user_refresh_peer(peer_stream, initial_config, refreshed_config);
        let frontend = PreparedVhostUserBlockFrontend::discover(
            frontend_stream,
            DriveCacheType::Unsafe,
            Duration::from_secs(2),
        )
        .expect("vhost-user discovery should complete");
        let config = DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
            .with_socket("/private/test-vhost.sock")
            .validate()
            .expect("vhost-user drive should validate");
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test shared memory range should validate"),
        ])
        .expect("test shared memory layout should validate");
        let memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("test shared guest memory should allocate");
        let prepared = PreparedBlockDevice::from_config_with_vhost_user(&config, frontend, &memory)
            .expect("vhost-user block should prepare with shared memory");
        let (_, _, config_space, device) = prepared.into_parts();
        (config_space, device, memory, peer)
    }

    fn vhost_user_refresh_handler(
        refreshed_config: TestVhostUserConfigReply,
    ) -> (
        super::VirtioBlockMmioHandler,
        GuestMemory,
        thread::JoinHandle<()>,
    ) {
        let (config_space, device, memory, peer) = vhost_user_refresh_device(refreshed_config);
        let mut handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_BLOCK_DEVICE_ID,
            config_space.available_features(),
            &VIRTIO_BLOCK_QUEUE_SIZES,
            config_space,
            device,
        )
        .expect("vhost-user block handler should build");
        let guest_features = config_space.available_features() & !VHOST_USER_F_PROTOCOL_FEATURES;
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("vhost-user refresh handler should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("vhost-user refresh handler should accept DRIVER");
        for (selector, features) in [
            (0, guest_features as u32),
            (
                1,
                u32::try_from(guest_features >> 32).expect("high feature word should fit"),
            ),
        ] {
            handler
                .write_register(VirtioMmioRegister::DriverFeaturesSel, selector)
                .expect("vhost-user driver feature selector should write");
            handler
                .write_register(VirtioMmioRegister::DriverFeatures, features)
                .expect("vhost-user driver features should write");
        }
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("vhost-user refresh handler should accept FEATURES_OK");
        handler
            .write_register(VirtioMmioRegister::QueueNum, u32::from(TEST_QUEUE_SIZE))
            .expect("vhost-user queue size should write");
        for (register, address) in [
            (VirtioMmioRegister::QueueDescLow, TEST_DESCRIPTOR_TABLE),
            (VirtioMmioRegister::QueueDriverLow, TEST_AVAILABLE_RING),
            (VirtioMmioRegister::QueueDeviceLow, TEST_USED_RING),
        ] {
            handler
                .write_register(register, guest_address_low(address))
                .expect("vhost-user queue address should write");
        }
        handler
            .write_register(VirtioMmioRegister::QueueReady, 1)
            .expect("vhost-user queue should become ready");
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("vhost-user refresh handler should activate");
        (handler, memory, peer)
    }

    #[derive(Debug)]
    struct TestVhostUserPciRoute {
        message: GuestMessage,
        delivery_ambiguity: Option<bool>,
        signals: Arc<Mutex<usize>>,
    }

    impl GuestMessageInterrupt for TestVhostUserPciRoute {
        fn matches(&self, message: GuestMessage) -> bool {
            self.message == message
        }

        fn signal(&self, message: GuestMessage) -> Result<(), GuestMessageInterruptSignalError> {
            if !self.matches(message) {
                return Err(GuestMessageInterruptSignalError::new(
                    "test route rejected a mismatched message",
                    false,
                ));
            }
            if let Some(delivery_ambiguous) = self.delivery_ambiguity {
                return Err(GuestMessageInterruptSignalError::new(
                    "injected configuration interrupt failure",
                    delivery_ambiguous,
                ));
            }
            let mut signals = self
                .signals
                .lock()
                .expect("test signal count should remain available");
            *signals = signals.saturating_add(1);
            Ok(())
        }
    }

    fn vhost_user_refresh_pci_endpoint(
        refreshed_config: TestVhostUserConfigReply,
        delivery_ambiguity: Option<bool>,
    ) -> (
        VirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice>,
        GuestMemory,
        thread::JoinHandle<()>,
        Arc<Mutex<usize>>,
    ) {
        let (config_space, mut device, memory, peer) = vhost_user_refresh_device(refreshed_config);
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, true);
        let registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BLOCK_DEVICE_ID,
            config_space.available_features(),
        )
        .with_runtime_state(
            [0, 1],
            config_space.available_features() & !VHOST_USER_F_PROTOCOL_FEATURES,
            DRIVER_OK_STATUS,
        );
        device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect("PCI vhost-user refresh device should activate");
        let range = GuestMemoryRange::new(
            GuestAddress::new(0x4_0000_0000),
            VIRTIO_PCI_CAPABILITY_BAR_SIZE * 2,
        )
        .expect("test PCI BAR range should validate");
        let mut allocator = PciBarAllocator::new(PciBarAddressSpace::Memory64, range);
        let bar = allocator
            .allocate(VIRTIO_PCI_CAPABILITY_BAR_SIZE)
            .expect("test PCI BAR should allocate");
        let config_message = GuestMessage::new(0x0800_0040, 96);
        let queue_message = GuestMessage::new(0x0800_0040, 97);
        let signals = Arc::new(Mutex::new(0));
        let routes: Vec<Arc<dyn GuestMessageInterrupt>> = vec![
            Arc::new(TestVhostUserPciRoute {
                message: config_message,
                delivery_ambiguity,
                signals: Arc::clone(&signals),
            }),
            Arc::new(TestVhostUserPciRoute {
                message: queue_message,
                delivery_ambiguity: None,
                signals: Arc::new(Mutex::new(0)),
            }),
        ];
        let registry = GuestMessageInterruptRegistry::new(routes)
            .expect("test PCI message registry should validate");
        let endpoint = VirtioPciEndpoint::new(
            VirtioPciIdentity::new(
                VirtioDeviceType::new(VIRTIO_BLOCK_DEVICE_ID)
                    .expect("block device type should validate"),
                config_space.available_features(),
            ),
            &VIRTIO_BLOCK_QUEUE_SIZES,
            config_space,
            device,
            false,
            &bar,
            registry,
        )
        .expect("vhost-user PCI endpoint should initialize");
        let mut bus = MmioBus::new();
        bus.insert(
            MmioRegionId::new(77),
            bar.range().start(),
            bar.range().size(),
        )
        .expect("test PCI BAR should register");
        let mut handler = endpoint.bar_handler();
        let config_entry = VIRTIO_PCI_MSIX_TABLE_OFFSET;
        let address_access = bus
            .lookup(bar.range().start().checked_add(config_entry).unwrap(), 8)
            .expect("test config address write should resolve");
        handler
            .write(
                address_access,
                MmioAccessBytes::new(&config_message.address().to_le_bytes()).unwrap(),
            )
            .expect("test config message address should program");
        let data_access = bus
            .lookup(
                bar.range().start().checked_add(config_entry + 8).unwrap(),
                8,
            )
            .expect("test config data write should resolve");
        handler
            .write(
                data_access,
                MmioAccessBytes::new(&u64::from(config_message.data()).to_le_bytes()).unwrap(),
            )
            .expect("test config message data should program");
        let vector_access = bus
            .lookup(bar.range().start().checked_add(0x10).unwrap(), 2)
            .expect("test config vector write should resolve");
        handler
            .write(
                vector_access,
                MmioAccessBytes::new(&0_u16.to_le_bytes()).unwrap(),
            )
            .expect("test config vector should program");
        drop(handler);
        drop(bar);
        drop(allocator);
        (endpoint, memory, peer, signals)
    }

    fn vhost_user_pci_config_snapshot(
        endpoint: &VirtioPciEndpoint<VirtioBlockConfigSpace, VirtioBlockDevice>,
    ) -> (u32, VirtioBlockConfigSpace) {
        endpoint
            .admit_device_work()
            .expect("test PCI endpoint work should admit")
            .with_core_mut(|core| (core.device.config_generation(), core.device_config))
            .expect("test PCI endpoint state should remain available")
    }

    fn prepare_disconnected_test_vhost_user_device()
    -> (VirtioBlockConfigSpace, VirtioBlockDevice, GuestMemory) {
        let (frontend_stream, peer_stream) =
            UnixStream::pair().expect("vhost-user stream pair should open");
        let peer = spawn_test_vhost_user_discovery_peer(
            peer_stream,
            SUPPORTED_VIRTIO_FEATURES,
            VHOST_USER_PROTOCOL_F_CONFIG,
            TestVhostUserConfigReply::Exact([0; VIRTIO_BLOCK_CONFIG_SIZE]),
        );
        let frontend = PreparedVhostUserBlockFrontend::discover(
            frontend_stream,
            DriveCacheType::Writeback,
            Duration::from_secs(2),
        )
        .expect("vhost-user discovery should complete");
        peer.join().expect("discovery peer should finish");
        let config = DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
            .with_socket("/private/test-vhost.sock")
            .with_cache_type(DriveCacheType::Writeback)
            .validate()
            .expect("vhost-user drive should validate");
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test shared memory range should validate"),
        ])
        .expect("test shared memory layout should validate");
        let memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("test shared guest memory should allocate");
        let prepared = PreparedBlockDevice::from_config_with_vhost_user(&config, frontend, &memory)
            .expect("vhost-user block should prepare");
        let (_, _, config_space, device) = prepared.into_parts();
        (config_space, device, memory)
    }

    #[derive(Debug)]
    struct TempPath {
        path: PathBuf,
    }

    impl TempPath {
        fn as_path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            match fs::symlink_metadata(&self.path) {
                Ok(metadata) if metadata.is_dir() => {
                    let _ = fs::remove_dir_all(&self.path);
                }
                Ok(_) => {
                    let _ = fs::remove_file(&self.path);
                }
                Err(_) => {}
            }
        }
    }

    fn input() -> DriveConfigInput {
        DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs.ext4", false)
    }

    fn validate(input: DriveConfigInput) -> Result<DriveConfig, DriveConfigError> {
        input.validate()
    }

    fn temp_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_PATH_ID.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!(
            "bangbang-block-test-{}-{timestamp}-{id}-{name}",
            std::process::id(),
        ))
    }

    fn temp_file(name: &str, bytes: &[u8]) -> TempPath {
        let temp = TempPath {
            path: temp_path(name),
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temp.as_path())
            .expect("test file should be created");
        file.write_all(bytes).expect("test file should be written");
        temp
    }

    fn temp_dir(name: &str) -> TempPath {
        let temp = TempPath {
            path: temp_path(name),
        };
        fs::create_dir(temp.as_path()).expect("test directory should be created");
        temp
    }

    fn temp_fifo(name: &str) -> TempPath {
        let temp = TempPath {
            path: temp_path(name),
        };
        let c_path = CString::new(temp.as_path().as_os_str().as_bytes())
            .expect("test FIFO path should not contain NUL");

        // SAFETY: `c_path` is a NUL-terminated path built from the test path
        // and lives for the duration of the call.
        let result = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        if result != 0 {
            panic!(
                "test FIFO should be created: {}",
                io::Error::last_os_error()
            );
        }

        temp
    }

    fn temp_socket(name: &str) -> (TempPath, UnixListener) {
        let temp = TempPath {
            path: short_temp_path(name),
        };
        let listener =
            UnixListener::bind(temp.as_path()).expect("test Unix socket should be created");
        (temp, listener)
    }

    fn short_temp_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_PATH_ID.fetch_add(1, Ordering::Relaxed);
        let base = Path::new("/tmp");
        let dir = if base.is_dir() {
            base.to_path_buf()
        } else {
            std::env::temp_dir()
        };
        dir.join(format!("bb-blk-{}-{id}-{name}", std::process::id()))
    }

    fn missing_path(name: &str) -> PathBuf {
        temp_path(name)
    }

    fn config_for_path(path: impl Into<PathBuf>, is_read_only: bool) -> DriveConfig {
        config_for_drive("rootfs", path, false, is_read_only)
    }

    fn config_for_drive(
        drive_id: &str,
        path: impl Into<PathBuf>,
        is_root_device: bool,
        is_read_only: bool,
    ) -> DriveConfig {
        DriveConfigInput::new(drive_id, drive_id, path, is_root_device)
            .with_is_read_only(is_read_only)
            .validate()
            .expect("drive config should validate")
    }

    fn open_backing(
        path: impl Into<PathBuf>,
        is_read_only: bool,
    ) -> Result<BlockFileBacking, BlockFileBackingError> {
        BlockFileBacking::open(&config_for_path(path, is_read_only))
    }

    fn virtio_mmio_access(offset: u64, len: u64) -> MmioAccess {
        let mut bus = MmioBus::new();
        bus.insert(
            MmioRegionId::new(1),
            GuestAddress::new(TEST_MMIO_BASE),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("virtio-mmio region should insert");
        bus.lookup(GuestAddress::new(TEST_MMIO_BASE + offset), len)
            .expect("virtio-mmio access should resolve")
    }

    fn block_config_handler(
        config: VirtioBlockConfigSpace,
    ) -> VirtioMmioRegisterHandler<VirtioBlockConfigSpace> {
        VirtioMmioRegisterHandler::with_device_config(
            VIRTIO_BLOCK_DEVICE_ID,
            config.available_features(),
            &VIRTIO_BLOCK_QUEUE_SIZES,
            config,
        )
        .expect("block config handler should build")
    }

    fn block_notification_handler(
        backing: BlockFileBacking,
        queue_max_sizes: &[u16],
    ) -> VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice> {
        let config = VirtioBlockConfigSpace::from_backing(&backing, DriveCacheType::Unsafe);
        let device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID);
        VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_BLOCK_DEVICE_ID,
            config.available_features(),
            queue_max_sizes,
            config,
            device,
        )
        .expect("block notification handler should build")
    }

    fn configure_block_notification_handler_queue(
        handler: &mut VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice>,
        queue_size: u16,
        device_ring: GuestAddress,
    ) {
        configure_block_notification_handler_queue_with_features(
            handler,
            queue_size,
            device_ring,
            0,
        );
    }

    fn configure_block_notification_handler_queue_with_event_idx(
        handler: &mut VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice>,
        queue_size: u16,
        device_ring: GuestAddress,
    ) {
        configure_block_notification_handler_queue_with_features(
            handler,
            queue_size,
            device_ring,
            EVENT_IDX_DRIVER_FEATURE,
        );
    }

    fn configure_block_notification_handler_queue_with_features(
        handler: &mut VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice>,
        queue_size: u16,
        device_ring: GuestAddress,
        driver_features_low: u32,
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
        if driver_features_low != 0 {
            handler
                .write_register(VirtioMmioRegister::DriverFeaturesSel, 0)
                .expect("driver feature select should write");
            handler
                .write_register(VirtioMmioRegister::DriverFeatures, driver_features_low)
                .expect("driver features should write");
        }
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("status should accept FEATURES_OK");
        handler
            .write_register(VirtioMmioRegister::QueueNum, u32::from(queue_size))
            .expect("queue size should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(TEST_DESCRIPTOR_TABLE),
            )
            .expect("queue descriptor table should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(TEST_AVAILABLE_RING),
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

    fn activate_block_notification_handler(
        handler: &mut VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice>,
    ) {
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("DRIVER_OK should activate block device");
    }

    fn read_interrupt_status(
        handler: &VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice>,
    ) -> u32 {
        handler
            .read_register(VirtioMmioRegister::InterruptStatus)
            .expect("interrupt status should read")
    }

    fn acknowledge_queue_interrupt(
        handler: &mut VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice>,
    ) {
        handler
            .write_register(
                VirtioMmioRegister::InterruptAck,
                DeviceInterruptKind::Queue.status().bits(),
            )
            .expect("queue interrupt acknowledgement should write");
    }

    fn read_block_config(
        config: VirtioBlockConfigSpace,
        offset: u64,
        len: u64,
    ) -> Result<MmioAccessBytes, VirtioMmioRegisterHandlerError> {
        block_config_handler(config).read_access(virtio_mmio_access(
            VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset,
            len,
        ))
    }

    fn write_block_config_after_driver(
        config: VirtioBlockConfigSpace,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), VirtioMmioRegisterHandlerError> {
        let mut handler = block_config_handler(config);
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("status should accept ACKNOWLEDGE");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("status should accept DRIVER");
        handler.write_access(
            virtio_mmio_access(
                VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset,
                u64::try_from(bytes.len()).expect("test byte length should fit in u64"),
            ),
            MmioAccessBytes::new(bytes).expect("test write bytes should be valid"),
        )
    }

    fn dispatch_block_mmio_read(
        devices: &mut BlockMmioDevices,
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

    fn dispatch_block_mmio_read_u32(
        devices: &mut BlockMmioDevices,
        device_index: usize,
        offset: u64,
    ) -> u32 {
        let data = dispatch_block_mmio_read(devices, device_index, offset, 4);
        u32::from_le_bytes(
            data.as_slice()
                .try_into()
                .expect("test MMIO read should return 4 bytes"),
        )
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

    fn request_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test range should be valid"),
        ])
        .expect("test layout should be valid");
        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    fn write_request_header(
        memory: &mut GuestMemory,
        address: GuestAddress,
        request_type: u32,
        sector: u64,
    ) {
        let mut bytes = [0; VIRTIO_BLOCK_REQUEST_HEADER_SIZE as usize];
        let (request_type_bytes, tail) = bytes.split_at_mut(4);
        let (_reserved, sector_bytes) = tail.split_at_mut(4);
        request_type_bytes.copy_from_slice(&request_type.to_le_bytes());
        sector_bytes.copy_from_slice(&sector.to_le_bytes());
        memory
            .write_slice(&bytes, address)
            .expect("request header should write");
    }

    fn write_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        write_descriptor_at(memory, TEST_DESCRIPTOR_TABLE, index, descriptor);
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

    fn write_guest_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 field should write");
    }

    fn read_guest_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("u16 field should read");
        u16::from_le_bytes(bytes)
    }

    fn read_guest_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("u32 field should read");
        u32::from_le_bytes(bytes)
    }

    fn available_ring_idx_address() -> GuestAddress {
        TEST_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("available idx address should not overflow")
    }

    fn available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("available entry address should not overflow")
    }

    fn available_ring_used_event_address(queue_size: u16) -> GuestAddress {
        TEST_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(queue_size) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("available used-event address should not overflow")
    }

    fn used_ring_idx_address() -> GuestAddress {
        TEST_USED_RING
            .checked_add(TEST_USED_RING_IDX_OFFSET)
            .expect("used idx address should not overflow")
    }

    fn used_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_USED_RING
            .checked_add(
                TEST_USED_RING_RING_OFFSET + u64::from(ring_index) * TEST_USED_RING_ELEMENT_SIZE,
            )
            .expect("used entry address should not overflow")
    }

    fn used_ring_avail_event_address(queue_size: u16) -> GuestAddress {
        TEST_USED_RING
            .checked_add(
                TEST_USED_RING_RING_OFFSET + u64::from(queue_size) * TEST_USED_RING_ELEMENT_SIZE,
            )
            .expect("used avail-event address should not overflow")
    }

    fn write_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn write_available_used_event(memory: &mut GuestMemory, queue_size: u16, used_event: u16) {
        write_guest_u16(
            memory,
            available_ring_used_event_address(queue_size),
            used_event,
        );
    }

    fn read_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, used_ring_idx_address())
    }

    fn read_used_avail_event(memory: &GuestMemory, queue_size: u16) -> u16 {
        read_guest_u16(memory, used_ring_avail_event_address(queue_size))
    }

    fn read_used_element(memory: &GuestMemory, ring_index: u16) -> (u32, u32) {
        let entry = used_ring_entry_address(ring_index);
        let len_address = entry
            .checked_add(4)
            .expect("used element len address should not overflow");
        (
            read_guest_u32(memory, entry),
            read_guest_u32(memory, len_address),
        )
    }

    fn block_queue() -> VirtioBlockQueue {
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            TEST_AVAILABLE_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let used = VirtqueueUsedRing::new(TEST_USED_RING, TEST_QUEUE_SIZE)
            .expect("used ring should build");
        VirtioBlockQueue::new(available, used)
    }

    fn block_queue_with_indirect_descriptors() -> VirtioBlockQueue {
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            TEST_AVAILABLE_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build")
        .with_descriptor_chain_options(
            VirtqueueDescriptorChainOptions::new().with_indirect_descriptors(true),
        );
        let used = VirtqueueUsedRing::new(TEST_USED_RING, TEST_QUEUE_SIZE)
            .expect("used ring should build");
        VirtioBlockQueue::new(available, used)
    }

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test address should fit in queue low register")
    }

    fn configured_mmio_queue(size: u16, ready: bool) -> VirtioMmioQueueRegisters {
        configured_mmio_queue_with_device_ring(size, guest_address_low(TEST_USED_RING), 0, ready)
    }

    fn configured_mmio_queue_with_device_ring(
        size: u16,
        device_ring_low: u32,
        device_ring_high: u32,
        ready: bool,
    ) -> VirtioMmioQueueRegisters {
        let mut queues =
            VirtioMmioQueueRegisters::new(&[TEST_QUEUE_SIZE]).expect("queue table should build");
        queues
            .write_register(
                VirtioMmioRegister::QueueNum,
                u32::from(size),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue size should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(TEST_DESCRIPTOR_TABLE),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue descriptor table should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(TEST_AVAILABLE_RING),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue driver ring should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                device_ring_low,
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue device ring should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueDeviceHigh,
                device_ring_high,
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue device ring high should write");

        if ready {
            queues
                .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
                .expect("queue ready should write");
        }

        queues
    }

    fn block_device_registers() -> VirtioMmioDeviceRegisters {
        VirtioMmioDeviceRegisters::new(
            VIRTIO_BLOCK_DEVICE_ID,
            VirtioBlockConfigSpace::new(VIRTIO_BLOCK_SECTOR_SIZE, false, DriveCacheType::Unsafe)
                .available_features(),
        )
    }

    fn write_queued_request(
        memory: &mut GuestMemory,
        descriptor_head: u16,
        request_type: u32,
        sector: u64,
        header_address: GuestAddress,
        data: Option<(GuestAddress, u32, bool)>,
        status_address: GuestAddress,
    ) {
        write_request_header(memory, header_address, request_type, sector);
        let data_index = descriptor_head
            .checked_add(1)
            .expect("test data descriptor index should not overflow");
        let status_index = if data.is_some() {
            descriptor_head
                .checked_add(2)
                .expect("test status descriptor index should not overflow")
        } else {
            data_index
        };

        write_descriptor(
            memory,
            descriptor_head,
            TestDescriptor::readable(
                header_address,
                VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
                Some(data_index),
            ),
        );
        if let Some((data_address, data_len, is_write_only)) = data {
            let descriptor = if is_write_only {
                TestDescriptor::writable(data_address, data_len, Some(status_index))
            } else {
                TestDescriptor::readable(data_address, data_len, Some(status_index))
            };
            write_descriptor(memory, data_index, descriptor);
        }
        write_descriptor(
            memory,
            status_index,
            TestDescriptor::writable(status_address, VIRTIO_BLOCK_STATUS_SIZE, None),
        );
    }

    fn request_chain(
        memory: &mut GuestMemory,
        request_type: u32,
        sector: u64,
        descriptors: &[TestDescriptor],
    ) -> VirtqueueDescriptorChain {
        write_request_header(memory, HEADER_ADDR, request_type, sector);
        for (index, descriptor) in descriptors.iter().copied().enumerate() {
            write_descriptor(
                memory,
                u16::try_from(index).expect("test descriptor index should fit in u16"),
                descriptor,
            );
        }
        read_descriptor_chain(memory, TEST_DESCRIPTOR_TABLE, TEST_QUEUE_SIZE, 0)
            .expect("descriptor chain should read")
    }

    fn parse_request(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<VirtioBlockRequest, VirtioBlockRequestError> {
        VirtioBlockRequest::parse(memory, chain, 16)
    }

    fn parse_request_with_capacity(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
        backing: &BlockFileBacking,
    ) -> VirtioBlockRequest {
        VirtioBlockRequest::parse(memory, chain, backing.len() >> VIRTIO_BLOCK_SECTOR_SHIFT)
            .expect("request should parse for backing capacity")
    }

    fn read_guest_bytes(memory: &GuestMemory, address: GuestAddress, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest bytes should read");
        bytes
    }

    fn read_status(memory: &GuestMemory) -> u8 {
        let mut status = [0];
        memory
            .read_slice(&mut status, STATUS_ADDR)
            .expect("status should read");
        status[0]
    }

    fn sector_payload(byte: u8) -> Vec<u8> {
        vec![byte; VIRTIO_BLOCK_SECTOR_SIZE as usize]
    }

    #[test]
    fn accepts_minimal_drive_config_with_defaults() {
        let config = validate(input()).expect("minimal drive config should be valid");

        assert_eq!(config.drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), Some(Path::new("/tmp/rootfs.ext4")));
        assert!(!config.is_root_device());
        assert_eq!(config.is_read_only(), Some(false));
        assert_eq!(config.partuuid(), None);
        assert_eq!(config.cache_type(), DriveCacheType::Unsafe);
        assert_eq!(config.io_engine(), Some(DriveIoEngine::Sync));
        assert_eq!(config.rate_limiter(), None);
    }

    #[test]
    fn accepts_read_only_root_drive_with_partuuid() {
        let config = validate(
            DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs.ext4", true)
                .with_is_read_only(true)
                .with_partuuid("0eaa91a0-01"),
        )
        .expect("root drive config should be valid");

        assert!(config.is_root_device());
        assert_eq!(config.is_read_only(), Some(true));
        assert_eq!(config.partuuid(), Some("0eaa91a0-01"));
    }

    #[test]
    fn accepts_firecracker_id_character_set() {
        let id = "root_\u{00e9}1";
        let config = validate(DriveConfigInput::new(id, id, "/tmp/rootfs.ext4", false))
            .expect("Firecracker-compatible drive id should be valid");

        assert_eq!(config.drive_id(), id);
    }

    #[test]
    fn rejects_empty_drive_ids() {
        assert_eq!(
            validate(DriveConfigInput::new(
                "",
                "rootfs",
                "/tmp/rootfs.ext4",
                false
            )),
            Err(DriveConfigError::EmptyDriveId {
                source: DriveIdSource::Path,
            })
        );
        assert_eq!(
            validate(DriveConfigInput::new(
                "rootfs",
                "",
                "/tmp/rootfs.ext4",
                false
            )),
            Err(DriveConfigError::EmptyDriveId {
                source: DriveIdSource::Body,
            })
        );
    }

    #[test]
    fn rejects_invalid_drive_ids_without_echoing_them() {
        let invalid = "bad/id\nsecret";
        let err = validate(DriveConfigInput::new(
            invalid,
            invalid,
            "/tmp/rootfs.ext4",
            false,
        ))
        .expect_err("invalid id should fail");

        assert_eq!(
            err,
            DriveConfigError::InvalidDriveId {
                source: DriveIdSource::Path,
                drive_id: invalid.to_string(),
            }
        );
        assert_eq!(
            err.to_string(),
            "path drive_id must contain only alphanumeric characters or '_'"
        );
        assert!(!err.to_string().contains(invalid));

        let err = validate(DriveConfigInput::new(
            "rootfs",
            invalid,
            "/tmp/rootfs.ext4",
            false,
        ))
        .expect_err("invalid body id should fail");
        assert_eq!(
            err,
            DriveConfigError::InvalidDriveId {
                source: DriveIdSource::Body,
                drive_id: invalid.to_string(),
            }
        );
        assert_eq!(
            err.to_string(),
            "body drive_id must contain only alphanumeric characters or '_'"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn rejects_mismatched_drive_ids_without_echoing_them() {
        let err = validate(DriveConfigInput::new(
            "rootfs",
            "scratch",
            "/tmp/rootfs.ext4",
            false,
        ))
        .expect_err("mismatched ids should fail");

        assert_eq!(
            err,
            DriveConfigError::MismatchedDriveId {
                path_drive_id: "rootfs".to_string(),
                body_drive_id: "scratch".to_string(),
            }
        );
        assert_eq!(err.to_string(), "path drive_id must match body drive_id");
        assert!(!err.to_string().contains("scratch"));
    }

    #[test]
    fn rejects_empty_path_on_host_without_echoing_paths() {
        let err = validate(DriveConfigInput::new(
            "rootfs",
            "rootfs",
            PathBuf::new(),
            false,
        ))
        .expect_err("empty host path should fail");

        assert_eq!(err, DriveConfigError::EmptyPathOnHost);
        assert_eq!(err.to_string(), "drive path_on_host must not be empty");
    }

    #[test]
    fn rejects_missing_path_on_host_without_socket() {
        let err = validate(DriveConfigInput::new_without_path_on_host(
            "rootfs", "rootfs", false,
        ))
        .expect_err("missing host path should fail");

        assert_eq!(err, DriveConfigError::EmptyPathOnHost);
        assert_eq!(err.to_string(), "drive path_on_host must not be empty");
    }

    #[test]
    fn accepts_writeback_cache_type() {
        let config = validate(input().with_cache_type(DriveCacheType::Writeback))
            .expect("Writeback cache should be supported");

        assert_eq!(config.cache_type(), DriveCacheType::Writeback);
    }

    #[test]
    fn accepts_async_io_engine() {
        let config = validate(input().with_io_engine(DriveIoEngine::Async))
            .expect("Async I/O engine should be supported");

        assert_eq!(config.io_engine(), Some(DriveIoEngine::Async));
    }

    #[test]
    fn accepts_configured_rate_limiter() {
        let rate_limiter = DriveRateLimiterConfig::new(
            Some(DriveTokenBucketConfig::new(1024, Some(2048), 100)),
            Some(DriveTokenBucketConfig::new(10, None, 1000)),
        );
        let config = validate(input().with_rate_limiter(rate_limiter))
            .expect("configured drive rate limiter should be stored");

        assert_eq!(config.rate_limiter(), Some(rate_limiter));
    }

    #[test]
    fn drops_unconfigured_rate_limiter() {
        let input = input().with_rate_limiter(DriveRateLimiterConfig::new(None, None));
        assert!(!input.rate_limiter_configured());

        let config =
            validate(input).expect("empty rate limiter should be accepted as unconfigured");

        assert_eq!(config.rate_limiter(), None);
    }

    #[test]
    fn rejects_socket_together_with_path_on_host() {
        assert_eq!(
            validate(input().with_socket("/tmp/vhost-user-block.sock")),
            Err(DriveConfigError::InvalidVhostUserConfiguration)
        );
    }

    #[test]
    fn accepts_socket_field_without_file_backed_fields() {
        let config = validate(
            DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
                .with_socket("/tmp/private-vhost.sock"),
        )
        .expect("socket-backed drive should be supported");

        assert_eq!(config.drive_id(), "vhost");
        assert_eq!(config.path_on_host(), None);
        assert_eq!(config.socket(), Some(Path::new("/tmp/private-vhost.sock")));
        assert!(config.is_vhost_user());
        assert_eq!(config.is_read_only(), None);
        assert_eq!(config.io_engine(), None);
        assert_eq!(config.rate_limiter(), None);
    }

    #[test]
    fn rejects_vhost_user_with_explicit_file_backed_fields() {
        for input in [
            DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
                .with_socket("/tmp/private-vhost.sock")
                .with_is_read_only(false),
            DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
                .with_socket("/tmp/private-vhost.sock")
                .with_io_engine(DriveIoEngine::Sync),
            DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
                .with_socket("/tmp/private-vhost.sock")
                .with_rate_limiter(DriveRateLimiterConfig::new(None, None)),
        ] {
            let err = validate(input).expect_err("file-backed field should be rejected");

            assert_eq!(err, DriveConfigError::InvalidVhostUserConfiguration);
            assert!(!err.to_string().contains("private-vhost"));
        }
    }

    #[test]
    fn drive_config_input_exposes_firecracker_shape() {
        let input = input()
            .with_is_read_only(false)
            .with_partuuid("part")
            .with_cache_type(DriveCacheType::Unsafe)
            .with_io_engine(DriveIoEngine::Sync);

        assert_eq!(input.path_drive_id(), "rootfs");
        assert_eq!(input.body_drive_id(), "rootfs");
        assert_eq!(input.path_on_host(), Some(Path::new("/tmp/rootfs.ext4")));
        assert!(!input.is_root_device());
        assert_eq!(input.is_read_only(), Some(false));
        assert_eq!(input.partuuid(), Some("part"));
        assert_eq!(input.cache_type(), Some(DriveCacheType::Unsafe));
        assert_eq!(input.io_engine(), Some(DriveIoEngine::Sync));
        assert_eq!(input.rate_limiter(), None);
        assert!(!input.rate_limiter_configured());
        assert_eq!(input.socket(), None);
    }

    #[test]
    fn drive_config_errors_display_and_preserve_sources() {
        let err = DriveConfigError::InvalidVhostUserConfiguration;

        assert_eq!(
            err.to_string(),
            "vhost-user drive contains incompatible file-backed fields"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn drive_configs_insert_validated_config() {
        let mut configs = DriveConfigs::new();

        configs
            .insert(input())
            .expect("minimal drive config should be stored");

        assert_eq!(configs.as_slice().len(), 1);
        let config = &configs.as_slice()[0];
        assert_eq!(config.drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), Some(Path::new("/tmp/rootfs.ext4")));
    }

    #[test]
    fn drive_configs_replace_duplicate_drive_id() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(input())
            .expect("initial drive config should be stored");

        configs
            .insert(
                DriveConfigInput::new("rootfs", "rootfs", "/tmp/replaced.ext4", true)
                    .with_is_read_only(true),
            )
            .expect("duplicate drive id should replace existing config");

        assert_eq!(configs.as_slice().len(), 1);
        let config = &configs.as_slice()[0];
        assert_eq!(config.drive_id(), "rootfs");
        assert_eq!(config.path_on_host(), Some(Path::new("/tmp/replaced.ext4")));
        assert!(config.is_root_device());
        assert_eq!(config.is_read_only(), Some(true));
    }

    #[test]
    fn drive_configs_preserve_non_root_order_when_replacing_existing_drive() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))
            .expect("root drive config should be stored");
        configs
            .insert(DriveConfigInput::new(
                "data1",
                "data1",
                "/tmp/data1.ext4",
                false,
            ))
            .expect("first data drive config should be stored");
        configs
            .insert(DriveConfigInput::new(
                "data2",
                "data2",
                "/tmp/data2.ext4",
                false,
            ))
            .expect("second data drive config should be stored");

        configs
            .insert(DriveConfigInput::new(
                "data1",
                "data1",
                "/tmp/data1-replaced.ext4",
                false,
            ))
            .expect("replacement data drive config should be stored");

        assert_eq!(configs.as_slice().len(), 3);
        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
        assert_eq!(configs.as_slice()[1].drive_id(), "data1");
        assert_eq!(
            configs.as_slice()[1].path_on_host(),
            Some(Path::new("/tmp/data1-replaced.ext4"))
        );
        assert_eq!(configs.as_slice()[2].drive_id(), "data2");
    }

    #[test]
    fn drive_configs_keep_root_first_when_replacing_existing_root() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))
            .expect("root drive config should be stored");
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                "/tmp/data.ext4",
                false,
            ))
            .expect("data drive config should be stored");

        configs
            .insert(
                DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs-new.ext4", true)
                    .with_is_read_only(true),
            )
            .expect("replacement root drive config should be stored");

        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
        assert_eq!(
            configs.as_slice()[0].path_on_host(),
            Some(Path::new("/tmp/rootfs-new.ext4"))
        );
        assert!(configs.as_slice()[0].is_root_device());
        assert_eq!(configs.as_slice()[0].is_read_only(), Some(true));
        assert_eq!(configs.as_slice()[1].drive_id(), "data");
    }

    #[test]
    fn drive_configs_preserve_root_slot_when_replacing_root_as_non_root() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))
            .expect("root drive config should be stored");
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                "/tmp/data.ext4",
                false,
            ))
            .expect("data drive config should be stored");

        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs-data.ext4",
                false,
            ))
            .expect("replacement non-root drive config should be stored");

        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
        assert_eq!(
            configs.as_slice()[0].path_on_host(),
            Some(Path::new("/tmp/rootfs-data.ext4"))
        );
        assert!(!configs.as_slice()[0].is_root_device());
        assert_eq!(configs.as_slice()[1].drive_id(), "data");
    }

    #[test]
    fn drive_configs_move_existing_drive_to_front_when_promoted_to_root() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "data1",
                "data1",
                "/tmp/data1.ext4",
                false,
            ))
            .expect("first data drive config should be stored");
        configs
            .insert(DriveConfigInput::new(
                "data2",
                "data2",
                "/tmp/data2.ext4",
                false,
            ))
            .expect("second data drive config should be stored");

        configs
            .insert(DriveConfigInput::new(
                "data2",
                "data2",
                "/tmp/data2-root.ext4",
                true,
            ))
            .expect("promoted root drive config should be stored");

        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].drive_id(), "data2");
        assert_eq!(
            configs.as_slice()[0].path_on_host(),
            Some(Path::new("/tmp/data2-root.ext4"))
        );
        assert!(configs.as_slice()[0].is_root_device());
        assert_eq!(configs.as_slice()[1].drive_id(), "data1");
    }

    #[test]
    fn drive_configs_keep_root_drive_first() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                "/tmp/data.ext4",
                false,
            ))
            .expect("data drive config should be stored");
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))
            .expect("root drive config should be stored");

        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
        assert_eq!(configs.as_slice()[1].drive_id(), "data");
    }

    #[test]
    fn drive_configs_reject_second_root_without_mutating() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))
            .expect("root drive config should be stored");

        let err = configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                "/tmp/data.ext4",
                true,
            ))
            .expect_err("second root drive should fail");

        assert_eq!(err, DriveConfigError::RootDeviceAlreadyConfigured);
        assert_eq!(err.to_string(), "a root drive is already configured");
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
    }

    #[test]
    fn drive_configs_reject_invalid_input_without_mutating() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(input())
            .expect("initial drive config should be stored");

        let err = configs
            .insert(DriveConfigInput::new("data", "data", PathBuf::new(), false))
            .expect_err("invalid drive config should fail");

        assert_eq!(err, DriveConfigError::EmptyPathOnHost);
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
    }

    #[test]
    fn runtime_drive_insert_is_duplicate_only_and_commits_after_preparation() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))
            .expect("root drive should configure");

        let prepared = configs
            .prepare_runtime_insert(DriveConfigInput::new(
                "data",
                "data",
                "/tmp/data.ext4",
                false,
            ))
            .expect("runtime data drive should prepare");
        assert_eq!(prepared.config().drive_id(), "data");
        assert_eq!(configs.as_slice().len(), 1);
        configs.commit_runtime_insert(prepared);
        assert_eq!(configs.as_slice().len(), 2);

        assert!(matches!(
            configs.prepare_runtime_insert(DriveConfigInput::new(
                "data",
                "data",
                "/tmp/replacement.ext4",
                false,
            )),
            Err(DriveRuntimeMutationError::DuplicateDrive { drive_id }) if drive_id == "data"
        ));
        assert!(matches!(
            configs.prepare_runtime_insert(DriveConfigInput::new(
                "late_root",
                "late_root",
                "/tmp/root.ext4",
                true,
            )),
            Err(DriveRuntimeMutationError::RootInsertUnsupported)
        ));
        assert_eq!(configs.as_slice().len(), 2);
    }

    #[test]
    fn runtime_drive_replacement_preserves_identity_and_allows_engine_change() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(
                DriveConfigInput::new("data", "data", "/tmp/data.ext4", false)
                    .with_is_read_only(false)
                    .with_partuuid("part-a")
                    .with_cache_type(DriveCacheType::Writeback),
            )
            .expect("initial drive should configure");
        let replacement = DriveConfigInput::new("data", "data", "/tmp/replacement.ext4", false)
            .with_is_read_only(false)
            .with_partuuid("part-a")
            .with_cache_type(DriveCacheType::Writeback)
            .with_io_engine(DriveIoEngine::Async)
            .with_rate_limiter_configured()
            .validate()
            .expect("replacement should validate");

        let replacement = configs
            .validate_runtime_replacement(replacement)
            .expect("stable identity replacement should prepare");
        assert_eq!(replacement.io_engine(), Some(DriveIoEngine::Async));
        assert!(replacement.rate_limiter().is_some());
        assert_eq!(
            configs.as_slice()[0].path_on_host(),
            Some(Path::new("/tmp/data.ext4"))
        );

        for (input, field) in [
            (
                DriveConfigInput::new("data", "data", "/tmp/new.ext4", true)
                    .with_is_read_only(false)
                    .with_partuuid("part-a")
                    .with_cache_type(DriveCacheType::Writeback),
                DriveReplacementIdentityField::RootDevice,
            ),
            (
                DriveConfigInput::new("data", "data", "/tmp/new.ext4", false)
                    .with_is_read_only(true)
                    .with_partuuid("part-a")
                    .with_cache_type(DriveCacheType::Writeback),
                DriveReplacementIdentityField::ReadOnly,
            ),
            (
                DriveConfigInput::new("data", "data", "/tmp/new.ext4", false)
                    .with_is_read_only(false)
                    .with_partuuid("part-b")
                    .with_cache_type(DriveCacheType::Writeback),
                DriveReplacementIdentityField::Partuuid,
            ),
            (
                DriveConfigInput::new("data", "data", "/tmp/new.ext4", false)
                    .with_is_read_only(false)
                    .with_partuuid("part-a")
                    .with_cache_type(DriveCacheType::Unsafe),
                DriveReplacementIdentityField::CacheType,
            ),
        ] {
            assert_eq!(
                configs.validate_runtime_replacement(
                    input.validate().expect("candidate should validate")
                ),
                Err(DriveUpdateError::ReplacementIdentityMismatch {
                    drive_id: "data".to_string(),
                    field,
                })
            );
        }
    }

    #[test]
    fn runtime_drive_removal_rejects_root_and_missing_then_commits_exact_data_drive() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))
            .expect("root drive should configure");
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                "/tmp/data.ext4",
                false,
            ))
            .expect("data drive should configure");

        assert!(matches!(
            configs.prepare_runtime_removal("missing"),
            Err(DriveRuntimeMutationError::UnknownDrive { drive_id }) if drive_id == "missing"
        ));
        assert!(matches!(
            configs.prepare_runtime_removal("rootfs"),
            Err(DriveRuntimeMutationError::RootRemovalUnsupported { drive_id })
                if drive_id == "rootfs"
        ));
        let prepared = configs
            .prepare_runtime_removal("data")
            .expect("data removal should prepare");
        assert_eq!(configs.as_slice().len(), 2);
        configs.commit_runtime_removal(prepared);
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
    }

    #[test]
    fn runtime_drive_insert_and_removal_accept_non_root_vhost_user_backend() {
        let mut configs = DriveConfigs::new();
        let prepared = configs
            .prepare_runtime_insert(
                DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
                    .with_socket("/private/vhost.sock"),
            )
            .expect("vhost-user runtime insertion should prepare");
        assert!(prepared.config().is_vhost_user());
        assert!(configs.as_slice().is_empty());
        configs.commit_runtime_insert(prepared);

        let removal = configs
            .prepare_runtime_removal("vhost")
            .expect("vhost-user runtime removal should prepare");
        assert_eq!(configs.as_slice().len(), 1);
        assert!(configs.as_slice()[0].is_vhost_user());
        configs.commit_runtime_removal(removal);
        assert!(configs.as_slice().is_empty());
    }

    #[test]
    fn runtime_drive_mutation_diagnostics_redact_rejected_ids_and_backing_paths() {
        let sensitive_id = "private_drive_1420";
        for error in [
            DriveRuntimeMutationError::InvalidConfig(DriveConfigError::InvalidDriveId {
                source: DriveIdSource::Path,
                drive_id: sensitive_id.to_string(),
            }),
            DriveRuntimeMutationError::DuplicateDrive {
                drive_id: sensitive_id.to_string(),
            },
            DriveRuntimeMutationError::RootRemovalUnsupported {
                drive_id: sensitive_id.to_string(),
            },
            DriveRuntimeMutationError::UnknownDrive {
                drive_id: sensitive_id.to_string(),
            },
        ] {
            assert!(!error.to_string().contains(sensitive_id));
        }

        let path = missing_path("private-runtime-backing-1420.img");
        let config = DriveConfigInput::new(sensitive_id, sensitive_id, &path, false)
            .validate()
            .expect("runtime drive config should validate before backing preparation");
        let error = PreparedBlockDevice::from_config_with_backing(&config, None)
            .expect_err("missing runtime backing should fail preparation");
        let rendered = error.to_string();
        assert!(!rendered.contains(sensitive_id));
        assert!(!rendered.contains("private-runtime-backing-1420"));
    }

    #[test]
    fn drive_update_input_validates_ids_and_empty_path() {
        let unicode_update = DriveUpdateInput::new("数据_1", "数据_1", None)
            .validate()
            .expect("drive update ID validation should match initial config validation");
        assert_eq!(unicode_update.drive_id(), "数据_1");

        assert_eq!(
            DriveUpdateInput::new("", "", None).validate(),
            Err(DriveUpdateError::EmptyDriveId {
                source: DriveIdSource::Path
            })
        );
        assert_eq!(
            DriveUpdateInput::new("rootfs", "", None).validate(),
            Err(DriveUpdateError::EmptyDriveId {
                source: DriveIdSource::Body
            })
        );
        assert_eq!(
            DriveUpdateInput::new("rootfs", "root-fs", None).validate(),
            Err(DriveUpdateError::InvalidDriveId {
                source: DriveIdSource::Body,
                drive_id: "root-fs".to_string(),
            })
        );
        assert_eq!(
            DriveUpdateInput::new("rootfs", "data", None).validate(),
            Err(DriveUpdateError::MismatchedDriveId {
                path_drive_id: "rootfs".to_string(),
                body_drive_id: "data".to_string(),
            })
        );
        assert_eq!(
            DriveUpdateInput::new("rootfs", "rootfs", Some(PathBuf::new())).validate(),
            Err(DriveUpdateError::EmptyPathOnHost)
        );

        let rate_limiter =
            DriveRateLimiterConfig::new(None, Some(DriveTokenBucketConfig::new(1, None, 1)));
        let rate_limited_update =
            DriveUpdateInput::new("rootfs", "rootfs", None).with_rate_limiter(rate_limiter);
        assert!(rate_limited_update.rate_limiter_configured());
        let update = rate_limited_update
            .validate()
            .expect("rate-limited drive update should validate");
        assert_eq!(update.rate_limiter(), Some(rate_limiter));
    }

    #[test]
    fn drive_update_error_displays_active_session_command_failure() {
        let err = DriveUpdateError::ActiveSessionCommand {
            message: "boot run loop command queue is full".to_string(),
        };

        assert_eq!(
            err.to_string(),
            "active drive update command failed: boot run loop command queue is full"
        );
    }

    #[test]
    fn drive_configs_build_and_commit_runtime_update() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(
                DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs.ext4", true)
                    .with_is_read_only(true),
            )
            .expect("root drive config should insert");
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                "/tmp/data.ext4",
                false,
            ))
            .expect("data drive config should insert");

        let updated = configs
            .updated_config(DriveUpdateInput::new(
                "data",
                "data",
                Some(PathBuf::from("/tmp/data-updated.ext4")),
            ))
            .expect("runtime drive update should build");

        assert_eq!(updated.drive_id(), "data");
        assert_eq!(
            updated.path_on_host(),
            Some(Path::new("/tmp/data-updated.ext4"))
        );
        assert!(!updated.is_root_device());
        configs
            .commit_update(updated)
            .expect("runtime drive update should commit");
        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
        assert_eq!(
            configs.as_slice()[0].path_on_host(),
            Some(Path::new("/tmp/rootfs.ext4"))
        );
        assert_eq!(configs.as_slice()[1].drive_id(), "data");
        assert_eq!(
            configs.as_slice()[1].path_on_host(),
            Some(Path::new("/tmp/data-updated.ext4"))
        );
    }

    #[test]
    fn drive_configs_update_without_path_keeps_existing_path() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))
            .expect("root drive config should insert");

        let updated = configs
            .updated_config(DriveUpdateInput::new("rootfs", "rootfs", None))
            .expect("pathless runtime drive update should build");

        assert_eq!(updated.path_on_host(), Some(Path::new("/tmp/rootfs.ext4")));
    }

    #[test]
    fn drive_configs_accept_only_id_only_vhost_user_update() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(
                DriveConfigInput::new_without_path_on_host("scratch", "scratch", false)
                    .with_socket("/private/scratch.sock"),
            )
            .expect("vhost-user drive config should insert");

        let updated = configs
            .updated_config(DriveUpdateInput::new("scratch", "scratch", None))
            .expect("ID-only vhost-user update should build");
        assert_eq!(updated, configs.as_slice()[0]);
        assert!(updated.is_vhost_user());

        assert_eq!(
            configs.updated_config(DriveUpdateInput::new(
                "scratch",
                "scratch",
                Some(PathBuf::from("/tmp/not-a-vhost-update")),
            )),
            Err(DriveUpdateError::UnsupportedBackend)
        );
        assert_eq!(
            configs.updated_config(
                DriveUpdateInput::new("scratch", "scratch", None).with_rate_limiter(
                    DriveRateLimiterConfig::new(
                        None,
                        Some(DriveTokenBucketConfig::new(1, None, 1)),
                    ),
                ),
            ),
            Err(DriveUpdateError::UnsupportedBackend)
        );
        assert_eq!(
            DriveUpdateError::UnsupportedBackend.to_string(),
            "file-backed drive updates are unsupported for vhost-user drives"
        );
    }

    #[test]
    fn drive_configs_runtime_rate_limiter_update_preserves_missing_bucket() {
        let mut configs = DriveConfigs::new();
        let bandwidth = DriveTokenBucketConfig::new(1024, Some(2048), 100);
        configs
            .insert(
                DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs.ext4", true)
                    .with_rate_limiter(DriveRateLimiterConfig::new(Some(bandwidth), None)),
            )
            .expect("root drive config should insert");

        let ops = DriveTokenBucketConfig::new(10, None, 1000);
        let updated = configs
            .updated_config(
                DriveUpdateInput::new("rootfs", "rootfs", None)
                    .with_rate_limiter(DriveRateLimiterConfig::new(None, Some(ops))),
            )
            .expect("runtime drive limiter update should build");

        assert_eq!(
            updated.rate_limiter(),
            Some(DriveRateLimiterConfig::new(Some(bandwidth), Some(ops)))
        );
    }

    #[test]
    fn drive_configs_runtime_rate_limiter_update_clears_disabled_bucket() {
        let mut configs = DriveConfigs::new();
        let bandwidth = DriveTokenBucketConfig::new(1024, Some(2048), 100);
        let ops = DriveTokenBucketConfig::new(10, None, 1000);
        configs
            .insert(
                DriveConfigInput::new("rootfs", "rootfs", "/tmp/rootfs.ext4", true)
                    .with_rate_limiter(DriveRateLimiterConfig::new(Some(bandwidth), Some(ops))),
            )
            .expect("root drive config should insert");

        let updated = configs
            .updated_config(
                DriveUpdateInput::new("rootfs", "rootfs", None).with_rate_limiter(
                    DriveRateLimiterConfig::new(
                        Some(DriveTokenBucketConfig::new(0, None, 100)),
                        None,
                    ),
                ),
            )
            .expect("runtime drive limiter update should build");

        assert_eq!(
            updated.rate_limiter(),
            Some(DriveRateLimiterConfig::new(None, Some(ops)))
        );
    }

    #[test]
    fn drive_configs_reject_unknown_runtime_update_without_mutation() {
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                "/tmp/rootfs.ext4",
                true,
            ))
            .expect("root drive config should insert");

        let err = configs
            .updated_config(DriveUpdateInput::new(
                "missing",
                "missing",
                Some(PathBuf::from("/tmp/missing.ext4")),
            ))
            .expect_err("unknown runtime drive should fail");

        assert_eq!(
            err,
            DriveUpdateError::UnknownDrive {
                drive_id: "missing".to_string()
            }
        );
        assert_eq!(configs.as_slice().len(), 1);
        assert_eq!(
            configs.as_slice()[0].path_on_host(),
            Some(Path::new("/tmp/rootfs.ext4"))
        );
    }

    #[test]
    fn prepared_block_devices_accept_empty_configs() {
        let configs = DriveConfigs::new();

        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("empty configs should prepare");

        assert!(prepared.is_empty());
        assert_eq!(prepared.len(), 0);
        assert!(prepared.into_vec().is_empty());
    }

    #[test]
    fn prepared_block_devices_prepare_read_only_block_device() {
        let file = temp_file("prepared-ro.img", &[0; 1024]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(
                DriveConfigInput::new("rootfs", "rootfs", file.as_path(), true)
                    .with_is_read_only(true),
            )
            .expect("root drive config should insert");

        let prepared = PreparedBlockDevices::from_configs(&configs)
            .expect("read-only block device should prepare");

        assert_eq!(prepared.len(), 1);
        let device = &prepared.as_slice()[0];
        assert_eq!(device.drive_id(), "rootfs");
        assert_eq!(device.config_space().capacity_sectors(), 2);
        assert!(device.config_space().is_read_only());
        assert_eq!(
            device.device().device_id(),
            VirtioBlockDeviceId::from_bytes(b"rootfs")
        );
        let backing = device
            .device()
            .backing()
            .expect("file device should retain backing");
        assert_eq!(backing.len(), 1024);
        assert!(backing.is_read_only());
        assert!(matches!(
            backing.write_at(0, b"x"),
            Err(BlockFileBackingError::ReadOnlyWrite),
        ));
        assert!(!device.device().is_activated());
    }

    #[test]
    fn prepared_block_devices_prepare_read_write_block_device() {
        let file = temp_file("prepared-rw.img", b"abc");
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new("data", "data", file.as_path(), false))
            .expect("data drive config should insert");

        let prepared = PreparedBlockDevices::from_configs(&configs)
            .expect("read-write block device should prepare");

        let device = &prepared.as_slice()[0];
        assert_eq!(device.drive_id(), "data");
        assert!(!device.config_space().is_read_only());
        let backing = device
            .device()
            .backing()
            .expect("file device should retain backing");
        assert!(!backing.is_read_only());
        backing
            .write_at(1, b"Z")
            .expect("read-write prepared backing should write");
        assert_eq!(fs::read(file.as_path()).expect("file should read"), b"aZc");
    }

    #[test]
    fn prepared_block_devices_preserve_writeback_cache_type() {
        let file = temp_file("prepared-writeback.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(
                DriveConfigInput::new("data", "data", file.as_path(), false)
                    .with_cache_type(DriveCacheType::Writeback),
            )
            .expect("writeback drive config should insert");

        let prepared = PreparedBlockDevices::from_configs(&configs)
            .expect("writeback block device should prepare");

        let config_space = prepared.as_slice()[0].config_space();
        assert_eq!(config_space.cache_type(), DriveCacheType::Writeback);
        assert_ne!(
            config_space.available_features() & (1_u64 << VIRTIO_BLOCK_FEATURE_FLUSH),
            0
        );
    }

    #[test]
    fn prepared_async_device_binds_and_reset_replaces_its_generation() {
        let file = temp_file("prepared-async-reset.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(
                DriveConfigInput::new("data", "data", file.as_path(), false)
                    .with_io_engine(DriveIoEngine::Async),
            )
            .expect("Async drive config should insert");
        let mut prepared = PreparedBlockDevices::from_configs(&configs)
            .expect("Async device should prepare without starting workers");
        let runtime = SharedBlockAsyncRuntime::new();
        assert_eq!(runtime.completion_fd().expect("runtime should lock"), None);

        prepared
            .bind_async_runtime(&runtime)
            .expect("Async device should bind shared runtime");
        let first = prepared.as_slice()[0]
            .device()
            .async_binding()
            .expect("Async binding should be visible")
            .1;
        assert_eq!(runtime.generation_count().expect("runtime should lock"), 1);
        prepared.devices[0].device_mut().reset();
        let second = prepared.as_slice()[0]
            .device()
            .async_binding()
            .expect("reset should retain a live Async binding")
            .1;

        assert!(second.value() > first.value());
        assert_eq!(runtime.generation_count().expect("runtime should lock"), 1);
        runtime
            .shutdown(&mut request_memory())
            .expect("Async runtime should shut down");
    }

    #[test]
    fn block_capture_retains_sync_and_reopenable_async_engine_state() {
        let sync_file = temp_file("capture-sync.img", &[0; 512]);
        let sync_config = DriveConfigInput::new("sync", "sync", sync_file.as_path(), true)
            .with_is_read_only(true)
            .with_io_engine(DriveIoEngine::Sync)
            .validate()
            .expect("Sync capture config should validate");
        let sync = PreparedBlockDevice::from_config_with_backing(&sync_config, None)
            .expect("Sync block device should prepare");
        let sync_state = sync
            .device()
            .capture_state_at(sync.config_space(), &sync_config, Instant::now())
            .expect("Sync block state should capture");
        assert_eq!(sync_state.config_space(), sync.config_space());
        assert_eq!(sync_state.backing().len(), 512);
        assert_eq!(sync_state.io_engine(), BlockCaptureIoEngine::Sync);
        assert_eq!(
            format!("{sync_state:?}"),
            "VirtioBlockDeviceCaptureState { state: \"<redacted>\" }"
        );

        let async_file = temp_file("capture-async.img", &[0; 512]);
        let async_config = DriveConfigInput::new("async", "async", async_file.as_path(), false)
            .with_io_engine(DriveIoEngine::Async)
            .validate()
            .expect("Async capture config should validate");
        let mut asynchronous = PreparedBlockDevice::from_config_with_backing(&async_config, None)
            .expect("Async block device should prepare");
        let runtime = SharedBlockAsyncRuntime::new();
        let generation = asynchronous
            .bind_async_runtime(runtime.clone())
            .expect("Async runtime should bind")
            .expect("Async configuration should own a generation");
        let mut memory = request_memory();
        runtime
            .stop_generations(&[generation])
            .expect("Async admission should stop");
        runtime
            .quiesce_generation(generation, &mut memory)
            .expect("idle Async generation should quiesce");
        let captured = asynchronous
            .device()
            .capture_state_at(asynchronous.config_space(), &async_config, Instant::now())
            .expect("quiesced Async state should capture");
        let BlockCaptureIoEngine::Async(async_state) = captured.io_engine() else {
            panic!("Async configuration should capture an Async engine");
        };
        assert_eq!(async_state.generation(), generation);
        assert!(async_state.admission_stopped());
        assert_eq!(async_state.owned_operations(), 0);
        assert_eq!(async_state.final_completions(), 0);
        runtime
            .resume_quiesced_generation(generation)
            .expect("captured Async generation should reopen");
        assert!(matches!(
            asynchronous.device().capture_state_at(
                asynchronous.config_space(),
                &async_config,
                Instant::now(),
            ),
            Err(VirtioBlockDeviceCaptureError::Async(
                BlockAsyncRuntimeError::AdmissionNotStopped
            ))
        ));
        runtime
            .stop_generations(&[generation])
            .expect("reopened Async generation should stop");
        runtime
            .quiesce_generation(generation, &mut memory)
            .expect("reopened Async generation should quiesce");
        runtime
            .unbind_quiesced(generation)
            .expect("Async generation should unbind");
        assert!(
            runtime
                .shutdown_if_idle()
                .expect("idle Async runtime should stop")
        );
    }

    #[test]
    fn live_file_replacement_switches_engines_and_replaces_exact_limiter() {
        let original_file = temp_file("live-engine-original.img", &[0; 512]);
        let async_file = temp_file("live-engine-async.img", &[1; 1024]);
        let sync_file = temp_file("live-engine-sync.img", &[2; 1536]);
        let original = DriveConfigInput::new("data", "data", original_file.as_path(), false)
            .validate()
            .expect("original config should validate");
        let limiter = DriveRateLimiterConfig::new(
            Some(DriveTokenBucketConfig::new(512, Some(1024), 100)),
            None,
        );
        let async_config = DriveConfigInput::new("data", "data", async_file.as_path(), false)
            .with_io_engine(DriveIoEngine::Async)
            .with_rate_limiter(limiter)
            .validate()
            .expect("Async replacement should validate");
        let sync_config = DriveConfigInput::new("data", "data", sync_file.as_path(), false)
            .with_io_engine(DriveIoEngine::Sync)
            .validate()
            .expect("Sync replacement should validate");
        let mut device = PreparedBlockDevice::from_config_with_backing(&original, None)
            .expect("original device should prepare")
            .device;
        let runtime = SharedBlockAsyncRuntime::new();
        let mut memory = request_memory();

        let async_dispatch = device
            .update_file_backend_with_opened(
                &mut memory,
                &async_config,
                Some(BlockFileBacking::open(&async_config).expect("Async backing should open")),
                None,
                DriveLiveUpdateMode::Replacement,
                &runtime,
            )
            .expect("Sync-to-Async replacement should succeed");
        assert_eq!(async_dispatch.processed_requests(), 0);
        assert_eq!(device.io_engine(), Some(DriveIoEngine::Async));
        assert_eq!(
            device.backing().expect("backing should remain file").len(),
            1024
        );
        assert!(device.rate_limiter().is_some());
        assert_eq!(runtime.generation_count().expect("runtime should lock"), 1);

        device
            .update_file_backend_with_opened(
                &mut memory,
                &sync_config,
                Some(BlockFileBacking::open(&sync_config).expect("Sync backing should open")),
                None,
                DriveLiveUpdateMode::Replacement,
                &runtime,
            )
            .expect("Async-to-Sync replacement should succeed");
        assert_eq!(device.io_engine(), Some(DriveIoEngine::Sync));
        assert_eq!(
            device.backing().expect("backing should remain file").len(),
            1536
        );
        assert!(device.rate_limiter().is_none());
        assert_eq!(runtime.generation_count().expect("runtime should lock"), 0);
        runtime
            .shutdown(&mut memory)
            .expect("idle runtime should shut down");
    }

    #[test]
    fn prepared_block_devices_preserve_drive_config_order() {
        let root_file = temp_file("prepared-root.img", &[0; 512]);
        let data_file = temp_file("prepared-data.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                data_file.as_path(),
                false,
            ))
            .expect("data drive config should insert");
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                root_file.as_path(),
                true,
            ))
            .expect("root drive config should insert");

        let prepared = PreparedBlockDevices::from_configs(&configs)
            .expect("ordered block devices should prepare");

        assert_eq!(prepared.as_slice()[0].drive_id(), "rootfs");
        assert_eq!(prepared.as_slice()[1].drive_id(), "data");
    }

    #[test]
    fn prepared_block_devices_derive_device_id_from_drive_id() {
        let short_file = temp_file("prepared-short-id.img", &[0; 512]);
        let long_file = temp_file("prepared-long-id.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "id",
                "id",
                short_file.as_path(),
                false,
            ))
            .expect("short drive config should insert");
        configs
            .insert(DriveConfigInput::new(
                "01234567890123456789extra",
                "01234567890123456789extra",
                long_file.as_path(),
                false,
            ))
            .expect("long drive config should insert");

        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block devices should prepare");

        let short_id = prepared.as_slice()[0].device().device_id();
        let mut expected_short = [0; VIRTIO_BLOCK_ID_BYTES as usize];
        expected_short[0] = b'i';
        expected_short[1] = b'd';
        assert_eq!(short_id.as_bytes(), &expected_short);
        assert_eq!(
            prepared.as_slice()[1].device().device_id().as_bytes(),
            b"01234567890123456789",
        );
    }

    #[test]
    fn runtime_block_resource_preserves_prepared_device_and_redacts_debug() {
        let file = temp_file("secret-runtime-resource.img", &[0; 512]);
        let config = config_for_drive("data", file.as_path(), false, false);
        let prepared = PreparedBlockDevice::from_config_with_backing(&config, None)
            .expect("runtime block resource should prepare");
        let expected_config = prepared.config_space();
        let expected_device_id = *prepared.device().device_id().as_bytes();
        let resource = RuntimeBlockDeviceResource::prepared(prepared);

        assert_eq!(resource.drive_id(), "data");
        assert!(!resource.is_vhost_user());
        let debug = format!("{resource:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("data"));
        assert!(!debug.contains("secret-runtime-resource"));

        let RuntimeBlockDeviceResource::Prepared(materialized) = resource else {
            panic!("prepared resource should retain its concrete device")
        };
        assert_eq!(materialized.config_space(), expected_config);
        assert_eq!(
            materialized.device().device_id().as_bytes(),
            &expected_device_id
        );
    }

    #[test]
    fn prepared_block_devices_reject_missing_backing_without_path_leak() {
        let path = missing_path("secret-prepared-missing.img");
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new("rootfs", "rootfs", &path, false))
            .expect("missing path is still a valid stored config");

        let err = PreparedBlockDevices::from_configs(&configs)
            .expect_err("missing backing should fail preparation");

        match &err {
            PreparedBlockDeviceError::OpenBacking { drive_id, source } => {
                assert_eq!(drive_id, "rootfs");
                assert!(matches!(source, BlockFileBackingError::OpenFile { .. }));
            }
            PreparedBlockDeviceError::AllocateDevices { .. } => {
                panic!("missing path should not fail allocation")
            }
            PreparedBlockDeviceError::BackingModeMismatch { .. } => {
                panic!("path-only preparation should not have a backing mode mismatch")
            }
            PreparedBlockDeviceError::UnexpectedBacking => {
                panic!("path-only preparation should not have a provided backing")
            }
            other => panic!("unexpected preparation error: {other:?}"),
        }
        assert!(err.source().is_some());
        assert!(!err.to_string().contains("secret-prepared-missing"));
    }

    #[test]
    fn prepared_block_devices_reject_directory_without_path_leak() {
        let dir = temp_dir("secret-prepared-dir.img");
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                dir.as_path(),
                false,
            ))
            .expect("directory path is still a valid stored config");

        let err = PreparedBlockDevices::from_configs(&configs)
            .expect_err("directory backing should fail preparation");

        match &err {
            PreparedBlockDeviceError::OpenBacking { drive_id, source } => {
                assert_eq!(drive_id, "rootfs");
                assert!(matches!(
                    source,
                    BlockFileBackingError::OpenFile { .. } | BlockFileBackingError::NonRegularFile
                ));
            }
            PreparedBlockDeviceError::AllocateDevices { .. } => {
                panic!("directory should not fail allocation")
            }
            PreparedBlockDeviceError::BackingModeMismatch { .. } => {
                panic!("path-only preparation should not have a backing mode mismatch")
            }
            PreparedBlockDeviceError::UnexpectedBacking => {
                panic!("path-only preparation should not have a provided backing")
            }
            other => panic!("unexpected preparation error: {other:?}"),
        }
        assert!(!err.to_string().contains("secret-prepared-dir"));
    }

    #[test]
    fn prepared_block_devices_reject_fifo_without_blocking_or_path_leak() {
        let fifo = temp_fifo("secret-prepared-fifo.img");
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                fifo.as_path(),
                false,
            ))
            .expect("FIFO path is still a valid stored config");

        let err = PreparedBlockDevices::from_configs(&configs)
            .expect_err("FIFO backing should fail preparation");

        match &err {
            PreparedBlockDeviceError::OpenBacking { drive_id, source } => {
                assert_eq!(drive_id, "rootfs");
                assert!(matches!(
                    source,
                    BlockFileBackingError::OpenFile { .. } | BlockFileBackingError::NonRegularFile
                ));
            }
            PreparedBlockDeviceError::AllocateDevices { .. } => {
                panic!("FIFO should not fail allocation")
            }
            PreparedBlockDeviceError::BackingModeMismatch { .. } => {
                panic!("path-only preparation should not have a backing mode mismatch")
            }
            PreparedBlockDeviceError::UnexpectedBacking => {
                panic!("path-only preparation should not have a provided backing")
            }
            other => panic!("unexpected preparation error: {other:?}"),
        }
        assert!(!err.to_string().contains("secret-prepared-fifo"));
    }

    #[test]
    fn prepared_block_devices_fail_without_mutating_drive_configs() {
        let file = temp_file("prepared-valid.img", &[0; 512]);
        let missing = missing_path("secret-prepared-partial.img");
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                file.as_path(),
                true,
            ))
            .expect("valid config should insert");
        configs
            .insert(DriveConfigInput::new("data", "data", &missing, false))
            .expect("missing path is still a valid stored config");

        let err = PreparedBlockDevices::from_configs(&configs)
            .expect_err("missing second backing should fail preparation");

        assert!(matches!(err, PreparedBlockDeviceError::OpenBacking { .. }));
        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
        assert_eq!(configs.as_slice()[1].drive_id(), "data");
    }

    #[test]
    fn block_mmio_devices_accept_empty_prepared_devices() {
        let configs = DriveConfigs::new();
        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("empty configs should prepare");

        let devices = prepared
            .register_mmio(BlockMmioLayout::new(
                GuestAddress::new(TEST_MMIO_BASE),
                MmioRegionId::new(10),
            ))
            .expect("empty prepared block devices should register");

        assert!(devices.is_empty());
        assert_eq!(devices.len(), 0);
        assert!(devices.registrations().is_empty());
        assert!(devices.dispatcher().regions().is_empty());
    }

    #[test]
    fn block_mmio_devices_register_one_prepared_device() {
        let file = temp_file("block-mmio-one.img", &[0; 1024]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                file.as_path(),
                true,
            ))
            .expect("root drive config should insert");
        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block device should prepare");

        let mut devices = prepared
            .register_mmio(BlockMmioLayout::new(
                GuestAddress::new(TEST_MMIO_BASE),
                MmioRegionId::new(10),
            ))
            .expect("block MMIO device should register");

        assert_eq!(devices.len(), 1);
        let registration = &devices.registrations()[0];
        assert_eq!(registration.index(), 0);
        assert_eq!(registration.drive_id(), "rootfs");
        assert_eq!(registration.region_id(), MmioRegionId::new(10));
        assert_eq!(registration.address(), GuestAddress::new(TEST_MMIO_BASE));
        assert_eq!(
            registration.region().range().size(),
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE
        );
        assert_eq!(devices.dispatcher().regions().len(), 1);
        assert_eq!(devices.dispatcher().regions()[0], registration.region());
        assert_eq!(
            dispatch_block_mmio_read_u32(&mut devices, 0, VirtioMmioRegister::MagicValue.offset()),
            VIRTIO_MMIO_MAGIC_VALUE,
        );
        assert_eq!(
            dispatch_block_mmio_read_u32(&mut devices, 0, VirtioMmioRegister::DeviceId.offset()),
            VIRTIO_BLOCK_DEVICE_ID,
        );
    }

    #[test]
    fn prepared_block_device_attaches_configured_rate_limiter() {
        let file = temp_file("prepared-rate-limiter.img", &[0; 1024]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(
                DriveConfigInput::new("rootfs", "rootfs", file.as_path(), true).with_rate_limiter(
                    DriveRateLimiterConfig::new(
                        Some(DriveTokenBucketConfig::new(512, None, 100)),
                        Some(DriveTokenBucketConfig::new(1, None, 1000)),
                    ),
                ),
            )
            .expect("root drive config should insert");

        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block device should prepare");

        assert_eq!(prepared.as_slice().len(), 1);
        assert!(prepared.as_slice()[0].device().rate_limiter().is_some());
    }

    #[test]
    fn block_device_rate_limiter_update_preserves_missing_bucket_runtime_state() {
        let now = Instant::now();
        let file = temp_file("runtime-rate-limiter-preserve.img", &[0; 1024]);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID).with_rate_limiter(
            VirtioBlockRateLimiter::new_at(
                DriveRateLimiterConfig::new(
                    Some(DriveTokenBucketConfig::new(4, Some(2), 1000)),
                    Some(DriveTokenBucketConfig::new(1, None, 1000)),
                ),
                now,
            )
            .expect("rate limiter should be enabled"),
        );
        let VirtioBlockBackend::File { rate_limiter, .. } = &mut device.backend else {
            panic!("test device should use the file backend");
        };
        let rate_limiter = rate_limiter.as_mut().expect("rate limiter should exist");
        let bandwidth = rate_limiter
            .bandwidth
            .as_mut()
            .expect("bandwidth bucket should exist");
        assert!(bandwidth.reduce_at(3, now));
        let bandwidth_snapshot = bandwidth.snapshot();

        device
            .update_rate_limiter(DriveRateLimiterConfig::new(
                None,
                Some(DriveTokenBucketConfig::new(2, None, 1000)),
            ))
            .expect("file device rate limiter should update");

        let rate_limiter = device
            .rate_limiter()
            .expect("rate limiter should remain configured");
        assert_eq!(
            rate_limiter
                .bandwidth
                .as_ref()
                .expect("missing bandwidth update should preserve bucket")
                .snapshot(),
            bandwidth_snapshot
        );
        assert!(rate_limiter.ops.is_some());
    }

    #[test]
    fn block_device_rate_limiter_update_clears_disabled_buckets() {
        let now = Instant::now();
        let file = temp_file("runtime-rate-limiter-clear.img", &[0; 1024]);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID).with_rate_limiter(
            VirtioBlockRateLimiter::new_at(
                DriveRateLimiterConfig::new(
                    Some(DriveTokenBucketConfig::new(4, None, 1000)),
                    Some(DriveTokenBucketConfig::new(1, None, 1000)),
                ),
                now,
            )
            .expect("rate limiter should be enabled"),
        );

        device
            .update_rate_limiter(DriveRateLimiterConfig::new(
                Some(DriveTokenBucketConfig::new(0, None, 1000)),
                None,
            ))
            .expect("file device rate limiter should update");

        let rate_limiter = device
            .rate_limiter()
            .expect("ops bucket should keep limiter configured");
        assert!(rate_limiter.bandwidth.is_none());
        assert!(rate_limiter.ops.is_some());

        device
            .update_rate_limiter(DriveRateLimiterConfig::new(
                None,
                Some(DriveTokenBucketConfig::new(1, None, 0)),
            ))
            .expect("file device rate limiter should update");

        assert!(device.rate_limiter().is_none());
    }

    #[test]
    fn block_mmio_devices_preserve_prepared_drive_order_and_layout() {
        let root_file = temp_file("block-mmio-root.img", &[0; 512]);
        let data_file = temp_file("block-mmio-data.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                data_file.as_path(),
                false,
            ))
            .expect("data drive config should insert");
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                root_file.as_path(),
                true,
            ))
            .expect("root drive config should insert");
        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block devices should prepare");

        let devices = prepared
            .register_mmio(
                BlockMmioLayout::new(GuestAddress::new(TEST_MMIO_BASE), MmioRegionId::new(20))
                    .with_address_stride(VIRTIO_MMIO_DEVICE_WINDOW_SIZE * 2)
                    .with_region_id_stride(3),
            )
            .expect("block MMIO devices should register");

        assert_eq!(devices.registrations()[0].drive_id(), "rootfs");
        assert_eq!(devices.registrations()[0].index(), 0);
        assert_eq!(
            devices.registrations()[0].region_id(),
            MmioRegionId::new(20)
        );
        assert_eq!(
            devices.registrations()[0].address(),
            GuestAddress::new(TEST_MMIO_BASE),
        );
        assert_eq!(devices.registrations()[1].drive_id(), "data");
        assert_eq!(devices.registrations()[1].index(), 1);
        assert_eq!(
            devices.registrations()[1].region_id(),
            MmioRegionId::new(23)
        );
        assert_eq!(
            devices.registrations()[1].address(),
            GuestAddress::new(TEST_MMIO_BASE + VIRTIO_MMIO_DEVICE_WINDOW_SIZE * 2),
        );
    }

    #[test]
    fn block_mmio_devices_dispatch_read_only_config_space() {
        let file = temp_file("block-mmio-read-only.img", &[0; 1024]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(
                DriveConfigInput::new("rootfs", "rootfs", file.as_path(), true)
                    .with_is_read_only(true),
            )
            .expect("root drive config should insert");
        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block device should prepare");
        let mut devices = prepared
            .register_mmio(BlockMmioLayout::new(
                GuestAddress::new(TEST_MMIO_BASE),
                MmioRegionId::new(30),
            ))
            .expect("block MMIO device should register");

        let low_features = dispatch_block_mmio_read_u32(
            &mut devices,
            0,
            VirtioMmioRegister::DeviceFeatures.offset(),
        );
        assert_ne!(low_features & (1 << VIRTIO_BLOCK_FEATURE_READ_ONLY), 0);

        let capacity = dispatch_block_mmio_read(
            &mut devices,
            0,
            VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
            VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE as u64,
        );
        assert_eq!(capacity.as_slice(), &2_u64.to_le_bytes());
    }

    #[test]
    fn block_mmio_devices_reject_overlapping_address_stride() {
        let root_file = temp_file("block-mmio-overlap-root.img", &[0; 512]);
        let data_file = temp_file("block-mmio-overlap-data.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                root_file.as_path(),
                true,
            ))
            .expect("root drive config should insert");
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                data_file.as_path(),
                false,
            ))
            .expect("data drive config should insert");
        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block devices should prepare");

        let err = prepared
            .register_mmio(
                BlockMmioLayout::new(GuestAddress::new(TEST_MMIO_BASE), MmioRegionId::new(40))
                    .with_address_stride(VIRTIO_MMIO_DEVICE_WINDOW_SIZE - 1),
            )
            .expect_err("overlapping block MMIO layout should fail");

        assert!(matches!(
            err,
            BlockMmioRegistrationError::AddressStrideTooSmall { .. },
        ));
    }

    #[test]
    fn block_mmio_devices_reject_duplicate_region_id_stride() {
        let root_file = temp_file("block-mmio-duplicate-id-root.img", &[0; 512]);
        let data_file = temp_file("block-mmio-duplicate-id-data.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                root_file.as_path(),
                true,
            ))
            .expect("root drive config should insert");
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                data_file.as_path(),
                false,
            ))
            .expect("data drive config should insert");
        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block devices should prepare");

        let err = prepared
            .register_mmio(
                BlockMmioLayout::new(GuestAddress::new(TEST_MMIO_BASE), MmioRegionId::new(50))
                    .with_region_id_stride(0),
            )
            .expect_err("duplicate block MMIO region id layout should fail");

        assert!(matches!(
            err,
            BlockMmioRegistrationError::DuplicateRegionIdStride { .. },
        ));
    }

    #[test]
    fn block_mmio_devices_reject_address_overflow_without_returning_bundle() {
        let root_file = temp_file("block-mmio-overflow-root.img", &[0; 512]);
        let data_file = temp_file("block-mmio-overflow-data.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                root_file.as_path(),
                true,
            ))
            .expect("root drive config should insert");
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                data_file.as_path(),
                false,
            ))
            .expect("data drive config should insert");
        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block devices should prepare");

        let err = prepared
            .register_mmio(
                BlockMmioLayout::new(GuestAddress::new(TEST_MMIO_BASE), MmioRegionId::new(60))
                    .with_address_stride(u64::MAX),
            )
            .expect_err("overflowing block MMIO layout should fail");

        assert!(matches!(
            err,
            BlockMmioRegistrationError::AddressOverflow { .. },
        ));
    }

    #[test]
    fn block_mmio_devices_reject_region_range_overflow() {
        let file = temp_file("block-mmio-range-overflow.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                file.as_path(),
                true,
            ))
            .expect("root drive config should insert");
        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block device should prepare");

        let err = prepared
            .register_mmio(BlockMmioLayout::new(
                GuestAddress::new(u64::MAX),
                MmioRegionId::new(70),
            ))
            .expect_err("overflowing block MMIO region range should fail");

        assert!(matches!(
            err,
            BlockMmioRegistrationError::InvalidRegion { .. },
        ));
    }

    #[test]
    fn block_mmio_devices_reject_region_id_overflow() {
        let root_file = temp_file("block-mmio-region-id-root.img", &[0; 512]);
        let data_file = temp_file("block-mmio-region-id-data.img", &[0; 512]);
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new(
                "rootfs",
                "rootfs",
                root_file.as_path(),
                true,
            ))
            .expect("root drive config should insert");
        configs
            .insert(DriveConfigInput::new(
                "data",
                "data",
                data_file.as_path(),
                false,
            ))
            .expect("data drive config should insert");
        let prepared =
            PreparedBlockDevices::from_configs(&configs).expect("block devices should prepare");

        let err = prepared
            .register_mmio(BlockMmioLayout::new(
                GuestAddress::new(TEST_MMIO_BASE),
                MmioRegionId::new(u64::MAX),
            ))
            .expect_err("overflowing block MMIO region id should fail");

        assert!(matches!(
            err,
            BlockMmioRegistrationError::RegionIdOverflow { .. },
        ));
    }

    #[test]
    fn virtio_block_constants_match_firecracker_shape() {
        assert_eq!(VIRTIO_BLOCK_DEVICE_ID, 2);
        assert_eq!(VIRTIO_BLOCK_QUEUE_COUNT, 1);
        assert_eq!(VIRTIO_BLOCK_QUEUE_SIZE, 256);
        assert_eq!(VIRTIO_BLOCK_QUEUE_SIZES, [256]);
        assert_eq!(VIRTIO_BLOCK_SECTOR_SHIFT, 9);
        assert_eq!(VIRTIO_BLOCK_SECTOR_SIZE, 512);
        assert_eq!(VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE, 8);
        assert_eq!(VIRTIO_BLOCK_FEATURE_READ_ONLY, 5);
        assert_eq!(VIRTIO_BLOCK_FEATURE_FLUSH, 9);
        assert_eq!(VIRTIO_RING_FEATURE_INDIRECT_DESC, 28);
        assert_eq!(VIRTIO_RING_FEATURE_EVENT_IDX, 29);
        assert_eq!(VIRTIO_FEATURE_VERSION_1, 32);
        assert_eq!(VIRTIO_BLOCK_REQUEST_HEADER_SIZE, 16);
        assert_eq!(VIRTIO_BLOCK_STATUS_SIZE, 1);
        assert_eq!(VIRTIO_BLOCK_ID_BYTES, 20);
        assert_eq!(VIRTIO_BLOCK_REQUEST_TYPE_IN, 0);
        assert_eq!(VIRTIO_BLOCK_REQUEST_TYPE_OUT, 1);
        assert_eq!(VIRTIO_BLOCK_REQUEST_TYPE_FLUSH, 4);
        assert_eq!(VIRTIO_BLOCK_REQUEST_TYPE_GET_ID, 8);
        assert_eq!(VIRTIO_BLOCK_STATUS_OK, 0);
        assert_eq!(VIRTIO_BLOCK_STATUS_IOERR, 1);
        assert_eq!(VIRTIO_BLOCK_STATUS_UNSUPPORTED, 2);
    }

    #[test]
    fn config_space_reports_sector_capacity() {
        let config = VirtioBlockConfigSpace::new(4096, false, DriveCacheType::Unsafe);

        assert_eq!(config.capacity_sectors(), 8);
        assert!(!config.is_read_only());
        assert_eq!(config.cache_type(), DriveCacheType::Unsafe);
    }

    #[test]
    fn config_space_truncates_unaligned_tail() {
        assert_eq!(
            VirtioBlockConfigSpace::new(511, false, DriveCacheType::Unsafe).capacity_sectors(),
            0
        );
        assert_eq!(
            VirtioBlockConfigSpace::new(512, false, DriveCacheType::Unsafe).capacity_sectors(),
            1
        );
        assert_eq!(
            VirtioBlockConfigSpace::new(4097, false, DriveCacheType::Unsafe).capacity_sectors(),
            8
        );
    }

    #[test]
    fn config_space_tracks_cache_and_read_only_features() {
        let indirect_feature = 1_u64 << VIRTIO_RING_FEATURE_INDIRECT_DESC;
        let event_idx_feature = 1_u64 << VIRTIO_RING_FEATURE_EVENT_IDX;
        let base_features =
            (1_u64 << VIRTIO_FEATURE_VERSION_1) | indirect_feature | event_idx_feature;
        let flush_feature = 1_u64 << VIRTIO_BLOCK_FEATURE_FLUSH;
        let read_only_feature = 1_u64 << VIRTIO_BLOCK_FEATURE_READ_ONLY;

        assert_eq!(
            VirtioBlockConfigSpace::new(512, false, DriveCacheType::Unsafe).available_features(),
            base_features
        );
        assert_ne!(
            VirtioBlockConfigSpace::new(512, false, DriveCacheType::Unsafe).available_features()
                & indirect_feature,
            0
        );
        assert_ne!(
            VirtioBlockConfigSpace::new(512, false, DriveCacheType::Unsafe).available_features()
                & event_idx_feature,
            0
        );
        assert_eq!(
            VirtioBlockConfigSpace::new(512, false, DriveCacheType::Writeback).available_features(),
            base_features | flush_feature
        );
        assert_eq!(
            VirtioBlockConfigSpace::new(512, true, DriveCacheType::Unsafe).available_features(),
            base_features | read_only_feature
        );
        assert_eq!(
            VirtioBlockConfigSpace::new(512, true, DriveCacheType::Writeback).available_features(),
            base_features | read_only_feature | flush_feature
        );
    }

    #[test]
    fn config_space_can_be_derived_from_backing() {
        let file = temp_file("config-space.img", &[0; 1024]);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let config = VirtioBlockConfigSpace::from_backing(&backing, DriveCacheType::Writeback);

        assert_eq!(config.capacity_sectors(), 2);
        assert!(config.is_read_only());
        assert_eq!(config.cache_type(), DriveCacheType::Writeback);
    }

    #[test]
    fn block_handler_refresh_updates_backing_config_generation_and_interrupt() {
        let first = temp_file("refresh-first.img", &[0; 512]);
        let second = temp_file("refresh-second.img", &[0; 1024]);
        let first_backing =
            open_backing(first.as_path(), false).expect("first backing should open");
        let mut handler = block_notification_handler(first_backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        let replacement_config = config_for_path(second.as_path(), false);

        assert_eq!(handler.device_config_handler().capacity_sectors(), 1);
        assert_eq!(
            handler
                .activation_handler()
                .backing()
                .expect("file device should retain backing")
                .len(),
            512
        );
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );

        handler
            .refresh_block_backing(&replacement_config)
            .expect("replacement backing should refresh");

        assert_eq!(handler.device_config_handler().capacity_sectors(), 2);
        assert_eq!(
            handler
                .activation_handler()
                .backing()
                .expect("file device should retain backing")
                .len(),
            1024
        );
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(1)
        );
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Config.status().bits()
        );
    }

    #[test]
    fn block_handler_refresh_failure_preserves_previous_backing_and_config() {
        let first = temp_file("refresh-failure-first.img", &[0; 512]);
        let missing = missing_path("secret-refresh-missing.img");
        let first_backing =
            open_backing(first.as_path(), false).expect("first backing should open");
        let mut handler = block_notification_handler(first_backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        let replacement_config = config_for_path(&missing, false);

        let err = handler
            .refresh_block_backing(&replacement_config)
            .expect_err("missing replacement backing should fail");

        assert!(matches!(err, DriveUpdateError::OpenBacking { .. }));
        assert!(!err.to_string().contains("secret-refresh-missing"));
        assert_eq!(handler.device_config_handler().capacity_sectors(), 1);
        assert_eq!(
            handler
                .activation_handler()
                .backing()
                .expect("file device should retain backing")
                .len(),
            512
        );
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );
        assert_eq!(read_interrupt_status(&handler), 0);
    }

    #[test]
    fn vhost_user_config_refresh_rejects_pre_activation_without_frontend_io() {
        let (_config_space, mut device, _memory) = prepare_disconnected_test_vhost_user_device();

        let error = device
            .refreshed_vhost_user_config(DriveCacheType::Unsafe)
            .expect_err("pre-activation vhost-user config refresh should reject");

        assert_eq!(error, VhostUserBlockConfigRefreshError::Inactive);
        assert!(!device.is_activated());
        assert!(device.vhost_user_call_fd().is_some());
    }

    #[test]
    fn vhost_user_block_handler_refreshes_exact_config_generation_and_interrupt() {
        let mut refreshed = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        refreshed[..8].copy_from_slice(&2_u64.to_le_bytes());
        refreshed[20..24].copy_from_slice(&512_u32.to_le_bytes());
        let (mut handler, _memory, peer) =
            vhost_user_refresh_handler(TestVhostUserConfigReply::Exact(refreshed));
        let config = DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
            .with_socket("/private/redacted-vhost.sock")
            .validate()
            .expect("vhost-user capture config should validate");

        assert_eq!(
            handler.block_backend_kind(),
            VirtioBlockBackendKind::VhostUser
        );
        assert!(matches!(
            handler.capture_block_device_state_at(&config, Instant::now()),
            Err(VirtioBlockDeviceCaptureError::UnsupportedBackend)
        ));
        assert_eq!(handler.device_config_handler().capacity_sectors(), 1);
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );
        assert_eq!(read_interrupt_status(&handler), 0);

        handler
            .refresh_vhost_user_block_config(DriveCacheType::Unsafe)
            .expect("vhost-user config refresh should succeed");

        assert_eq!(handler.device_config_handler().bytes, refreshed);
        assert_eq!(handler.device_config_handler().capacity_sectors(), 2);
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(1)
        );
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Config.status().bits()
        );
        peer.join().expect("refresh peer should finish");
    }

    #[test]
    fn vhost_user_block_handler_refresh_failure_preserves_config_and_generation() {
        let (mut handler, _memory, peer) =
            vhost_user_refresh_handler(TestVhostUserConfigReply::Malformed);
        let original = *handler.device_config_handler();

        let error = handler
            .refresh_vhost_user_block_config(DriveCacheType::Unsafe)
            .expect_err("malformed config refresh should fail");

        assert!(matches!(
            error,
            DriveUpdateError::ActiveSessionCommand { .. }
        ));
        assert!(!error.to_string().contains("0x5a"));
        assert_eq!(*handler.device_config_handler(), original);
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );
        assert_eq!(read_interrupt_status(&handler), 0);
        peer.join().expect("refresh peer should finish");
    }

    #[test]
    fn vhost_user_mmio_refresh_rolls_back_confirmed_interrupt_failure() {
        let mut refreshed = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        refreshed[..8].copy_from_slice(&2_u64.to_le_bytes());
        let (mut handler, _memory, peer) =
            vhost_user_refresh_handler(TestVhostUserConfigReply::Exact(refreshed));
        let original_transport = handler.transport_state();
        let original_config = *handler.device_config_handler();

        let error = handler
            .refresh_vhost_user_block_config_with_signal(DriveCacheType::Unsafe, || {
                Err(VhostUserBlockConfigSignalError::new(
                    "injected confirmed SPI failure",
                    false,
                ))
            })
            .expect_err("confirmed MMIO configuration interrupt failure should roll back");

        assert!(matches!(
            error,
            DriveUpdateError::ActiveSessionCommand { .. }
        ));
        assert_eq!(handler.transport_state(), original_transport);
        assert_eq!(*handler.device_config_handler(), original_config);
        peer.join().expect("refresh peer should finish");
    }

    #[test]
    fn vhost_user_mmio_refresh_is_terminal_after_ambiguous_interrupt_failure() {
        let mut refreshed = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        refreshed[..8].copy_from_slice(&2_u64.to_le_bytes());
        let (mut handler, _memory, peer) =
            vhost_user_refresh_handler(TestVhostUserConfigReply::Exact(refreshed));

        let error = handler
            .refresh_vhost_user_block_config_with_signal(DriveCacheType::Unsafe, || {
                Err(VhostUserBlockConfigSignalError::new(
                    "injected ambiguous SPI failure",
                    true,
                ))
            })
            .expect_err("ambiguous MMIO configuration interrupt failure should be terminal");

        assert!(matches!(
            error,
            DriveUpdateError::TerminalActiveSessionCommand { .. }
        ));
        assert_eq!(handler.device_config_handler().bytes, refreshed);
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(1)
        );
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Config.status().bits()
        );
        peer.join().expect("refresh peer should finish");
    }

    #[test]
    fn vhost_user_pci_refresh_delivers_exact_config_once() {
        let mut refreshed = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        refreshed[..8].copy_from_slice(&2_u64.to_le_bytes());
        refreshed[20..24].copy_from_slice(&512_u32.to_le_bytes());
        let (endpoint, _memory, peer, signals) =
            vhost_user_refresh_pci_endpoint(TestVhostUserConfigReply::Exact(refreshed), None);

        endpoint
            .refresh_vhost_user_block_config(DriveCacheType::Unsafe)
            .expect("PCI vhost-user config refresh should succeed");

        let (generation, config) = vhost_user_pci_config_snapshot(&endpoint);
        assert_eq!(generation, 1);
        assert_eq!(config.bytes, refreshed);
        assert_eq!(
            *signals
                .lock()
                .expect("test signal count should remain available"),
            1
        );
        peer.join().expect("refresh peer should finish");
    }

    #[test]
    fn vhost_user_pci_refresh_rolls_back_confirmed_interrupt_failure() {
        let mut refreshed = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        refreshed[..8].copy_from_slice(&2_u64.to_le_bytes());
        let (endpoint, _memory, peer, signals) = vhost_user_refresh_pci_endpoint(
            TestVhostUserConfigReply::Exact(refreshed),
            Some(false),
        );
        let original = vhost_user_pci_config_snapshot(&endpoint);

        let error = endpoint
            .refresh_vhost_user_block_config(DriveCacheType::Unsafe)
            .expect_err("confirmed configuration interrupt failure should roll back");

        assert!(matches!(
            error,
            DriveUpdateError::ActiveSessionCommand { .. }
        ));
        assert_eq!(vhost_user_pci_config_snapshot(&endpoint), original);
        assert_eq!(
            *signals
                .lock()
                .expect("test signal count should remain available"),
            0
        );
        peer.join().expect("refresh peer should finish");
    }

    #[test]
    fn vhost_user_pci_refresh_is_terminal_after_ambiguous_interrupt_failure() {
        let mut refreshed = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        refreshed[..8].copy_from_slice(&2_u64.to_le_bytes());
        let (endpoint, _memory, peer, signals) =
            vhost_user_refresh_pci_endpoint(TestVhostUserConfigReply::Exact(refreshed), Some(true));

        let error = endpoint
            .refresh_vhost_user_block_config(DriveCacheType::Unsafe)
            .expect_err("ambiguous configuration interrupt failure should be terminal");

        assert!(matches!(
            error,
            DriveUpdateError::TerminalActiveSessionCommand { .. }
        ));
        let (generation, config) = vhost_user_pci_config_snapshot(&endpoint);
        assert_eq!(generation, 1);
        assert_eq!(config.bytes, refreshed);
        assert_eq!(
            *signals
                .lock()
                .expect("test signal count should remain available"),
            0
        );
        peer.join().expect("refresh peer should finish");
    }

    #[test]
    fn config_space_reads_full_and_partial_capacity() {
        let sectors = 0x0102_0304_u64;
        let config = VirtioBlockConfigSpace::new(
            sectors << VIRTIO_BLOCK_SECTOR_SHIFT,
            false,
            DriveCacheType::Unsafe,
        );
        let expected = sectors.to_le_bytes();

        assert_eq!(
            read_block_config(config, 0, 8)
                .expect("full capacity read should succeed")
                .as_slice(),
            expected.as_slice()
        );
        assert_eq!(
            read_block_config(config, 1, 2)
                .expect("partial capacity read should succeed")
                .as_slice(),
            &[0x03, 0x02]
        );
        assert_eq!(
            read_block_config(config, 4, 4)
                .expect("high capacity word read should succeed")
                .as_slice(),
            &[0, 0, 0, 0]
        );
    }

    #[test]
    fn config_space_reads_ending_at_capacity_boundary() {
        let config = VirtioBlockConfigSpace::new(u64::MAX, false, DriveCacheType::Unsafe);
        let expected = config.capacity_sectors().to_le_bytes();

        assert_eq!(
            read_block_config(config, 7, 1)
                .expect("last capacity byte should read")
                .as_slice(),
            expected.get(7..8).expect("test slice should exist")
        );
        assert_eq!(
            read_block_config(config, 4, 4)
                .expect("read ending at capacity boundary should succeed")
                .as_slice(),
            expected.get(4..8).expect("test slice should exist")
        );
    }

    #[test]
    fn config_space_rejects_out_of_bounds_reads() {
        let config = VirtioBlockConfigSpace::new(512, false, DriveCacheType::Unsafe);

        assert!(matches!(
            read_block_config(config, 8, 1),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 8, len: 1 })
        ));
        assert!(matches!(
            read_block_config(config, 7, 2),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 7, len: 2 })
        ));
    }

    #[test]
    fn config_space_rejects_writes_after_driver_status() {
        let config = VirtioBlockConfigSpace::new(512, false, DriveCacheType::Unsafe);

        assert!(matches!(
            write_block_config_after_driver(config, 0, &[1, 2, 3, 4]),
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite { offset: 0, len: 4 })
        ));
    }

    #[test]
    fn parses_read_request() {
        let mut memory = request_memory();
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            2,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        let request = parse_request(&memory, &chain).expect("read request should parse");
        let data = request.data().expect("read request should have data");
        let status = request.status();

        assert_eq!(request.request_type(), VirtioBlockRequestType::In);
        assert_eq!(request.sector(), 2);
        assert_eq!(data.index(), 1);
        assert_eq!(data.address(), DATA_ADDR);
        assert_eq!(data.len(), 512);
        assert!(data.is_write_only());
        assert_eq!(status.index(), 2);
        assert_eq!(status.address(), STATUS_ADDR);
        assert_eq!(status.len(), VIRTIO_BLOCK_STATUS_SIZE);
    }

    #[test]
    fn parses_write_request() {
        let mut memory = request_memory();
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            3,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(DATA_ADDR, 1024, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        let request = parse_request(&memory, &chain).expect("write request should parse");
        let data = request.data().expect("write request should have data");

        assert_eq!(request.request_type(), VirtioBlockRequestType::Out);
        assert_eq!(request.sector(), 3);
        assert_eq!(data.len(), 1024);
        assert!(!data.is_write_only());
    }

    #[test]
    fn parses_flush_request_without_data_descriptor() {
        let mut memory = request_memory();
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        let request = parse_request(&memory, &chain).expect("flush request should parse");

        assert_eq!(request.request_type(), VirtioBlockRequestType::Flush);
        assert_eq!(request.data(), None);
        assert_eq!(request.status().address(), STATUS_ADDR);
    }

    #[test]
    fn parses_flush_request_with_data_descriptor() {
        let mut memory = request_memory();
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(DATA_ADDR, 13, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        let request = parse_request(&memory, &chain).expect("flush request should parse");
        let data = request.data().expect("flush request should keep data");

        assert_eq!(request.request_type(), VirtioBlockRequestType::Flush);
        assert_eq!(data.address(), DATA_ADDR);
        assert_eq!(data.len(), 13);
        assert_eq!(request.status().address(), STATUS_ADDR);
    }

    #[test]
    fn parses_without_touching_data_or_status_memory() {
        let mut memory = request_memory();
        let unmapped_address = GuestAddress::new(TEST_MEMORY_SIZE);
        let unmapped_data = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(unmapped_address, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        let request =
            parse_request(&memory, &unmapped_data).expect("unmapped data should not fail parsing");
        assert_eq!(
            request.data().expect("request should keep data").address(),
            unmapped_address
        );

        let unmapped_status = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(unmapped_address, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        let request = parse_request(&memory, &unmapped_status)
            .expect("unmapped status should not fail parsing");
        assert_eq!(request.status().address(), unmapped_address);
    }

    #[test]
    fn parses_get_device_id_request() {
        let mut memory = request_memory();
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_GET_ID,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, VIRTIO_BLOCK_ID_BYTES, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        let request = parse_request(&memory, &chain).expect("get-id request should parse");

        assert_eq!(request.request_type(), VirtioBlockRequestType::GetDeviceId);
        assert_eq!(
            request.data().expect("get-id should have data").len(),
            VIRTIO_BLOCK_ID_BYTES
        );
    }

    #[test]
    fn preserves_unsupported_request_type() {
        let mut memory = request_memory();
        let chain = request_chain(
            &mut memory,
            99,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(DATA_ADDR, 7, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        let request = parse_request(&memory, &chain).expect("unsupported request should parse");

        assert_eq!(
            request.request_type(),
            VirtioBlockRequestType::Unsupported(99)
        );
        assert_eq!(request.request_type().raw_value(), 99);
        assert_eq!(
            request
                .data()
                .expect("unsupported request keeps data metadata")
                .len(),
            7
        );
    }

    #[test]
    fn rejects_short_request_chains() {
        let mut memory = request_memory();
        let header_only = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[TestDescriptor::readable(
                HEADER_ADDR,
                VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
                None,
            )],
        );

        assert_eq!(
            parse_request(&memory, &header_only),
            Err(VirtioBlockRequestError::DescriptorChainTooShort {
                expected: 2,
                actual: 1,
            })
        );

        let missing_status = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 512, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &missing_status),
            Err(VirtioBlockRequestError::DescriptorChainTooShort {
                expected: 3,
                actual: 2,
            })
        );
    }

    #[test]
    fn rejects_invalid_header_descriptors() {
        let mut memory = request_memory();
        let write_only_header = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::writable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &write_only_header),
            Err(VirtioBlockRequestError::HeaderDescriptorWriteOnly { index: 0 })
        );

        let short_header = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(
                    HEADER_ADDR,
                    VIRTIO_BLOCK_REQUEST_HEADER_SIZE - 1,
                    Some(1),
                ),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &short_header),
            Err(VirtioBlockRequestError::HeaderDescriptorTooSmall {
                index: 0,
                len: VIRTIO_BLOCK_REQUEST_HEADER_SIZE - 1,
                min: VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
            })
        );
    }

    #[test]
    fn rejects_wrong_data_descriptor_direction() {
        let mut memory = request_memory();
        let read_with_readable_data = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &read_with_readable_data),
            Err(VirtioBlockRequestError::DataDescriptorReadOnly {
                request_type: VirtioBlockRequestType::In,
                index: 1,
            })
        );

        let write_with_write_only_data = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &write_with_write_only_data),
            Err(VirtioBlockRequestError::DataDescriptorWriteOnly {
                request_type: VirtioBlockRequestType::Out,
                index: 1,
            })
        );
    }

    #[test]
    fn rejects_invalid_status_descriptors() {
        let mut memory = request_memory();
        let readable_status = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::readable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &readable_status),
            Err(VirtioBlockRequestError::StatusDescriptorReadOnly { index: 2 })
        );

        let short_status = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, 0, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &short_status),
            Err(VirtioBlockRequestError::StatusDescriptorTooSmall {
                index: 2,
                len: 0,
                min: VIRTIO_BLOCK_STATUS_SIZE,
            })
        );
    }

    #[test]
    fn rejects_invalid_data_lengths_and_ranges() {
        let mut memory = request_memory();
        let unaligned_data = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 1, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &unaligned_data),
            Err(VirtioBlockRequestError::InvalidDataLength {
                request_type: VirtioBlockRequestType::In,
                len: 1,
            })
        );

        let short_get_id = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_GET_ID,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, VIRTIO_BLOCK_ID_BYTES - 1, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &short_get_id),
            Err(VirtioBlockRequestError::InvalidDataLength {
                request_type: VirtioBlockRequestType::GetDeviceId,
                len: VIRTIO_BLOCK_ID_BYTES - 1,
            })
        );

        let overflowing_range = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            u64::MAX,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &overflowing_range),
            Err(VirtioBlockRequestError::SectorRangeOverflow {
                sector: u64::MAX,
                sectors_len: 1,
            })
        );

        let exact_capacity_end = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            15,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        parse_request(&memory, &exact_capacity_end)
            .expect("request ending exactly at capacity should parse");

        let out_of_bounds = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            15,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 1024, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        assert_eq!(
            parse_request(&memory, &out_of_bounds),
            Err(VirtioBlockRequestError::SectorRangeOutOfBounds {
                sector: 15,
                sectors_len: 2,
                capacity_sectors: 16,
            })
        );
    }

    #[test]
    fn header_read_error_does_not_mutate_status() {
        let mut memory = request_memory();
        memory
            .write_slice(&[0xaa], STATUS_ADDR)
            .expect("status sentinel should write");
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(
                    GuestAddress::new(TEST_MEMORY_SIZE),
                    VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
                    Some(1),
                ),
                TestDescriptor::writable(DATA_ADDR, 512, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );

        let err = parse_request(&memory, &chain).expect_err("unmapped header should fail");
        assert!(matches!(err, VirtioBlockRequestError::ReadHeader { .. }));

        let mut status = [0];
        memory
            .read_slice(&mut status, STATUS_ADDR)
            .expect("status sentinel should read");
        assert_eq!(status, [0xaa]);
    }

    #[test]
    fn device_id_from_bytes_truncates_and_zero_pads() {
        let short = VirtioBlockDeviceId::from_bytes(b"id");
        let mut expected_short = [0; VIRTIO_BLOCK_ID_BYTES as usize];
        expected_short[0] = b'i';
        expected_short[1] = b'd';
        assert_eq!(short.as_bytes(), &expected_short);

        let long = VirtioBlockDeviceId::from_bytes(b"01234567890123456789extra");
        assert_eq!(long.as_bytes(), b"01234567890123456789");
    }

    #[test]
    fn completion_length_overflow_maps_to_io_error_status() {
        let (status_code, bytes_written_to_guest, outcome) = normalize_completion_status(
            VIRTIO_BLOCK_STATUS_OK,
            u32::MAX,
            VirtioBlockRequestExecutionOutcome::Ok,
        );

        assert_eq!(status_code, VIRTIO_BLOCK_STATUS_IOERR);
        assert_eq!(bytes_written_to_guest, 0);
        assert!(matches!(
            outcome,
            VirtioBlockRequestExecutionOutcome::IoError {
                error: VirtioBlockRequestExecutionError::CompletionLengthOverflow {
                    bytes_written_to_guest: u32::MAX,
                }
            }
        ));
    }

    #[test]
    fn executes_read_request_into_guest_memory() {
        let mut memory = request_memory();
        let payload = sector_payload(0x5a);
        let mut file_bytes = vec![0; (VIRTIO_BLOCK_SECTOR_SIZE * 4) as usize];
        let offset = (VIRTIO_BLOCK_SECTOR_SIZE * 2) as usize;
        file_bytes[offset..offset + payload.len()].copy_from_slice(&payload);
        let file = temp_file("execute-read.img", &file_bytes);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            2,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );
        let request = parse_request_with_capacity(&memory, &chain, &backing);

        let execution = request.execute(&mut memory, &backing, TEST_DEVICE_ID);

        assert_eq!(request.descriptor_head(), 0);
        assert_eq!(
            execution.completion(),
            VirtioBlockRequestCompletion::new(
                0,
                VIRTIO_BLOCK_SECTOR_SIZE as u32 + VIRTIO_BLOCK_STATUS_SIZE,
            )
        );
        assert_eq!(execution.status_code(), VIRTIO_BLOCK_STATUS_OK);
        assert!(matches!(
            execution.outcome(),
            VirtioBlockRequestExecutionOutcome::Ok
        ));
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
    }

    #[test]
    fn executes_write_request_into_backing_file() {
        let mut memory = request_memory();
        let payload = sector_payload(0x37);
        memory
            .write_slice(&payload, DATA_ADDR)
            .expect("guest data should write");
        let file = temp_file(
            "execute-write.img",
            &vec![0; (VIRTIO_BLOCK_SECTOR_SIZE * 4) as usize],
        );
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            3,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );
        let request = parse_request_with_capacity(&memory, &chain, &backing);

        let execution = request.execute(&mut memory, &backing, TEST_DEVICE_ID);

        let offset = (VIRTIO_BLOCK_SECTOR_SIZE * 3) as usize;
        let written_file = fs::read(file.as_path()).expect("file should read");
        assert_eq!(
            execution.completion(),
            VirtioBlockRequestCompletion::new(0, VIRTIO_BLOCK_STATUS_SIZE)
        );
        assert_eq!(execution.status_code(), VIRTIO_BLOCK_STATUS_OK);
        assert!(matches!(
            execution.outcome(),
            VirtioBlockRequestExecutionOutcome::Ok
        ));
        assert_eq!(
            &written_file[offset..offset + payload.len()],
            payload.as_slice()
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
    }

    #[test]
    fn executes_flush_request() {
        let mut memory = request_memory();
        let file = temp_file("execute-flush.img", &sector_payload(0x11));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );
        let request = parse_request_with_capacity(&memory, &chain, &backing);

        let execution = request.execute(&mut memory, &backing, TEST_DEVICE_ID);

        assert_eq!(
            execution.completion(),
            VirtioBlockRequestCompletion::new(0, VIRTIO_BLOCK_STATUS_SIZE)
        );
        assert_eq!(execution.status_code(), VIRTIO_BLOCK_STATUS_OK);
        assert!(matches!(
            execution.outcome(),
            VirtioBlockRequestExecutionOutcome::Ok
        ));
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
    }

    #[test]
    fn executes_get_device_id_request() {
        let mut memory = request_memory();
        memory
            .write_slice(&[0xaa; 24], DATA_ADDR)
            .expect("guest data sentinel should write");
        let file = temp_file("execute-get-id.img", &sector_payload(0x22));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_GET_ID,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, 24, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );
        let request = parse_request_with_capacity(&memory, &chain, &backing);

        let execution = request.execute(&mut memory, &backing, TEST_DEVICE_ID);

        let data = read_guest_bytes(&memory, DATA_ADDR, 24);
        assert_eq!(
            execution.completion(),
            VirtioBlockRequestCompletion::new(0, VIRTIO_BLOCK_ID_BYTES + VIRTIO_BLOCK_STATUS_SIZE,)
        );
        assert_eq!(execution.status_code(), VIRTIO_BLOCK_STATUS_OK);
        assert!(matches!(
            execution.outcome(),
            VirtioBlockRequestExecutionOutcome::Ok
        ));
        assert_eq!(
            &data[..VIRTIO_BLOCK_ID_BYTES as usize],
            TEST_DEVICE_ID.as_bytes()
        );
        assert_eq!(&data[VIRTIO_BLOCK_ID_BYTES as usize..], &[0xaa; 4]);
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
    }

    #[test]
    fn unsupported_request_writes_unsupported_status_without_touching_data() {
        let mut memory = request_memory();
        memory
            .write_slice(b"sentinel", DATA_ADDR)
            .expect("guest sentinel should write");
        let file = temp_file("execute-unsupported.img", &[]);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let chain = request_chain(
            &mut memory,
            99,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(DATA_ADDR, 8, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );
        let request = parse_request_with_capacity(&memory, &chain, &backing);

        let execution = request.execute(&mut memory, &backing, TEST_DEVICE_ID);

        assert_eq!(
            execution.completion(),
            VirtioBlockRequestCompletion::new(0, VIRTIO_BLOCK_STATUS_SIZE)
        );
        assert_eq!(execution.status_code(), VIRTIO_BLOCK_STATUS_UNSUPPORTED);
        assert!(matches!(
            execution.outcome(),
            VirtioBlockRequestExecutionOutcome::Unsupported { request_type: 99 }
        ));
        assert_eq!(read_guest_bytes(&memory, DATA_ADDR, 8), b"sentinel");
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_UNSUPPORTED);
    }

    #[test]
    fn read_only_write_returns_io_error_status_without_mutating_file() {
        let mut memory = request_memory();
        let payload = sector_payload(0x44);
        memory
            .write_slice(&payload, DATA_ADDR)
            .expect("guest data should write");
        let original = sector_payload(0x33);
        let file = temp_file("execute-readonly-write.img", &original);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::readable(DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );
        let request = parse_request_with_capacity(&memory, &chain, &backing);

        let execution = request.execute(&mut memory, &backing, TEST_DEVICE_ID);

        assert_eq!(
            execution.completion(),
            VirtioBlockRequestCompletion::new(0, VIRTIO_BLOCK_STATUS_SIZE)
        );
        assert_eq!(execution.status_code(), VIRTIO_BLOCK_STATUS_IOERR);
        match execution.outcome() {
            VirtioBlockRequestExecutionOutcome::IoError {
                error:
                    VirtioBlockRequestExecutionError::Backing {
                        request_type,
                        source,
                    },
            } => {
                assert_eq!(*request_type, VirtioBlockRequestType::Out);
                assert_eq!(source.to_string(), "block backing file is read-only");
            }
            other => panic!("expected read-only backing error, got {other:?}"),
        }
        assert_eq!(
            fs::read(file.as_path()).expect("file should read"),
            original
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_IOERR);
    }

    #[test]
    fn unmapped_read_data_returns_io_error_status() {
        let mut memory = request_memory();
        let file = temp_file("execute-unmapped-data.img", &sector_payload(0x55));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(
                    GuestAddress::new(TEST_MEMORY_SIZE),
                    VIRTIO_BLOCK_SECTOR_SIZE as u32,
                    Some(2),
                ),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );
        let request = parse_request_with_capacity(&memory, &chain, &backing);

        let execution = request.execute(&mut memory, &backing, TEST_DEVICE_ID);

        assert_eq!(
            execution.completion(),
            VirtioBlockRequestCompletion::new(0, VIRTIO_BLOCK_STATUS_SIZE)
        );
        assert_eq!(execution.status_code(), VIRTIO_BLOCK_STATUS_IOERR);
        assert!(matches!(
            execution.outcome(),
            VirtioBlockRequestExecutionOutcome::IoError {
                error: VirtioBlockRequestExecutionError::GuestMemoryWrite { .. }
            }
        ));
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_IOERR);
    }

    #[test]
    fn unmapped_status_returns_zero_completion_bytes() {
        let mut memory = request_memory();
        let payload = sector_payload(0x66);
        let file = temp_file("execute-unmapped-status.img", &payload);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, Some(2)),
                TestDescriptor::writable(
                    GuestAddress::new(TEST_MEMORY_SIZE),
                    VIRTIO_BLOCK_STATUS_SIZE,
                    None,
                ),
            ],
        );
        let request = parse_request_with_capacity(&memory, &chain, &backing);

        let execution = request.execute(&mut memory, &backing, TEST_DEVICE_ID);

        assert_eq!(
            execution.completion(),
            VirtioBlockRequestCompletion::new(0, 0)
        );
        assert_eq!(execution.status_code(), VIRTIO_BLOCK_STATUS_OK);
        assert!(matches!(
            execution.outcome(),
            VirtioBlockRequestExecutionOutcome::StatusWriteFailed {
                status_code: VIRTIO_BLOCK_STATUS_OK,
                ..
            }
        ));
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
    }

    #[test]
    fn block_queue_from_mmio_queue_state_uses_configured_queue_metadata() {
        let queues = configured_mmio_queue(4, true);

        let queue = VirtioBlockQueue::from_mmio_queue_state(
            queues.queue(0).expect("queue state should exist"),
        )
        .expect("block queue should build from ready mmio queue state");

        assert_eq!(
            queue.available_ring().descriptor_table(),
            TEST_DESCRIPTOR_TABLE
        );
        assert_eq!(queue.available_ring().available_ring(), TEST_AVAILABLE_RING);
        assert_eq!(queue.available_ring().queue_size(), 4);
        assert_eq!(queue.available_ring().next_avail(), 0);
        assert_eq!(queue.used_ring().used_ring(), TEST_USED_RING);
        assert_eq!(queue.used_ring().queue_size(), 4);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert!(!queue.event_idx_enabled());
    }

    #[test]
    fn block_queue_from_mmio_queue_state_rejects_not_ready_queue() {
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, false);

        let error = VirtioBlockQueue::from_mmio_queue_state(
            queues.queue(0).expect("queue state should exist"),
        )
        .expect_err("not-ready queue should not build");

        assert!(matches!(error, VirtioBlockQueueBuildError::QueueNotReady));
        assert_eq!(error.to_string(), "virtio-block queue is not ready");
        assert!(error.source().is_none());
    }

    #[test]
    fn block_queue_from_mmio_queue_state_wraps_available_ring_error() {
        let mut queues =
            VirtioMmioQueueRegisters::new(&[TEST_QUEUE_SIZE]).expect("queue table should build");
        queues
            .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
            .expect("queue ready should write");

        let error = VirtioBlockQueue::from_mmio_queue_state(
            queues.queue(0).expect("queue state should exist"),
        )
        .expect_err("zero-size queue should not build");

        assert!(matches!(
            error,
            VirtioBlockQueueBuildError::AvailableRing {
                source: VirtqueueAvailableRingError::InvalidQueueSize { queue_size: 0 }
            }
        ));
        assert_eq!(
            error
                .source()
                .expect("source should be preserved")
                .to_string(),
            "virtqueue size 0 must be a nonzero power of two"
        );
    }

    #[test]
    fn block_queue_from_mmio_queue_state_wraps_used_ring_error() {
        let queues =
            configured_mmio_queue_with_device_ring(TEST_QUEUE_SIZE, u32::MAX - 3, u32::MAX, true);

        let error = VirtioBlockQueue::from_mmio_queue_state(
            queues.queue(0).expect("queue state should exist"),
        )
        .expect_err("overflowing used ring should not build");

        match error {
            VirtioBlockQueueBuildError::UsedRing {
                source:
                    VirtqueueUsedRingError::UsedRingRangeOverflow {
                        used_ring,
                        queue_size,
                    },
            } => {
                assert_eq!(used_ring, GuestAddress::new(u64::MAX - 3));
                assert_eq!(queue_size, TEST_QUEUE_SIZE);
            }
            other => panic!("expected used ring overflow, got {other:?}"),
        }
    }

    #[test]
    fn block_device_activation_builds_active_queue() {
        let file = temp_file("block-device-activate.img", &sector_payload(0x11));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID);
        let queues = configured_mmio_queue(4, true);
        let registers = block_device_registers();

        VirtioMmioDeviceActivationHandler::activate(
            &mut device,
            VirtioMmioDeviceActivation::new(&registers, &queues),
        )
        .expect("block device should activate");

        let queue = device
            .active_queue()
            .expect("active queue should be stored");
        assert!(device.is_activated());
        assert_eq!(device.device_id(), TEST_DEVICE_ID);
        let backing = device.backing().expect("file device should retain backing");
        assert_eq!(backing.len(), VIRTIO_BLOCK_SECTOR_SIZE);
        assert!(!backing.is_read_only());
        assert_eq!(
            queue.available_ring().descriptor_table(),
            TEST_DESCRIPTOR_TABLE
        );
        assert_eq!(queue.available_ring().available_ring(), TEST_AVAILABLE_RING);
        assert_eq!(queue.available_ring().queue_size(), 4);
        assert_eq!(queue.used_ring().used_ring(), TEST_USED_RING);
        assert_eq!(queue.used_ring().queue_size(), 4);
    }

    #[test]
    fn vhost_user_block_discovers_activates_notifies_and_terminalizes_over_real_descriptors() {
        let (frontend_stream, peer_stream) =
            UnixStream::pair().expect("vhost-user stream pair should open");
        let features = SUPPORTED_VIRTIO_FEATURES;
        let mut config_bytes = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        config_bytes[..8].copy_from_slice(&2_u64.to_le_bytes());
        config_bytes[20..24].copy_from_slice(&512_u32.to_le_bytes());
        let peer = spawn_complete_test_vhost_user_peer(peer_stream, features, config_bytes);

        let frontend = PreparedVhostUserBlockFrontend::discover(
            frontend_stream,
            DriveCacheType::Writeback,
            Duration::from_secs(2),
        )
        .expect("vhost-user discovery should complete");
        assert_eq!(frontend.available_features(), features);
        assert!(frontend.is_read_only());
        assert_eq!(frontend.config_bytes(), &config_bytes);

        let config = DriveConfigInput::new_without_path_on_host("vhost", "vhost", true)
            .with_socket("/private/test-vhost.sock")
            .with_partuuid("0eaa91a0-01")
            .with_cache_type(DriveCacheType::Writeback)
            .validate()
            .expect("vhost-user drive should validate");
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test shared memory range should validate"),
        ])
        .expect("test shared memory layout should validate");
        let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("test shared guest memory should allocate");
        let prepared = PreparedBlockDevice::from_config_with_vhost_user(&config, frontend, &memory)
            .expect("vhost-user block should prepare with shared memory");
        assert_eq!(prepared.drive_id(), "vhost");
        assert!(prepared.is_root_device());
        assert_eq!(
            prepared.config_space().config_len(),
            VIRTIO_BLOCK_CONFIG_SIZE
        );
        assert_eq!(prepared.config_space().capacity_sectors(), 2);
        assert!(prepared.config_space().is_read_only());
        assert_eq!(prepared.config_space().available_features(), features);
        assert_eq!(prepared.config_space().bytes, config_bytes);

        let (_, _, config_space, mut device) = prepared.into_parts();
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, true);
        let registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BLOCK_DEVICE_ID,
            config_space.available_features(),
        )
        .with_runtime_state(
            [0, 1],
            features & !VHOST_USER_F_PROTOCOL_FEATURES,
            DRIVER_OK_STATUS,
        );
        device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect("vhost-user block should activate");
        assert!(device.is_activated());
        assert!(device.vhost_user_call_fd().is_some());

        let dispatch = device
            .dispatch_drained_queue_notifications(&mut memory, vec![0])
            .expect("guest kick and backend call should dispatch");
        assert_eq!(dispatch.drained_notifications(), &[0]);
        assert_eq!(dispatch.vhost_user_kicks(), 1);
        assert_eq!(dispatch.vhost_user_calls(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert!(dispatch.queue_dispatch().is_none());

        let observation = peer.join().expect("vhost-user peer should finish");
        assert_eq!(observation.guest_features, features);
        assert_eq!(observation.memory_region_count, 1);
        assert_eq!(observation.queue_size, u32::from(TEST_QUEUE_SIZE));

        let error = device
            .dispatch_drained_queue_notifications(&mut memory, Vec::new())
            .expect_err("closed backend call endpoint should terminalize the device");
        assert!(matches!(
            error,
            VirtioBlockDeviceNotificationError::VhostUser {
                calls: 0,
                source: VhostUserBlockNotificationError::Closed,
                ..
            }
        ));
        assert!(device.is_activated());
        assert_eq!(device.vhost_user_call_fd(), None);
        let terminal = device
            .dispatch_drained_queue_notifications(&mut memory, Vec::new())
            .expect_err("terminal device should reject later dispatch");
        assert!(matches!(
            terminal,
            VirtioBlockDeviceNotificationError::VhostUser {
                source: VhostUserBlockNotificationError::Terminal,
                ..
            }
        ));
    }

    #[test]
    fn vhost_user_pre_activation_reset_preserves_the_discovered_frontend() {
        let (_config_space, mut device, _memory) = prepare_disconnected_test_vhost_user_device();

        device.reset();
        device.reset();

        let VirtioBlockBackend::VhostUser(backend) = &device.backend else {
            panic!("test device should retain its vhost-user backend");
        };
        assert!(backend.frontend.is_some());
        assert!(matches!(
            backend.state,
            VhostUserBlockState::Prepared { .. }
        ));
        assert!(!device.is_activated());
        assert!(device.vhost_user_call_fd().is_some());
    }

    #[test]
    fn vhost_user_discovery_intersects_cache_features_and_preserves_exact_config() {
        let (frontend_stream, peer_stream) =
            UnixStream::pair().expect("vhost-user stream pair should open");
        let mut config_bytes = [0_u8; VIRTIO_BLOCK_CONFIG_SIZE];
        for (index, byte) in config_bytes.iter_mut().enumerate() {
            *byte = u8::try_from(index).expect("config byte index should fit u8");
        }
        let peer = spawn_test_vhost_user_discovery_peer(
            peer_stream,
            SUPPORTED_VIRTIO_FEATURES,
            VHOST_USER_PROTOCOL_F_CONFIG,
            TestVhostUserConfigReply::Exact(config_bytes),
        );

        let frontend = PreparedVhostUserBlockFrontend::discover(
            frontend_stream,
            DriveCacheType::Unsafe,
            Duration::from_secs(2),
        )
        .expect("unsafe-cache discovery should complete");

        assert_eq!(
            frontend.available_features(),
            SUPPORTED_VIRTIO_FEATURES & !VIRTIO_BLK_F_FLUSH
        );
        assert_eq!(frontend.config_bytes(), &config_bytes);
        peer.join().expect("discovery peer should finish");
    }

    #[test]
    fn vhost_user_discovery_rejects_missing_mandatory_features_before_later_requests() {
        for features in [
            SUPPORTED_VIRTIO_FEATURES & !VIRTIO_F_VERSION_1,
            SUPPORTED_VIRTIO_FEATURES & !VHOST_USER_F_PROTOCOL_FEATURES,
        ] {
            let (frontend_stream, peer_stream) =
                UnixStream::pair().expect("vhost-user stream pair should open");
            let peer = spawn_test_vhost_user_discovery_peer(
                peer_stream,
                features,
                VHOST_USER_PROTOCOL_F_CONFIG,
                TestVhostUserConfigReply::Exact([0; VIRTIO_BLOCK_CONFIG_SIZE]),
            );

            let error = PreparedVhostUserBlockFrontend::discover(
                frontend_stream,
                DriveCacheType::Unsafe,
                Duration::from_secs(2),
            )
            .expect_err("missing mandatory feature should fail discovery");

            assert!(matches!(
                error,
                PreparedVhostUserBlockFrontendError::MissingRequiredVirtioFeatures
            ));
            peer.join().expect("discovery peer should finish");
        }
    }

    #[test]
    fn vhost_user_discovery_rejects_missing_config_and_invalid_config_replies() {
        let (frontend_stream, peer_stream) =
            UnixStream::pair().expect("vhost-user stream pair should open");
        let peer = spawn_test_vhost_user_discovery_peer(
            peer_stream,
            SUPPORTED_VIRTIO_FEATURES,
            VHOST_USER_PROTOCOL_F_REPLY_ACK,
            TestVhostUserConfigReply::Exact([0; VIRTIO_BLOCK_CONFIG_SIZE]),
        );
        let error = PreparedVhostUserBlockFrontend::discover(
            frontend_stream,
            DriveCacheType::Unsafe,
            Duration::from_secs(2),
        )
        .expect_err("missing CONFIG protocol feature should fail discovery");
        assert!(matches!(
            error,
            PreparedVhostUserBlockFrontendError::MissingConfigProtocolFeature
        ));
        peer.join().expect("discovery peer should finish");

        for config_reply in [
            TestVhostUserConfigReply::BackendFailure,
            TestVhostUserConfigReply::Malformed,
        ] {
            let (frontend_stream, peer_stream) =
                UnixStream::pair().expect("vhost-user stream pair should open");
            let peer = spawn_test_vhost_user_discovery_peer(
                peer_stream,
                SUPPORTED_VIRTIO_FEATURES,
                VHOST_USER_PROTOCOL_F_CONFIG,
                config_reply,
            );
            let error = PreparedVhostUserBlockFrontend::discover(
                frontend_stream,
                DriveCacheType::Unsafe,
                Duration::from_secs(2),
            )
            .expect_err("invalid backend config should fail discovery");
            assert!(matches!(
                error,
                PreparedVhostUserBlockFrontendError::Frontend(_)
                    | PreparedVhostUserBlockFrontendError::InvalidConfig
            ));
            assert!(!error.to_string().contains("0x5a"));
            peer.join().expect("discovery peer should finish");
        }
    }

    #[test]
    fn vhost_user_block_rejects_anonymous_guest_memory_before_activation() {
        let (frontend_stream, peer_stream) =
            UnixStream::pair().expect("vhost-user stream pair should open");
        let peer = spawn_test_vhost_user_discovery_peer(
            peer_stream,
            SUPPORTED_VIRTIO_FEATURES,
            VHOST_USER_PROTOCOL_F_CONFIG,
            TestVhostUserConfigReply::Exact([0; VIRTIO_BLOCK_CONFIG_SIZE]),
        );
        let frontend = PreparedVhostUserBlockFrontend::discover(
            frontend_stream,
            DriveCacheType::Unsafe,
            Duration::from_secs(2),
        )
        .expect("vhost-user discovery should complete");
        peer.join().expect("discovery peer should finish");
        let config = DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
            .with_socket("/private/test-vhost.sock")
            .validate()
            .expect("vhost-user drive should validate");
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test memory range should validate"),
        ])
        .expect("test memory layout should validate");
        let memory =
            GuestMemory::allocate(&layout).expect("anonymous guest memory should allocate");

        let error = PreparedBlockDevice::from_config_with_vhost_user(&config, frontend, &memory)
            .expect_err("vhost-user block should reject anonymous memory");

        assert!(matches!(
            error,
            PreparedBlockDeviceError::PrepareVhostMemory {
                source: PreparedVhostUserBlockMemoryError::AnonymousMemory,
                ..
            }
        ));
        assert!(!error.to_string().contains("private"));
    }

    #[test]
    fn vhost_user_block_rejects_guest_feature_and_queue_range_before_protocol_commit() {
        let (config_space, mut device, _memory) = prepare_disconnected_test_vhost_user_device();
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, true);
        let guest_features = config_space.available_features() & !VIRTIO_F_VERSION_1;
        let registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BLOCK_DEVICE_ID,
            config_space.available_features(),
        )
        .with_runtime_state([0, 1], guest_features, DRIVER_OK_STATUS);

        let error = device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("missing guest VERSION_1 should fail before protocol commit");

        assert!(matches!(
            error,
            VirtioBlockDeviceActivationError::UnsupportedGuestFeatures
        ));
        assert!(!device.is_activated());
        assert!(device.vhost_user_call_fd().is_some());

        let (config_space, mut device, _memory) = prepare_disconnected_test_vhost_user_device();
        let queues = configured_mmio_queue_with_device_ring(
            TEST_QUEUE_SIZE,
            u32::try_from(TEST_MEMORY_SIZE - 4).expect("test address should fit u32"),
            0,
            true,
        );
        let registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BLOCK_DEVICE_ID,
            config_space.available_features(),
        )
        .with_runtime_state([0, 1], config_space.available_features(), DRIVER_OK_STATUS);

        let error = device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("queue outside exported memory should fail before protocol commit");

        assert!(matches!(
            error,
            VirtioBlockDeviceActivationError::QueueRange
        ));
        assert!(!device.is_activated());
        assert!(device.vhost_user_call_fd().is_some());
    }

    #[test]
    fn vhost_user_block_activation_stops_after_first_backend_rejection() {
        let (frontend_stream, mut peer_stream) =
            UnixStream::pair().expect("vhost-user stream pair should open");
        let peer = thread::spawn(move || {
            peer_stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("peer read timeout should set");
            peer_stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .expect("peer write timeout should set");
            expect_test_vhost_user_request(&mut peer_stream, 3, false, 0);
            expect_test_vhost_user_request(&mut peer_stream, 1, false, 0);
            send_test_vhost_user_reply(
                &mut peer_stream,
                1,
                &SUPPORTED_VIRTIO_FEATURES.to_ne_bytes(),
            );
            expect_test_vhost_user_request(&mut peer_stream, 15, false, 0);
            let protocols = VHOST_USER_PROTOCOL_F_CONFIG | VHOST_USER_PROTOCOL_F_REPLY_ACK;
            send_test_vhost_user_reply(&mut peer_stream, 15, &protocols.to_ne_bytes());
            expect_test_vhost_user_request(&mut peer_stream, 16, false, 0);
            expect_test_vhost_user_request(&mut peer_stream, 24, false, 0);
            let mut config_reply = Vec::with_capacity(12 + VIRTIO_BLOCK_CONFIG_SIZE);
            config_reply.extend_from_slice(&0_u32.to_ne_bytes());
            config_reply.extend_from_slice(&(VIRTIO_BLOCK_CONFIG_SIZE as u32).to_ne_bytes());
            config_reply.extend_from_slice(&1_u32.to_ne_bytes());
            config_reply.resize(12 + VIRTIO_BLOCK_CONFIG_SIZE, 0);
            send_test_vhost_user_reply(&mut peer_stream, 24, &config_reply);

            let set_features = expect_test_vhost_user_request(&mut peer_stream, 2, true, 0);
            assert_eq!(
                test_vhost_user_u64(&set_features.body, 0),
                SUPPORTED_VIRTIO_FEATURES
            );
            send_test_vhost_user_reply(&mut peer_stream, 2, &1_u64.to_ne_bytes());
            let mut later_byte = [0_u8; 1];
            assert_eq!(
                peer_stream
                    .read(&mut later_byte)
                    .expect("terminal frontend should close its stream"),
                0,
                "frontend must not send a memory table after SET_FEATURES rejection"
            );
        });
        let frontend = PreparedVhostUserBlockFrontend::discover(
            frontend_stream,
            DriveCacheType::Writeback,
            Duration::from_secs(2),
        )
        .expect("vhost-user discovery should complete");
        let config = DriveConfigInput::new_without_path_on_host("vhost", "vhost", false)
            .with_socket("/private/test-vhost.sock")
            .with_cache_type(DriveCacheType::Writeback)
            .validate()
            .expect("vhost-user drive should validate");
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test shared memory range should validate"),
        ])
        .expect("test shared memory layout should validate");
        let memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("test shared guest memory should allocate");
        let prepared = PreparedBlockDevice::from_config_with_vhost_user(&config, frontend, &memory)
            .expect("vhost-user block should prepare");
        let (_, _, config_space, mut device) = prepared.into_parts();
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, true);
        let registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BLOCK_DEVICE_ID,
            config_space.available_features(),
        )
        .with_runtime_state([0, 1], config_space.available_features(), DRIVER_OK_STATUS);

        let error = device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("backend SET_FEATURES rejection should fail activation");

        assert!(matches!(
            error,
            VirtioBlockDeviceActivationError::Frontend(_)
        ));
        assert!(!device.is_activated());
        assert_eq!(device.vhost_user_call_fd(), None);
        assert!(matches!(
            device.activate_block(VirtioMmioDeviceActivation::new(&registers, &queues)),
            Err(VirtioBlockDeviceActivationError::Terminal)
        ));
        peer.join().expect("rejecting peer should finish");
    }

    #[test]
    fn block_device_activation_enables_indirect_descriptors_when_negotiated() {
        let file = temp_file("block-device-activate-indirect.img", &sector_payload(0x11));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue_with_features(
            &mut handler,
            4,
            TEST_USED_RING,
            INDIRECT_DESC_DRIVER_FEATURE,
        );

        activate_block_notification_handler(&mut handler);

        let active_queue = handler
            .activation_handler()
            .active_queue()
            .expect("active queue should be stored");
        assert!(
            active_queue
                .available_ring()
                .descriptor_chain_options()
                .indirect_descriptors()
        );
    }

    #[test]
    fn block_device_activation_runs_through_mmio_register_handler_and_reset() {
        let file = temp_file("block-device-mmio-handler.img", &sector_payload(0x12));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let config = VirtioBlockConfigSpace::from_backing(&backing, DriveCacheType::Unsafe);
        let device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID);
        let mut handler = VirtioMmioRegisterHandler::with_device_config_and_activation(
            VIRTIO_BLOCK_DEVICE_ID,
            config.available_features(),
            &VIRTIO_BLOCK_QUEUE_SIZES,
            config,
            device,
        )
        .expect("block register handler should build");
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
        handler
            .write_register(VirtioMmioRegister::QueueNum, 4)
            .expect("queue size should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDescLow,
                guest_address_low(TEST_DESCRIPTOR_TABLE),
            )
            .expect("queue descriptor table should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDriverLow,
                guest_address_low(TEST_AVAILABLE_RING),
            )
            .expect("queue driver ring should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueDeviceLow,
                guest_address_low(TEST_USED_RING),
            )
            .expect("queue device ring should write");
        handler
            .write_register(VirtioMmioRegister::QueueReady, 1)
            .expect("queue ready should write");

        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("DRIVER_OK should activate block device");

        assert!(handler.is_device_activated());
        let active_queue = handler
            .activation_handler()
            .active_queue()
            .expect("block device should store active queue");
        assert_eq!(active_queue.available_ring().queue_size(), 4);
        assert_eq!(active_queue.used_ring().used_ring(), TEST_USED_RING);

        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_INIT)
            .expect("INIT status should reset block activation state");

        assert!(!handler.is_device_activated());
        assert!(handler.activation_handler().active_queue().is_none());
    }

    #[test]
    fn block_device_activation_rejects_not_ready_queue_without_stale_state() {
        let file = temp_file("block-device-not-ready.img", &sector_payload(0x22));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID);
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, false);
        let registers = block_device_registers();

        let error = device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("not-ready queue should not activate");

        assert!(matches!(
            error,
            VirtioBlockDeviceActivationError::QueueBuild {
                queue_index: 0,
                source: VirtioBlockQueueBuildError::QueueNotReady,
            }
        ));
        assert!(device.active_queue().is_none());
        assert!(!device.is_activated());
    }

    #[test]
    fn block_device_activation_rejects_invalid_used_ring_without_stale_state() {
        let file = temp_file("block-device-invalid-used.img", &sector_payload(0x33));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID);
        let queues =
            configured_mmio_queue_with_device_ring(TEST_QUEUE_SIZE, u32::MAX - 3, u32::MAX, true);
        let registers = block_device_registers();

        let error = device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &queues))
            .expect_err("invalid used ring should not activate");

        match error {
            VirtioBlockDeviceActivationError::QueueBuild {
                queue_index: 0,
                source: VirtioBlockQueueBuildError::UsedRing { source },
            } => match source {
                VirtqueueUsedRingError::UsedRingRangeOverflow {
                    used_ring,
                    queue_size,
                } => {
                    assert_eq!(used_ring, GuestAddress::new(u64::MAX - 3));
                    assert_eq!(queue_size, TEST_QUEUE_SIZE);
                }
                other => panic!("expected used ring overflow, got {other:?}"),
            },
            other => panic!("expected queue build error, got {other:?}"),
        }
        assert!(device.active_queue().is_none());
        assert!(!device.is_activated());
    }

    #[test]
    fn block_device_activation_reset_clears_active_queue_and_allows_retry() {
        let file = temp_file("block-device-reset.img", &sector_payload(0x44));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID);
        let registers = block_device_registers();
        let first_queues = configured_mmio_queue(4, true);

        device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &first_queues))
            .expect("first activation should succeed");
        assert!(device.is_activated());

        VirtioMmioDeviceActivationHandler::reset(&mut device);

        assert!(!device.is_activated());
        assert!(device.active_queue().is_none());

        let second_queues = configured_mmio_queue(TEST_QUEUE_SIZE, true);
        device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &second_queues))
            .expect("second activation should succeed after reset");

        assert_eq!(
            device
                .active_queue()
                .expect("active queue should be present")
                .available_ring()
                .queue_size(),
            TEST_QUEUE_SIZE
        );
    }

    #[test]
    fn block_device_activation_rejects_duplicate_activation_without_replacing_queue() {
        let file = temp_file("block-device-duplicate.img", &sector_payload(0x55));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID);
        let registers = block_device_registers();
        let first_queues = configured_mmio_queue(4, true);
        let second_queues = configured_mmio_queue(TEST_QUEUE_SIZE, true);

        device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &first_queues))
            .expect("first activation should succeed");

        let error = device
            .activate_block(VirtioMmioDeviceActivation::new(&registers, &second_queues))
            .expect_err("duplicate activation should fail");

        assert!(matches!(
            error,
            VirtioBlockDeviceActivationError::AlreadyActive
        ));
        assert_eq!(
            device
                .active_queue()
                .expect("original queue should remain active")
                .available_ring()
                .queue_size(),
            4
        );
    }

    #[test]
    fn block_device_activation_trait_error_is_generic_handler_error() {
        let file = temp_file("block-device-trait-error.img", &sector_payload(0x66));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut device = VirtioBlockDevice::new(backing, TEST_DEVICE_ID);
        let queues = configured_mmio_queue(TEST_QUEUE_SIZE, false);
        let registers = block_device_registers();

        let error = VirtioMmioDeviceActivationHandler::activate(
            &mut device,
            VirtioMmioDeviceActivation::new(&registers, &queues),
        )
        .expect_err("trait activation should fail with generic handler error");

        match error {
            VirtioMmioDeviceActivationError::Handler { source } => {
                assert_eq!(
                    source.to_string(),
                    "failed to activate virtio-block queue 0: virtio-block queue is not ready"
                );
            }
        }
        assert!(device.active_queue().is_none());
    }

    #[test]
    fn block_device_notification_dispatch_without_pending_notification_is_noop() {
        let mut memory = request_memory();
        let file = temp_file("block-notify-noop.img", &sector_payload(0x71));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue(&mut handler, 4, TEST_USED_RING);
        activate_block_notification_handler(&mut handler);

        let dispatch = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect("no pending notification should not fail");

        assert!(dispatch.drained_notifications().is_empty());
        assert!(dispatch.queue_dispatch().is_none());
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        let active_queue = handler
            .activation_handler()
            .active_queue()
            .expect("block queue should remain active");
        assert_eq!(active_queue.available_ring().next_avail(), 0);
        assert_eq!(active_queue.used_ring().next_used(), 0);
    }

    #[test]
    fn block_device_notification_dispatch_empty_queue_has_no_interrupt() {
        let mut memory = request_memory();
        let file = temp_file("block-notify-empty-queue.img", &sector_payload(0x72));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue(&mut handler, 4, TEST_USED_RING);
        activate_block_notification_handler(&mut handler);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        let notification = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect("empty queue notification dispatch should succeed");

        assert_eq!(notification.drained_notifications(), [0]);
        let dispatch = notification
            .queue_dispatch()
            .expect("queue dispatch summary should be present");
        assert_eq!(dispatch.processed_requests(), 0);
        assert_eq!(dispatch.successful_requests(), 0);
        assert!(!notification.needs_queue_interrupt());
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        let active_queue = handler
            .activation_handler()
            .active_queue()
            .expect("block queue should remain active");
        assert_eq!(active_queue.available_ring().next_avail(), 0);
        assert_eq!(active_queue.used_ring().next_used(), 0);
    }

    #[test]
    fn block_device_notification_dispatch_rejects_pending_notification_without_activation() {
        let mut memory = request_memory();
        let file = temp_file("block-notify-inactive.img", &sector_payload(0x73));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
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
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect_err("unconfigured queue should fail block activation");
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should record after failed DRIVER_OK");

        let error = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect_err("inactive block queue should reject notification dispatch");

        assert!(matches!(
            error,
            VirtioBlockDeviceNotificationError::Inactive { .. }
        ));
        assert_eq!(error.drained_notifications(), [0]);
        assert!(error.completed_dispatch().is_none());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        assert!(!handler.activation_handler().is_activated());
    }

    #[test]
    fn block_device_notification_dispatch_executes_queued_request() {
        let mut memory = request_memory();
        let payload = sector_payload(0x74);
        let file = temp_file("block-notify-read.img", &payload);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue(&mut handler, 4, TEST_USED_RING);
        activate_block_notification_handler(&mut handler);
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        let notification = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect("notification dispatch should succeed");

        assert_eq!(notification.drained_notifications(), [0]);
        let dispatch = notification
            .queue_dispatch()
            .expect("queue dispatch summary should be present");
        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert!(notification.needs_queue_interrupt());
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        acknowledge_queue_interrupt(&mut handler);
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(
            read_used_element(&memory, 0),
            (
                0,
                VIRTIO_BLOCK_SECTOR_SIZE as u32 + VIRTIO_BLOCK_STATUS_SIZE,
            )
        );
    }

    #[test]
    fn block_device_notification_dispatch_suppresses_interrupt_with_event_idx() {
        let queue_size = 4;
        let mut memory = request_memory();
        let payload = sector_payload(0x7a);
        let file = temp_file("block-notify-event-idx-suppressed.img", &payload);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue_with_event_idx(
            &mut handler,
            queue_size,
            TEST_USED_RING,
        );
        activate_block_notification_handler(&mut handler);
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        write_available_used_event(&mut memory, queue_size, 1);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        let notification = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect("event-idx notification dispatch should succeed");

        assert_eq!(notification.drained_notifications(), [0]);
        let dispatch = notification
            .queue_dispatch()
            .expect("queue dispatch summary should be present");
        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert!(!notification.needs_queue_interrupt());
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(
            handler
                .activation_handler()
                .active_queue()
                .expect("block queue should remain active")
                .event_idx_enabled()
        );
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(
            read_used_element(&memory, 0),
            (
                0,
                VIRTIO_BLOCK_SECTOR_SIZE as u32 + VIRTIO_BLOCK_STATUS_SIZE,
            )
        );
        assert_eq!(read_used_avail_event(&memory, queue_size), 1);
    }

    #[test]
    fn block_device_notification_dispatch_interrupts_at_event_idx_threshold() {
        let queue_size = 4;
        let mut memory = request_memory();
        let payload = sector_payload(0x7b);
        let file = temp_file("block-notify-event-idx-threshold.img", &payload);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue_with_event_idx(
            &mut handler,
            queue_size,
            TEST_USED_RING,
        );
        activate_block_notification_handler(&mut handler);
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        write_available_used_event(&mut memory, queue_size, 0);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        let notification = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect("event-idx threshold dispatch should succeed");

        assert_eq!(notification.drained_notifications(), [0]);
        let dispatch = notification
            .queue_dispatch()
            .expect("queue dispatch summary should be present");
        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert!(notification.needs_queue_interrupt());
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(
            handler
                .activation_handler()
                .active_queue()
                .expect("block queue should remain active")
                .event_idx_enabled()
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(
            read_used_element(&memory, 0),
            (
                0,
                VIRTIO_BLOCK_SECTOR_SIZE as u32 + VIRTIO_BLOCK_STATUS_SIZE,
            )
        );
    }

    #[test]
    fn block_device_notification_dispatch_preserves_suppressed_partial_event_idx_error() {
        let queue_size = 4;
        let mut memory = request_memory();
        let file = temp_file(
            "block-notify-event-idx-partial-dispatch-error.img",
            &sector_payload(0x7c),
        );
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue_with_event_idx(
            &mut handler,
            queue_size,
            TEST_USED_RING,
        );
        activate_block_notification_handler(&mut handler);
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            0,
            HEADER_ADDR,
            None,
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        write_available_used_event(&mut memory, queue_size, 1);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        let error = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect_err("invalid second available head should fail notification dispatch");

        match &error {
            VirtioBlockDeviceNotificationError::QueueDispatch {
                source: VirtioBlockQueueDispatchError::AvailableRing { .. },
                ..
            } => {}
            other => panic!("expected available ring dispatch error, got {other:?}"),
        }
        assert_eq!(error.drained_notifications(), [0]);
        let completed_dispatch = error
            .completed_dispatch()
            .expect("queue dispatch error should preserve partial summary");
        assert_eq!(completed_dispatch.processed_requests(), 1);
        assert_eq!(completed_dispatch.successful_requests(), 1);
        assert!(!completed_dispatch.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        let active_queue = handler
            .activation_handler()
            .active_queue()
            .expect("block queue should remain active");
        assert!(active_queue.event_idx_enabled());
        assert_eq!(active_queue.available_ring().next_avail(), 1);
        assert_eq!(active_queue.used_ring().next_used(), 1);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
    }

    #[test]
    fn block_device_notification_dispatch_preserves_queue_dispatch_error() {
        let mut memory = request_memory();
        let file = temp_file("block-notify-dispatch-error.img", &sector_payload(0x75));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue(&mut handler, 4, TEST_USED_RING);
        activate_block_notification_handler(&mut handler);
        write_guest_u16(
            &mut memory,
            available_ring_idx_address(),
            TEST_QUEUE_SIZE + 1,
        );
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        let error = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect_err("invalid available ring should fail notification dispatch");

        match &error {
            VirtioBlockDeviceNotificationError::QueueDispatch {
                source: VirtioBlockQueueDispatchError::AvailableRing { .. },
                ..
            } => {}
            other => panic!("expected available ring dispatch error, got {other:?}"),
        }
        assert_eq!(error.drained_notifications(), [0]);
        let completed_dispatch = error
            .completed_dispatch()
            .expect("queue dispatch error should preserve partial summary");
        assert_eq!(completed_dispatch.processed_requests(), 0);
        assert!(!completed_dispatch.needs_queue_interrupt());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn block_device_notification_dispatch_preserves_partial_queue_dispatch_error() {
        let mut memory = request_memory();
        let file = temp_file(
            "block-notify-partial-dispatch-error.img",
            &sector_payload(0x76),
        );
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue(&mut handler, 4, TEST_USED_RING);
        activate_block_notification_handler(&mut handler);
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            0,
            HEADER_ADDR,
            None,
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        let error = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect_err("invalid second available head should fail notification dispatch");

        match &error {
            VirtioBlockDeviceNotificationError::QueueDispatch {
                source: VirtioBlockQueueDispatchError::AvailableRing { .. },
                ..
            } => {}
            other => panic!("expected available ring dispatch error, got {other:?}"),
        }
        assert_eq!(error.drained_notifications(), [0]);
        let completed_dispatch = error
            .completed_dispatch()
            .expect("queue dispatch error should preserve partial summary");
        assert_eq!(completed_dispatch.processed_requests(), 1);
        assert_eq!(completed_dispatch.successful_requests(), 1);
        assert!(completed_dispatch.needs_queue_interrupt());
        assert_eq!(
            read_interrupt_status(&handler),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
        let active_queue = handler
            .activation_handler()
            .active_queue()
            .expect("block queue should remain active");
        assert_eq!(active_queue.available_ring().next_avail(), 1);
        assert_eq!(active_queue.used_ring().next_used(), 1);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
    }

    #[test]
    fn block_device_notification_dispatch_reset_clears_active_queue_and_notification() {
        let mut memory = request_memory();
        let file = temp_file("block-notify-reset.img", &sector_payload(0x77));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &VIRTIO_BLOCK_QUEUE_SIZES);
        configure_block_notification_handler_queue(&mut handler, 4, TEST_USED_RING);
        activate_block_notification_handler(&mut handler);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should write");

        handler.reset();

        let dispatch = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect("reset handler should have no notification to dispatch");

        assert!(dispatch.drained_notifications().is_empty());
        assert!(dispatch.queue_dispatch().is_none());
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(!handler.activation_handler().is_activated());
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn block_device_notification_dispatch_rejects_unsupported_queue_notification() {
        let mut memory = request_memory();
        let file = temp_file("block-notify-unsupported-queue.img", &sector_payload(0x78));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &[TEST_QUEUE_SIZE, TEST_QUEUE_SIZE]);
        configure_block_notification_handler_queue(&mut handler, 4, TEST_USED_RING);
        activate_block_notification_handler(&mut handler);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 1)
            .expect("queue one notification should write on two-queue handler");

        let error = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect_err("virtio-block should reject nonzero queue notifications");

        match error {
            VirtioBlockDeviceNotificationError::UnsupportedQueue {
                queue_index,
                drained_notifications,
            } => {
                assert_eq!(queue_index, 1);
                assert_eq!(drained_notifications, vec![1]);
            }
            other => panic!("expected unsupported queue error, got {other:?}"),
        }
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        let active_queue = handler
            .activation_handler()
            .active_queue()
            .expect("block queue should remain active");
        assert_eq!(active_queue.available_ring().next_avail(), 0);
        assert_eq!(active_queue.used_ring().next_used(), 0);
    }

    #[test]
    fn block_device_notification_dispatch_rejects_mixed_unsupported_queue_without_dispatch() {
        let mut memory = request_memory();
        let payload = sector_payload(0x79);
        let file = temp_file("block-notify-mixed-unsupported-queue.img", &payload);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut handler = block_notification_handler(backing, &[TEST_QUEUE_SIZE, TEST_QUEUE_SIZE]);
        configure_block_notification_handler_queue(&mut handler, 4, TEST_USED_RING);
        activate_block_notification_handler(&mut handler);
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue zero notification should write");
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 1)
            .expect("queue one notification should write");

        let error = handler
            .dispatch_block_queue_notifications(&mut memory)
            .expect_err("unsupported queue should prevent partial notification dispatch");

        match error {
            VirtioBlockDeviceNotificationError::UnsupportedQueue {
                queue_index,
                drained_notifications,
            } => {
                assert_eq!(queue_index, 1);
                assert_eq!(drained_notifications, vec![0, 1]);
            }
            other => panic!("expected unsupported queue error, got {other:?}"),
        }
        assert_eq!(read_interrupt_status(&handler), 0);
        assert!(handler.pending_queue_notifications().is_empty());
        assert_ne!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        let active_queue = handler
            .activation_handler()
            .active_queue()
            .expect("block queue should remain active");
        assert_eq!(active_queue.available_ring().next_avail(), 0);
        assert_eq!(active_queue.used_ring().next_used(), 0);
    }

    #[test]
    fn block_queue_build_errors_display_and_preserve_sources() {
        let available = VirtioBlockQueueBuildError::AvailableRing {
            source: VirtqueueAvailableRingError::InvalidQueueSize { queue_size: 0 },
        };
        let used = VirtioBlockQueueBuildError::UsedRing {
            source: VirtqueueUsedRingError::InvalidQueueSize { queue_size: 0 },
        };

        assert_eq!(
            available.to_string(),
            "failed to build virtio-block available ring from queue state: virtqueue size 0 must be a nonzero power of two"
        );
        assert_eq!(
            available
                .source()
                .expect("available ring source should be present")
                .to_string(),
            "virtqueue size 0 must be a nonzero power of two"
        );
        assert_eq!(
            used.to_string(),
            "failed to build virtio-block used ring from queue state: virtqueue size 0 must be a nonzero power of two"
        );
        assert_eq!(
            used.source()
                .expect("used ring source should be present")
                .to_string(),
            "virtqueue size 0 must be a nonzero power of two"
        );
    }

    #[test]
    fn executes_request_ending_at_capacity_boundary() {
        let mut memory = request_memory();
        let payload = sector_payload(0x77);
        let mut file_bytes = vec![0; (VIRTIO_BLOCK_SECTOR_SIZE * 16) as usize];
        let offset = (VIRTIO_BLOCK_SECTOR_SIZE * 15) as usize;
        file_bytes[offset..offset + payload.len()].copy_from_slice(&payload);
        let file = temp_file("execute-boundary.img", &file_bytes);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let chain = request_chain(
            &mut memory,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            15,
            &[
                TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
                TestDescriptor::writable(DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, Some(2)),
                TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
            ],
        );
        let request = parse_request_with_capacity(&memory, &chain, &backing);

        let execution = request.execute(&mut memory, &backing, TEST_DEVICE_ID);

        assert_eq!(
            execution.completion(),
            VirtioBlockRequestCompletion::new(
                0,
                VIRTIO_BLOCK_SECTOR_SIZE as u32 + VIRTIO_BLOCK_STATUS_SIZE,
            )
        );
        assert!(matches!(
            execution.outcome(),
            VirtioBlockRequestExecutionOutcome::Ok
        ));
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
    }

    #[test]
    fn block_queue_dispatch_empty_queue_is_noop() {
        let mut memory = request_memory();
        let file = temp_file("queue-empty.img", &[]);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect("empty queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 0);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.read_count(), 0);
        assert_eq!(dispatch.write_count(), 0);
        assert_eq!(dispatch.flush_count(), 0);
        assert_eq!(dispatch.read_bytes(), 0);
        assert_eq!(dispatch.write_bytes(), 0);
        assert_eq!(dispatch.parse_failures(), 0);
        assert_eq!(dispatch.io_errors(), 0);
        assert_eq!(dispatch.rate_limiter_retry_after(), None);
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 0);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_eq!(read_used_index(&memory), 0);
    }

    #[test]
    fn block_queue_dispatch_executes_read_and_publishes_used_element() {
        let mut memory = request_memory();
        let payload = sector_payload(0x8a);
        let file = temp_file("queue-read.img", &payload);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect("read queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(dispatch.read_count(), 1);
        assert_eq!(dispatch.read_bytes(), VIRTIO_BLOCK_SECTOR_SIZE);
        assert_eq!(dispatch.write_count(), 0);
        assert_eq!(dispatch.write_bytes(), 0);
        assert!(dispatch.read_latency_aggregate().is_some());
        assert!(dispatch.write_latency_aggregate().is_none());
        assert_eq!(dispatch.flush_count(), 0);
        assert_eq!(dispatch.parse_failures(), 0);
        assert_eq!(dispatch.io_errors(), 0);
        assert_eq!(dispatch.unsupported_requests(), 0);
        assert_eq!(dispatch.status_write_failures(), 0);
        assert_eq!(dispatch.rate_limiter_retry_after(), None);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(
            read_used_element(&memory, 0),
            (
                0,
                VIRTIO_BLOCK_SECTOR_SIZE as u32 + VIRTIO_BLOCK_STATUS_SIZE,
            )
        );
    }

    #[derive(Debug)]
    struct QueueAsyncReadGateHost {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        byte: u8,
        result: Option<BlockAsyncTransferResult>,
    }

    impl BlockAsyncHostIo for QueueAsyncReadGateHost {
        fn read_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            destination: &mut [u8],
        ) -> BlockAsyncTransferResult {
            self.entered
                .send(())
                .expect("queue test observer should remain live");
            self.release
                .lock()
                .expect("queue test release gate should lock")
                .recv_timeout(Duration::from_secs(2))
                .expect("queue test should release host read");
            destination.fill(self.byte);
            self.result
                .unwrap_or_else(|| BlockAsyncTransferResult::complete(destination.len()))
        }

        fn write_at(
            &self,
            _backing: &BlockFileBacking,
            _offset: u64,
            source: &[u8],
        ) -> BlockAsyncTransferResult {
            BlockAsyncTransferResult::complete(source.len())
        }

        fn flush(&self, _backing: &BlockFileBacking) -> Result<(), io::ErrorKind> {
            Ok(())
        }
    }

    #[test]
    fn async_block_queue_defers_status_used_and_metrics_until_host_completion() {
        let mut memory = request_memory();
        memory
            .write_slice(&[0xff], STATUS_ADDR)
            .expect("status sentinel should initialize");
        let file = temp_file(
            "queue-async-read.img",
            &vec![0; VIRTIO_BLOCK_SECTOR_SIZE as usize],
        );
        let backing = Arc::new(open_backing(file.as_path(), true).expect("backing should open"));
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let runtime = SharedBlockAsyncRuntime::with_config_and_host(
            BlockAsyncExecutorConfig::new(1, 1, 1, 512, 512),
            Arc::new(QueueAsyncReadGateHost {
                entered: entered_sender,
                release: Mutex::new(release_receiver),
                byte: 0x6d,
                result: None,
            }),
        );
        let generation = runtime
            .bind_drive(Arc::clone(&backing), DriveCacheType::Unsafe)
            .expect("Async generation should bind");
        let mut queue = block_queue();

        let submitted = queue
            .dispatch_with_async_runtime(
                &mut memory,
                backing.as_ref(),
                TEST_DEVICE_ID,
                None,
                &runtime,
                generation,
            )
            .expect("Async request should submit");
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("host read should enter");
        assert_eq!(submitted.processed_requests(), 0);
        assert_eq!(read_status(&memory), 0xff);
        assert_eq!(read_used_index(&memory), 0);
        assert_eq!(queue.available_ring().next_avail(), 1);

        release_sender.send(()).expect("host read should release");
        runtime
            .quiesce_generation(generation, &mut memory)
            .expect("Async generation should finish");
        let completed = queue
            .dispatch_with_async_runtime(
                &mut memory,
                backing.as_ref(),
                TEST_DEVICE_ID,
                None,
                &runtime,
                generation,
            )
            .expect("completion-only dispatch should publish");

        assert_eq!(completed.processed_requests(), 1);
        assert_eq!(completed.successful_requests(), 1);
        assert_eq!(completed.read_count(), 1);
        assert_eq!(completed.read_bytes(), VIRTIO_BLOCK_SECTOR_SIZE);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(
            read_used_element(&memory, 0),
            (
                0,
                VIRTIO_BLOCK_SECTOR_SIZE as u32 + VIRTIO_BLOCK_STATUS_SIZE,
            )
        );
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            vec![0x6d; VIRTIO_BLOCK_SECTOR_SIZE as usize]
        );
        runtime
            .unbind_quiesced(generation)
            .expect("published generation should unbind");
        runtime
            .shutdown(&mut memory)
            .expect("Async runtime should shut down");
    }

    #[test]
    fn quiesced_async_partial_publication_exposes_uncertain_guest_completion() {
        let mut memory = request_memory();
        memory
            .write_slice(&[0xff], STATUS_ADDR)
            .expect("partial-publication status sentinel should initialize");
        let file = temp_file(
            "queue-async-partial-publication.img",
            &vec![0; VIRTIO_BLOCK_SECTOR_SIZE as usize],
        );
        let backing = Arc::new(open_backing(file.as_path(), true).expect("backing should open"));
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let runtime = SharedBlockAsyncRuntime::with_config_and_host(
            BlockAsyncExecutorConfig::new(1, 1, 1, 512, 512),
            Arc::new(QueueAsyncReadGateHost {
                entered: entered_sender,
                release: Mutex::new(release_receiver),
                byte: 0x3c,
                result: None,
            }),
        );
        let generation = runtime
            .bind_drive(Arc::clone(&backing), DriveCacheType::Unsafe)
            .expect("partial-publication generation should bind");
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            TEST_AVAILABLE_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("partial-publication available ring should build");
        let used = VirtqueueUsedRing::new(GuestAddress::new(TEST_MEMORY_SIZE - 4), TEST_QUEUE_SIZE)
            .expect("out-of-memory used ring geometry should remain arithmetically valid");
        let mut queue = VirtioBlockQueue::new(available, used);

        queue
            .dispatch_with_async_runtime(
                &mut memory,
                backing.as_ref(),
                TEST_DEVICE_ID,
                None,
                &runtime,
                generation,
            )
            .expect("partial-publication request should enter the host");
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("partial-publication host read should enter");
        runtime
            .stop_generations(&[generation])
            .expect("partial-publication admission should stop");
        release_sender
            .send(())
            .expect("partial-publication host read should release");
        runtime
            .quiesce_generation(generation, &mut memory)
            .expect("partial-publication generation should drain");

        let error = queue
            .publish_async_completions(
                &mut memory,
                &runtime,
                generation,
                &mut VirtioBlockQueueDispatch::default(),
            )
            .expect_err("invalid used ring should fail after writing request status");
        let VirtioBlockQueueDispatchError::UsedRing {
            completed_dispatch,
            descriptor_head,
            bytes_written_to_guest,
            ..
        } = error
        else {
            panic!("partial publication should report an uncertain used-ring write");
        };
        assert_eq!(*completed_dispatch, VirtioBlockQueueDispatch::default());
        assert_eq!(descriptor_head, 0);
        assert_eq!(
            bytes_written_to_guest,
            VIRTIO_BLOCK_SECTOR_SIZE as u32 + VIRTIO_BLOCK_STATUS_SIZE
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_eq!(
            runtime
                .pop_completion(generation)
                .expect("uncertain completion lookup should succeed"),
            None,
            "the owner consumed the compact completion before the ambiguous guest write"
        );
        runtime
            .resume_quiesced_generation(generation)
            .expect("runtime ownership alone can reopen after the completion is consumed");
        runtime
            .stop_generations(&[generation])
            .expect("cleanup generation should stop again");
        runtime
            .quiesce_generation(generation, &mut memory)
            .expect("cleanup generation should remain drained");
        runtime
            .unbind_quiesced(generation)
            .expect("cleanup generation should unbind");
        assert!(
            runtime
                .shutdown_if_idle()
                .expect("partial-publication executor should stop")
        );
    }

    #[test]
    fn async_block_queue_partial_read_failure_publishes_only_ioerr_status() {
        let mut memory = request_memory();
        memory
            .write_slice(&[0xff], STATUS_ADDR)
            .expect("status sentinel should initialize");
        let file = temp_file(
            "queue-async-partial-read.img",
            &vec![0; VIRTIO_BLOCK_SECTOR_SIZE as usize],
        );
        let backing = Arc::new(open_backing(file.as_path(), true).expect("backing should open"));
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let runtime = SharedBlockAsyncRuntime::with_config_and_host(
            BlockAsyncExecutorConfig::new(1, 1, 1, 512, 512),
            Arc::new(QueueAsyncReadGateHost {
                entered: entered_sender,
                release: Mutex::new(release_receiver),
                byte: 0x4c,
                result: Some(BlockAsyncTransferResult::failed(128, io::ErrorKind::Other)),
            }),
        );
        let generation = runtime
            .bind_drive(Arc::clone(&backing), DriveCacheType::Unsafe)
            .expect("Async generation should bind");
        let mut queue = block_queue();

        let submitted = queue
            .dispatch_with_async_runtime(
                &mut memory,
                backing.as_ref(),
                TEST_DEVICE_ID,
                None,
                &runtime,
                generation,
            )
            .expect("Async request should submit");
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("host read should enter");
        assert_eq!(submitted.processed_requests(), 0);
        assert_eq!(read_status(&memory), 0xff);
        assert_eq!(read_used_index(&memory), 0);

        release_sender.send(()).expect("host read should release");
        runtime
            .quiesce_generation(generation, &mut memory)
            .expect("Async generation should finish");
        let completed = queue
            .dispatch_with_async_runtime(
                &mut memory,
                backing.as_ref(),
                TEST_DEVICE_ID,
                None,
                &runtime,
                generation,
            )
            .expect("failed completion should publish");

        assert_eq!(completed.processed_requests(), 1);
        assert_eq!(completed.successful_requests(), 0);
        assert_eq!(completed.read_count(), 0);
        assert_eq!(completed.read_bytes(), 0);
        assert_eq!(completed.io_errors(), 1);
        assert!(completed.read_latency_aggregate().is_some());
        assert!(completed.needs_queue_interrupt());
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_IOERR);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
        let data = read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize);
        assert_eq!(&data[..128], &[0x4c; 128]);
        assert_eq!(&data[128..], &[0; VIRTIO_BLOCK_SECTOR_SIZE as usize - 128]);

        runtime
            .unbind_quiesced(generation)
            .expect("published generation should unbind");
        runtime
            .shutdown(&mut memory)
            .expect("Async runtime should shut down");
    }

    #[test]
    fn async_block_queue_guest_range_overflow_is_request_ioerr_not_runtime_failure() {
        let mut memory = request_memory();
        memory
            .write_slice(&[0xff], STATUS_ADDR)
            .expect("status sentinel should initialize");
        let file = temp_file(
            "queue-async-guest-overflow.img",
            &vec![0; VIRTIO_BLOCK_SECTOR_SIZE as usize],
        );
        let backing = Arc::new(open_backing(file.as_path(), true).expect("backing should open"));
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((
                GuestAddress::new(u64::MAX - 255),
                VIRTIO_BLOCK_SECTOR_SIZE as u32,
                true,
            )),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (_release_sender, release_receiver) = mpsc::channel();
        let runtime = SharedBlockAsyncRuntime::with_config_and_host(
            BlockAsyncExecutorConfig::new(1, 1, 1, 512, 512),
            Arc::new(QueueAsyncReadGateHost {
                entered: entered_sender,
                release: Mutex::new(release_receiver),
                byte: 0,
                result: None,
            }),
        );
        let generation = runtime
            .bind_drive(Arc::clone(&backing), DriveCacheType::Unsafe)
            .expect("Async generation should bind");
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch_with_async_runtime(
                &mut memory,
                backing.as_ref(),
                TEST_DEVICE_ID,
                None,
                &runtime,
                generation,
            )
            .expect("guest range failure should complete without failing the runtime");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.io_errors(), 1);
        assert!(dispatch.read_latency_aggregate().is_some());
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_IOERR);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
        assert!(matches!(
            entered_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert_eq!(runtime.outstanding_tasks().expect("runtime should lock"), 0);

        runtime
            .quiesce_generation(generation, &mut memory)
            .expect("empty generation should quiesce");
        runtime
            .unbind_quiesced(generation)
            .expect("empty generation should unbind");
        runtime
            .shutdown(&mut memory)
            .expect("Async runtime should shut down");
    }

    #[test]
    fn async_block_queue_capacity_pressure_preflights_before_limiter_consumption() {
        let mut memory = request_memory();
        let second_header = HEADER_ADDR
            .checked_add(0x100)
            .expect("second header address should not overflow");
        let second_data = DATA_ADDR
            .checked_add(VIRTIO_BLOCK_SECTOR_SIZE)
            .expect("second data address should not overflow");
        let second_status = STATUS_ADDR
            .checked_add(0x100)
            .expect("second status address should not overflow");
        memory
            .write_slice(&[0xaa], second_status)
            .expect("second status sentinel should initialize");
        let file = temp_file(
            "queue-async-capacity-pressure.img",
            &vec![0; (VIRTIO_BLOCK_SECTOR_SIZE * 2) as usize],
        );
        let backing = Arc::new(open_backing(file.as_path(), true).expect("backing should open"));
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            STATUS_ADDR,
        );
        write_queued_request(
            &mut memory,
            3,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            1,
            second_header,
            Some((second_data, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            second_status,
        );
        write_available_heads(&mut memory, &[0, 3]);
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let runtime = SharedBlockAsyncRuntime::with_config_and_host(
            BlockAsyncExecutorConfig::new(1, 1, 1, 512, 512),
            Arc::new(QueueAsyncReadGateHost {
                entered: entered_sender,
                release: Mutex::new(release_receiver),
                byte: 0x5e,
                result: None,
            }),
        );
        let generation = runtime
            .bind_drive(Arc::clone(&backing), DriveCacheType::Unsafe)
            .expect("Async generation should bind");
        let limiter_config =
            DriveRateLimiterConfig::new(None, Some(DriveTokenBucketConfig::new(2, None, 60_000)));
        let mut limiter =
            VirtioBlockRateLimiter::new(limiter_config).expect("rate limiter should be enabled");
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch_with_async_runtime(
                &mut memory,
                backing.as_ref(),
                TEST_DEVICE_ID,
                Some(&mut limiter),
                &runtime,
                generation,
            )
            .expect("capacity pressure should leave the second request pending");
        entered_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("first host read should enter");

        assert_eq!(dispatch.processed_requests(), 0);
        assert_eq!(dispatch.io_engine_throttled_events(), 1);
        assert_eq!(dispatch.rate_limiter_throttled_requests(), 0);
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(read_guest_bytes(&memory, second_status, 1), [0xaa]);
        let limiter_state = VirtioBlockRateLimiter::persisted_state_at(
            Some(limiter_config),
            Some(&limiter),
            Instant::now(),
        )
        .expect("limiter state should capture");
        assert_eq!(
            limiter_state
                .ops()
                .expect("ops limiter state should exist")
                .budget(),
            1,
            "only the admitted first request may consume an ops token"
        );
        let metrics = SharedBlockDeviceMetrics::default();
        metrics.record_queue_dispatch(&dispatch);
        assert_eq!(
            metrics.snapshot(),
            BlockDeviceMetrics::default().with_io_engine_throttled_events(1)
        );

        release_sender
            .send(())
            .expect("first host read should release");
        runtime
            .quiesce_generation(generation, &mut memory)
            .expect("entered request should quiesce");
        let mut completion_dispatch = VirtioBlockQueueDispatch::default();
        queue
            .publish_async_completions(&mut memory, &runtime, generation, &mut completion_dispatch)
            .expect("entered request should publish");
        assert_eq!(completion_dispatch.processed_requests(), 1);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_guest_bytes(&memory, second_status, 1), [0xaa]);
        runtime
            .unbind_quiesced(generation)
            .expect("published generation should unbind");
        runtime
            .shutdown(&mut memory)
            .expect("Async runtime should shut down");
    }

    #[test]
    fn block_queue_dispatch_executes_write_and_records_write_bytes() {
        let mut memory = request_memory();
        let payload = sector_payload(0x29);
        memory
            .write_slice(&payload, DATA_ADDR)
            .expect("guest data should write");
        let file = temp_file(
            "queue-write.img",
            &vec![0; VIRTIO_BLOCK_SECTOR_SIZE as usize],
        );
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, false)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect("write queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(dispatch.read_count(), 0);
        assert_eq!(dispatch.read_bytes(), 0);
        assert_eq!(dispatch.write_count(), 1);
        assert_eq!(dispatch.write_bytes(), VIRTIO_BLOCK_SECTOR_SIZE);
        assert!(dispatch.read_latency_aggregate().is_none());
        assert!(dispatch.write_latency_aggregate().is_some());
        assert_eq!(dispatch.flush_count(), 0);
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
        assert_eq!(fs::read(file.as_path()).expect("file should read"), payload);
    }

    #[test]
    fn block_queue_dispatch_indirect_read_publishes_outer_descriptor_head() {
        let mut memory = request_memory();
        let payload = sector_payload(0x8c);
        let file = temp_file("queue-indirect-read.img", &payload);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let outer_head = 4;
        let indirect_table_len = u32::try_from(3 * VIRTQUEUE_DESCRIPTOR_SIZE)
            .expect("indirect table length should fit in u32");
        write_request_header(&mut memory, HEADER_ADDR, VIRTIO_BLOCK_REQUEST_TYPE_IN, 0);
        write_descriptor(
            &mut memory,
            outer_head,
            TestDescriptor::indirect(TEST_INDIRECT_DESCRIPTOR_TABLE, indirect_table_len),
        );
        write_descriptor_at(
            &mut memory,
            TEST_INDIRECT_DESCRIPTOR_TABLE,
            0,
            TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
        );
        write_descriptor_at(
            &mut memory,
            TEST_INDIRECT_DESCRIPTOR_TABLE,
            1,
            TestDescriptor::writable(DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, Some(2)),
        );
        write_descriptor_at(
            &mut memory,
            TEST_INDIRECT_DESCRIPTOR_TABLE,
            2,
            TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
        );
        write_available_heads(&mut memory, &[outer_head]);
        let mut queue = block_queue_with_indirect_descriptors();

        let dispatch = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect("indirect read queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(
            read_used_element(&memory, 0),
            (
                u32::from(outer_head),
                VIRTIO_BLOCK_SECTOR_SIZE as u32 + VIRTIO_BLOCK_STATUS_SIZE,
            )
        );
    }

    #[test]
    fn block_queue_dispatch_drains_multiple_requests() {
        let mut memory = request_memory();
        let file = temp_file("queue-multiple.img", &sector_payload(0x9b));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let second_header = HEADER_ADDR
            .checked_add(0x100)
            .expect("second header address should not overflow");
        let second_status = STATUS_ADDR
            .checked_add(0x100)
            .expect("second status address should not overflow");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            0,
            HEADER_ADDR,
            None,
            STATUS_ADDR,
        );
        write_queued_request(
            &mut memory,
            2,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            0,
            second_header,
            None,
            second_status,
        );
        write_available_heads(&mut memory, &[0, 2]);
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect("multi-request queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 2);
        assert_eq!(dispatch.successful_requests(), 2);
        assert_eq!(dispatch.read_count(), 0);
        assert_eq!(dispatch.write_count(), 0);
        assert_eq!(dispatch.flush_count(), 2);
        assert_eq!(dispatch.read_bytes(), 0);
        assert_eq!(dispatch.write_bytes(), 0);
        assert!(dispatch.read_latency_aggregate().is_none());
        assert!(dispatch.write_latency_aggregate().is_none());
        assert_eq!(dispatch.parse_failures(), 0);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 2);
        assert_eq!(queue.used_ring().next_used(), 2);
        assert_eq!(read_used_index(&memory), 2);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
        assert_eq!(read_used_element(&memory, 1), (2, VIRTIO_BLOCK_STATUS_SIZE));
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(
            read_guest_bytes(&memory, second_status, 1),
            [VIRTIO_BLOCK_STATUS_OK]
        );
    }

    #[test]
    fn block_queue_dispatch_keeps_rate_limited_request_pending() {
        let now = Instant::now();
        let mut memory = request_memory();
        let first_payload = sector_payload(0x41);
        let second_payload = sector_payload(0x42);
        memory
            .write_slice(&first_payload, DATA_ADDR)
            .expect("first guest data should write");
        let second_data = DATA_ADDR
            .checked_add(VIRTIO_BLOCK_SECTOR_SIZE)
            .expect("second data address should not overflow");
        let second_header = HEADER_ADDR
            .checked_add(0x100)
            .expect("second header address should not overflow");
        let second_status = STATUS_ADDR
            .checked_add(0x100)
            .expect("second status address should not overflow");
        memory
            .write_slice(&second_payload, second_data)
            .expect("second guest data should write");
        memory
            .write_slice(&[0xaa], second_status)
            .expect("second status sentinel should write");
        let file = temp_file(
            "queue-rate-limited-pending.img",
            &vec![0; (VIRTIO_BLOCK_SECTOR_SIZE * 2) as usize],
        );
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, false)),
            STATUS_ADDR,
        );
        write_queued_request(
            &mut memory,
            3,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            1,
            second_header,
            Some((second_data, VIRTIO_BLOCK_SECTOR_SIZE as u32, false)),
            second_status,
        );
        write_available_heads(&mut memory, &[0, 3]);
        let mut queue = block_queue();
        let mut rate_limiter = VirtioBlockRateLimiter::new_at(
            DriveRateLimiterConfig::new(None, Some(DriveTokenBucketConfig::new(1, None, 1000))),
            now,
        )
        .expect("rate limiter should be enabled");

        let dispatch = queue
            .dispatch_with_rate_limiter_at(
                &mut memory,
                &backing,
                TEST_DEVICE_ID,
                Some(&mut rate_limiter),
                now,
            )
            .expect("rate-limited queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(dispatch.rate_limiter_throttled_requests(), 1);
        assert_eq!(
            dispatch.rate_limiter_retry_after(),
            Some(Duration::from_millis(1000))
        );
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
        assert_eq!(read_guest_bytes(&memory, second_status, 1), [0xaa]);
        let written_file = fs::read(file.as_path()).expect("file should read");
        assert_eq!(
            &written_file[..VIRTIO_BLOCK_SECTOR_SIZE as usize],
            first_payload.as_slice()
        );
        assert_eq!(
            &written_file[VIRTIO_BLOCK_SECTOR_SIZE as usize..],
            vec![0; VIRTIO_BLOCK_SECTOR_SIZE as usize].as_slice()
        );
    }

    #[test]
    fn block_queue_dispatch_retries_rate_limited_request_after_replenish() {
        let now = Instant::now();
        let mut memory = request_memory();
        let first_payload = sector_payload(0x51);
        let second_payload = sector_payload(0x52);
        memory
            .write_slice(&first_payload, DATA_ADDR)
            .expect("first guest data should write");
        let second_data = DATA_ADDR
            .checked_add(VIRTIO_BLOCK_SECTOR_SIZE)
            .expect("second data address should not overflow");
        let second_header = HEADER_ADDR
            .checked_add(0x100)
            .expect("second header address should not overflow");
        let second_status = STATUS_ADDR
            .checked_add(0x100)
            .expect("second status address should not overflow");
        memory
            .write_slice(&second_payload, second_data)
            .expect("second guest data should write");
        let file = temp_file(
            "queue-rate-limited-retry.img",
            &vec![0; (VIRTIO_BLOCK_SECTOR_SIZE * 2) as usize],
        );
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, false)),
            STATUS_ADDR,
        );
        write_queued_request(
            &mut memory,
            3,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            1,
            second_header,
            Some((second_data, VIRTIO_BLOCK_SECTOR_SIZE as u32, false)),
            second_status,
        );
        write_available_heads(&mut memory, &[0, 3]);
        let mut queue = block_queue();
        let mut rate_limiter = VirtioBlockRateLimiter::new_at(
            DriveRateLimiterConfig::new(None, Some(DriveTokenBucketConfig::new(1, None, 1000))),
            now,
        )
        .expect("rate limiter should be enabled");
        queue
            .dispatch_with_rate_limiter_at(
                &mut memory,
                &backing,
                TEST_DEVICE_ID,
                Some(&mut rate_limiter),
                now,
            )
            .expect("first dispatch should leave one request pending");

        let dispatch = queue
            .dispatch_with_rate_limiter_at(
                &mut memory,
                &backing,
                TEST_DEVICE_ID,
                Some(&mut rate_limiter),
                now + Duration::from_millis(1000),
            )
            .expect("replenished queue dispatch should succeed");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(dispatch.rate_limiter_throttled_requests(), 0);
        assert_eq!(dispatch.rate_limiter_retry_after(), None);
        assert_eq!(queue.available_ring().next_avail(), 2);
        assert_eq!(queue.used_ring().next_used(), 2);
        assert_eq!(read_used_index(&memory), 2);
        assert_eq!(read_used_element(&memory, 1), (3, VIRTIO_BLOCK_STATUS_SIZE));
        assert_eq!(
            read_guest_bytes(&memory, second_status, 1),
            [VIRTIO_BLOCK_STATUS_OK]
        );
        let written_file = fs::read(file.as_path()).expect("file should read");
        assert_eq!(
            &written_file[..VIRTIO_BLOCK_SECTOR_SIZE as usize],
            first_payload.as_slice()
        );
        assert_eq!(
            &written_file[VIRTIO_BLOCK_SECTOR_SIZE as usize..],
            second_payload.as_slice()
        );
    }

    #[test]
    fn block_rate_limiter_rolls_back_ops_when_bandwidth_throttles() {
        let now = Instant::now();
        let mut memory = request_memory();
        let first_payload = sector_payload(0x61);
        let second_payload = sector_payload(0x62);
        memory
            .write_slice(&first_payload, DATA_ADDR)
            .expect("first guest data should write");
        let second_data = DATA_ADDR
            .checked_add(VIRTIO_BLOCK_SECTOR_SIZE)
            .expect("second data address should not overflow");
        let second_header = HEADER_ADDR
            .checked_add(0x100)
            .expect("second header address should not overflow");
        let second_status = STATUS_ADDR
            .checked_add(0x100)
            .expect("second status address should not overflow");
        memory
            .write_slice(&second_payload, second_data)
            .expect("second guest data should write");
        let file = temp_file(
            "queue-rate-limited-rollback.img",
            &vec![0; (VIRTIO_BLOCK_SECTOR_SIZE * 2) as usize],
        );
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, false)),
            STATUS_ADDR,
        );
        write_queued_request(
            &mut memory,
            3,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            1,
            second_header,
            Some((second_data, VIRTIO_BLOCK_SECTOR_SIZE as u32, false)),
            second_status,
        );
        write_available_heads(&mut memory, &[0, 3]);
        let mut queue = block_queue();
        let mut rate_limiter = VirtioBlockRateLimiter::new_at(
            DriveRateLimiterConfig::new(
                Some(DriveTokenBucketConfig::new(
                    VIRTIO_BLOCK_SECTOR_SIZE,
                    None,
                    1000,
                )),
                Some(DriveTokenBucketConfig::new(2, None, 60_000)),
            ),
            now,
        )
        .expect("rate limiter should be enabled");
        let first = queue
            .dispatch_with_rate_limiter_at(
                &mut memory,
                &backing,
                TEST_DEVICE_ID,
                Some(&mut rate_limiter),
                now,
            )
            .expect("first dispatch should leave second request bandwidth-throttled");
        assert_eq!(first.rate_limiter_throttled_requests(), 1);
        assert_eq!(
            first.rate_limiter_retry_after(),
            Some(Duration::from_millis(1000))
        );

        let dispatch = queue
            .dispatch_with_rate_limiter_at(
                &mut memory,
                &backing,
                TEST_DEVICE_ID,
                Some(&mut rate_limiter),
                now + Duration::from_millis(1000),
            )
            .expect("bandwidth replenish should allow retry with restored op budget");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 1);
        assert_eq!(dispatch.rate_limiter_throttled_requests(), 0);
        assert_eq!(dispatch.rate_limiter_retry_after(), None);
        assert_eq!(queue.available_ring().next_avail(), 2);
        assert_eq!(queue.used_ring().next_used(), 2);
        assert_eq!(
            read_guest_bytes(&memory, second_status, 1),
            [VIRTIO_BLOCK_STATUS_OK]
        );
        let written_file = fs::read(file.as_path()).expect("file should read");
        assert_eq!(
            &written_file[VIRTIO_BLOCK_SECTOR_SIZE as usize..],
            second_payload.as_slice()
        );
    }

    #[test]
    fn block_queue_dispatch_parse_failure_publishes_zero_length_used_element() {
        let mut memory = request_memory();
        memory
            .write_slice(&[0xaa], STATUS_ADDR)
            .expect("status sentinel should write");
        let file = temp_file("queue-parse-failure.img", &sector_payload(0x10));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, 1, true)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect("parse failure should still publish used entry");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.read_count(), 0);
        assert_eq!(dispatch.write_count(), 0);
        assert_eq!(dispatch.flush_count(), 0);
        assert!(dispatch.read_latency_aggregate().is_none());
        assert!(dispatch.write_latency_aggregate().is_none());
        assert_eq!(dispatch.parse_failures(), 1);
        assert!(matches!(
            dispatch.first_parse_failure(),
            Some(VirtioBlockRequestError::InvalidDataLength {
                request_type: VirtioBlockRequestType::In,
                len: 1,
            })
        ));
        assert_eq!(dispatch.io_errors(), 0);
        let metrics = SharedBlockDeviceMetrics::default();
        metrics.record_queue_dispatch(&dispatch);
        assert_eq!(
            metrics.snapshot(),
            BlockDeviceMetrics::default().with_execute_fails(1)
        );
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_used_element(&memory, 0), (0, 0));
        assert_eq!(read_status(&memory), 0xaa);
    }

    #[test]
    fn block_queue_dispatch_execution_failure_still_publishes_completion() {
        let mut memory = request_memory();
        let payload = sector_payload(0x24);
        memory
            .write_slice(&payload, DATA_ADDR)
            .expect("guest data should write");
        let original = sector_payload(0x42);
        let file = temp_file("queue-execution-failure.img", &original);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_OUT,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, false)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect("execution failure should still publish completion");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.read_count(), 0);
        assert_eq!(dispatch.write_count(), 0);
        assert_eq!(dispatch.flush_count(), 0);
        assert_eq!(dispatch.io_errors(), 1);
        assert!(dispatch.read_latency_aggregate().is_none());
        let write_agg = dispatch
            .write_latency_aggregate()
            .expect("attempted write side effect should record latency");
        assert_eq!(dispatch.parse_failures(), 0);
        let metrics = SharedBlockDeviceMetrics::default();
        metrics.record_queue_dispatch(&dispatch);
        assert_eq!(
            metrics.snapshot(),
            BlockDeviceMetrics::default()
                .with_invalid_reqs_count(1)
                .with_write_agg(write_agg)
        );
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_IOERR);
        assert_eq!(
            fs::read(file.as_path()).expect("file should read"),
            original
        );
    }

    #[test]
    fn block_queue_dispatch_unsupported_request_updates_summary() {
        let mut memory = request_memory();
        memory
            .write_slice(b"sentinel", DATA_ADDR)
            .expect("guest data sentinel should write");
        let file = temp_file("queue-unsupported.img", &[]);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            99,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, 8, false)),
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect("unsupported request should publish completion");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.unsupported_requests(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.read_count(), 0);
        assert_eq!(dispatch.write_count(), 0);
        assert_eq!(dispatch.flush_count(), 0);
        assert_eq!(dispatch.io_errors(), 0);
        let metrics = SharedBlockDeviceMetrics::default();
        metrics.record_queue_dispatch(&dispatch);
        assert_eq!(
            metrics.snapshot(),
            BlockDeviceMetrics::default().with_invalid_reqs_count(1)
        );
        assert_eq!(read_guest_bytes(&memory, DATA_ADDR, 8), b"sentinel");
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_UNSUPPORTED);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
    }

    #[test]
    fn block_queue_dispatch_status_write_failure_updates_summary() {
        let mut memory = request_memory();
        let payload = sector_payload(0x31);
        let file = temp_file("queue-status-failure.img", &payload);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_IN,
            0,
            HEADER_ADDR,
            Some((DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, true)),
            GuestAddress::new(TEST_MEMORY_SIZE),
        );
        write_available_heads(&mut memory, &[0]);
        let mut queue = block_queue();

        let dispatch = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect("status write failure should publish zero-length completion");

        assert_eq!(dispatch.processed_requests(), 1);
        assert_eq!(dispatch.status_write_failures(), 1);
        assert_eq!(dispatch.successful_requests(), 0);
        assert_eq!(dispatch.read_count(), 0);
        assert_eq!(dispatch.write_count(), 0);
        assert_eq!(dispatch.flush_count(), 0);
        assert_eq!(dispatch.io_errors(), 0);
        let read_agg = dispatch
            .read_latency_aggregate()
            .expect("completed read side effect should record latency");
        assert!(dispatch.write_latency_aggregate().is_none());
        let metrics = SharedBlockDeviceMetrics::default();
        metrics.record_queue_dispatch(&dispatch);
        assert_eq!(
            metrics.snapshot(),
            BlockDeviceMetrics::default()
                .with_execute_fails(1)
                .with_read_agg(read_agg)
        );
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        assert_eq!(read_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn block_queue_dispatch_used_ring_failure_preserves_used_index() {
        let mut memory = request_memory();
        let file = temp_file("queue-used-failure.img", &sector_payload(0x55));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            0,
            HEADER_ADDR,
            None,
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0]);
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            TEST_AVAILABLE_RING,
            TEST_QUEUE_SIZE,
        )
        .expect("available ring should build");
        let unmapped_used_ring = GuestAddress::new(TEST_MEMORY_SIZE);
        let used = VirtqueueUsedRing::new(unmapped_used_ring, TEST_QUEUE_SIZE)
            .expect("used ring metadata should build");
        let mut queue = VirtioBlockQueue::new(available, used);

        let error = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect_err("unmapped used ring should fail dispatch");

        match error {
            VirtioBlockQueueDispatchError::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                ..
            } => {
                assert_eq!(descriptor_head, 0);
                assert_eq!(bytes_written_to_guest, VIRTIO_BLOCK_STATUS_SIZE);
            }
            other => panic!("expected used ring error, got {other:?}"),
        }
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_OK);
    }

    #[test]
    fn block_queue_dispatch_available_ring_failure_preserves_available_index() {
        let mut memory = request_memory();
        let file = temp_file("queue-available-failure.img", &sector_payload(0x66));
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        write_guest_u16(
            &mut memory,
            available_ring_idx_address(),
            TEST_QUEUE_SIZE + 1,
        );
        let mut queue = block_queue();

        let error = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect_err("invalid available length should fail dispatch");

        assert!(matches!(
            error,
            VirtioBlockQueueDispatchError::AvailableRing { .. }
        ));
        assert_eq!(queue.available_ring().next_avail(), 0);
        assert_eq!(queue.used_ring().next_used(), 0);
        assert_eq!(read_used_index(&memory), 0);
    }

    #[test]
    fn block_queue_dispatch_available_ring_failure_reports_completed_dispatch() {
        let mut memory = request_memory();
        let file = temp_file("queue-partial-available-failure.img", &sector_payload(0x77));
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        write_queued_request(
            &mut memory,
            0,
            VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
            0,
            HEADER_ADDR,
            None,
            STATUS_ADDR,
        );
        write_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        let mut queue = block_queue();

        let error = queue
            .dispatch(&mut memory, &backing, TEST_DEVICE_ID)
            .expect_err("invalid second available head should fail dispatch");

        let completed_dispatch = error.completed_dispatch();
        assert_eq!(completed_dispatch.processed_requests(), 1);
        assert_eq!(completed_dispatch.successful_requests(), 1);
        assert!(completed_dispatch.needs_queue_interrupt());
        assert!(matches!(
            error,
            VirtioBlockQueueDispatchError::AvailableRing { .. }
        ));
        assert_eq!(queue.available_ring().next_avail(), 1);
        assert_eq!(queue.used_ring().next_used(), 1);
        assert_eq!(read_used_index(&memory), 1);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
    }

    #[test]
    fn opens_read_write_backing_and_reports_size() {
        let file = temp_file("rw.img", b"abcdef");
        let backing = open_backing(file.as_path(), false).expect("backing should open");

        assert_eq!(backing.len(), 6);
        assert!(!backing.is_empty());
        assert!(!backing.is_read_only());
    }

    #[test]
    fn opens_read_only_backing() {
        let file = temp_file("ro.img", b"abcdef");
        let backing = open_backing(file.as_path(), true).expect("read-only backing should open");

        assert_eq!(backing.len(), 6);
        assert!(backing.is_read_only());
    }

    #[test]
    fn adopts_provided_backing_with_the_same_io_behavior() {
        let file = temp_file("provided-rw.img", b"abcdef");
        let provided_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(file.as_path())
            .expect("provided backing should open");
        let backing = BlockFileBacking::from_file(provided_file, false)
            .expect("provided backing should validate");
        let mut read_buffer = [0_u8; 3];

        assert_eq!(backing.len(), 6);
        assert!(!backing.is_read_only());
        backing
            .read_at(1, &mut read_buffer)
            .expect("provided backing read should succeed");
        backing
            .write_at(2, b"XY")
            .expect("provided backing write should succeed");
        backing
            .flush()
            .expect("provided backing flush should succeed");

        assert_eq!(&read_buffer, b"bcd");
        assert_eq!(
            fs::read(file.as_path()).expect("file should read"),
            b"abXYef"
        );
    }

    #[test]
    fn provided_backing_enforces_read_only_policy_and_regular_file_validation() {
        let file = temp_file("provided-ro.img", b"abcdef");
        let provided_file = OpenOptions::new()
            .read(true)
            .open(file.as_path())
            .expect("provided read-only backing should open");
        let backing = BlockFileBacking::from_file(provided_file, true)
            .expect("provided read-only backing should validate");

        assert!(matches!(
            backing.write_at(0, b"z"),
            Err(BlockFileBackingError::ReadOnlyWrite)
        ));

        let dir = temp_dir("provided-dir.img");
        let provided_dir = fs::File::open(dir.as_path()).expect("provided directory should open");
        let err = BlockFileBacking::from_file(provided_dir, true)
            .expect_err("provided directory should fail");

        assert!(matches!(err, BlockFileBackingError::NonRegularFile));
    }

    #[test]
    fn adopts_provided_backing_without_opening_configured_path() {
        let file = temp_file("provided-source.img", b"provided");
        let missing = missing_path("provided-missing.img");
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new("data", "data", &missing, false))
            .expect("provided drive config should validate");
        let provided_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(file.as_path())
            .expect("provided backing should open");
        let provided = BlockFileBacking::from_file(provided_file, false)
            .expect("provided backing should validate");
        let mut backings = BTreeMap::new();
        backings.insert("data".to_string(), provided);

        let prepared =
            PreparedBlockDevices::from_config_slice_with_backings(configs.as_slice(), backings)
                .expect("provided backing should prepare without configured path");
        let rendered = format!("{prepared:?}");

        let backing = prepared.as_slice()[0]
            .device()
            .backing()
            .expect("file device should retain backing");
        assert_eq!(backing.len(), 8);
        assert!(!backing.is_read_only());
        assert!(!missing.exists());
        assert!(rendered.contains("<owned>"));
        assert!(!rendered.contains(file.as_path().to_string_lossy().as_ref()));
    }

    #[test]
    fn rejects_provided_backing_with_mismatched_read_only_mode() {
        let file = temp_file("provided-mode-mismatch.img", b"provided");
        let missing = missing_path("provided-mode-mismatch-missing.img");
        let mut configs = DriveConfigs::new();
        configs
            .insert(DriveConfigInput::new("data", "data", &missing, false).with_is_read_only(true))
            .expect("read-only drive config should validate");
        let provided = BlockFileBacking::from_file(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(file.as_path())
                .expect("provided backing should open"),
            false,
        )
        .expect("provided backing should validate");
        let mut backings = BTreeMap::new();
        backings.insert("data".to_string(), provided);

        let err =
            PreparedBlockDevices::from_config_slice_with_backings(configs.as_slice(), backings)
                .expect_err("mismatched provided backing mode should fail");

        assert!(matches!(
            err,
            PreparedBlockDeviceError::BackingModeMismatch { ref drive_id }
                if drive_id == "data"
        ));
        assert!(
            !err.to_string()
                .contains(file.as_path().to_string_lossy().as_ref())
        );
        assert!(!err.to_string().contains(missing.to_string_lossy().as_ref()));
        assert!(!missing.exists());
    }

    #[test]
    fn rejects_provided_backing_without_matching_drive() {
        let file = temp_file("provided-unexpected.img", b"provided");
        let provided = BlockFileBacking::from_file(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(file.as_path())
                .expect("provided backing should open"),
            false,
        )
        .expect("provided backing should validate");
        let mut backings = BTreeMap::new();
        backings.insert("missing".to_string(), provided);

        let err = PreparedBlockDevices::from_config_slice_with_backings(&[], backings)
            .expect_err("unexpected backing should fail");

        assert!(matches!(err, PreparedBlockDeviceError::UnexpectedBacking));
        assert_eq!(
            err.to_string(),
            "provided block backing does not match a configured drive"
        );
    }

    #[test]
    fn drive_debug_redacts_configured_paths_and_grant_references() {
        let reference = "bangbang-grant:secret-drive-grant";
        let input = DriveConfigInput::new("data", "data", reference, false);
        let socket_input = DriveConfigInput::new("socket", "socket", reference, false)
            .with_socket("/secret/socket");
        let update_input = DriveUpdateInput::new(
            "data",
            "data",
            Some(PathBuf::from("bangbang-grant:secret-drive-update")),
        );
        let config = input.clone().validate().expect("drive should validate");
        let update = update_input
            .clone()
            .validate()
            .expect("update should validate");

        for rendered in [
            format!("{input:?}"),
            format!("{socket_input:?}"),
            format!("{update_input:?}"),
            format!("{config:?}"),
            format!("{update:?}"),
            format!("{:?}", crate::VmmAction::PutDrive(input)),
        ] {
            assert!(rendered.contains("<redacted>"));
            assert!(!rendered.contains("secret-drive"));
            assert!(!rendered.contains("/secret/socket"));
        }
    }

    #[test]
    fn accepts_zero_length_regular_backing() {
        let file = temp_file("empty.img", b"");
        let backing = open_backing(file.as_path(), false).expect("empty backing should open");
        let mut empty = [];

        assert_eq!(backing.len(), 0);
        assert!(backing.is_empty());
        backing
            .read_at(0, &mut empty)
            .expect("zero-length read at EOF should succeed");
        backing
            .write_at(0, &empty)
            .expect("zero-length write at EOF should succeed");
    }

    #[test]
    fn rejects_missing_backing_without_echoing_path() {
        let path = missing_path("secret-missing.img");
        let err = open_backing(&path, true).expect_err("missing backing should fail");

        assert!(matches!(err, BlockFileBackingError::OpenFile { .. }));
        assert_eq!(
            err.source()
                .and_then(|source| source.downcast_ref::<io::Error>())
                .map(io::Error::kind),
            Some(io::ErrorKind::NotFound)
        );
        assert!(!err.to_string().contains("secret-missing"));
    }

    #[test]
    fn rejects_directory_backing_as_non_regular() {
        let dir = temp_dir("dir.img");
        let err = open_backing(dir.as_path(), true).expect_err("directory backing should fail");

        assert!(matches!(err, BlockFileBackingError::NonRegularFile));
        assert_eq!(
            err.to_string(),
            "block backing path does not reference a regular file"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn rejects_fifo_backing_without_blocking() {
        let fifo = temp_fifo("fifo.img");
        let err = open_backing(fifo.as_path(), true).expect_err("FIFO backing should fail");

        assert!(matches!(err, BlockFileBackingError::NonRegularFile));
    }

    #[test]
    fn rejects_socket_backing_without_blocking() {
        let (socket, listener) = temp_socket("socket.img");
        let err = open_backing(socket.as_path(), true).expect_err("socket backing should fail");
        drop(listener);

        assert!(matches!(
            err,
            BlockFileBackingError::OpenFile { .. } | BlockFileBackingError::NonRegularFile
        ));
        assert!(!err.to_string().contains("socket.img"));
    }

    #[test]
    fn reads_at_offsets_and_boundaries() {
        let file = temp_file("read.img", b"abcdef");
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut middle = [0_u8; 3];
        let mut last = [0_u8; 1];
        let mut empty = [];

        backing
            .read_at(2, &mut middle)
            .expect("middle read should succeed");
        backing
            .read_at(5, &mut last)
            .expect("last-byte read should succeed");
        backing
            .read_at(6, &mut empty)
            .expect("zero-length read at EOF should succeed");

        assert_eq!(&middle, b"cde");
        assert_eq!(&last, b"f");
    }

    #[test]
    fn writes_at_offsets_and_boundaries() {
        let file = temp_file("write.img", b"abcdef");
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let empty = [];

        backing
            .write_at(2, b"XY")
            .expect("middle write should succeed");
        backing
            .write_at(5, b"Z")
            .expect("last-byte write should succeed");
        backing
            .write_at(6, &empty)
            .expect("zero-length write at EOF should succeed");

        assert_eq!(
            fs::read(file.as_path()).expect("file should read"),
            b"abXYeZ"
        );
    }

    #[test]
    fn rejects_out_of_bounds_accesses_without_mutating_buffers_or_files() {
        let file = temp_file("bounds.img", b"abc");
        let backing = open_backing(file.as_path(), false).expect("backing should open");
        let mut read_buffer = *b"zz";

        let read_err = backing
            .read_at(2, &mut read_buffer)
            .expect_err("read extending past EOF should fail");
        assert!(matches!(
            read_err,
            BlockFileBackingError::AccessOutOfBounds {
                offset: 2,
                len: 2,
                file_len: 3,
            }
        ));
        assert_eq!(&read_buffer, b"zz");

        let write_err = backing
            .write_at(3, b"x")
            .expect_err("write extending past EOF should fail");
        assert!(matches!(
            write_err,
            BlockFileBackingError::AccessOutOfBounds {
                offset: 3,
                len: 1,
                file_len: 3,
            }
        ));
        assert_eq!(fs::read(file.as_path()).expect("file should read"), b"abc");
    }

    #[test]
    fn rejects_offset_length_overflow() {
        let file = temp_file("overflow.img", b"abc");
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let mut buffer = [0_u8; 1];

        let err = backing
            .read_at(u64::MAX, &mut buffer)
            .expect_err("overflowing access should fail");

        assert!(matches!(
            err,
            BlockFileBackingError::AccessOverflow {
                offset: u64::MAX,
                len: 1,
            }
        ));
        assert_eq!(
            err.to_string(),
            format!(
                "block backing access at offset {} with length 1 overflows",
                u64::MAX
            )
        );
    }

    #[test]
    fn rejects_read_only_writes_without_mutating_file() {
        let file = temp_file("readonly-write.img", b"abc");
        let backing = open_backing(file.as_path(), true).expect("backing should open");

        let err = backing
            .write_at(0, b"z")
            .expect_err("read-only write should fail");

        assert!(matches!(err, BlockFileBackingError::ReadOnlyWrite));
        assert_eq!(err.to_string(), "block backing file is read-only");
        assert!(err.source().is_none());
        assert_eq!(fs::read(file.as_path()).expect("file should read"), b"abc");
    }

    #[test]
    fn backing_errors_display_and_preserve_sources() {
        let read_only = BlockFileBackingError::ReadOnlyWrite;
        assert_eq!(read_only.to_string(), "block backing file is read-only");
        assert!(read_only.source().is_none());

        let open = BlockFileBackingError::OpenFile {
            source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
        };
        assert_eq!(
            open.to_string(),
            "failed to open block backing file: denied"
        );
        assert!(open.source().is_some());
    }

    #[test]
    fn snapshot_backing_open_is_nofollow_and_identity_is_descriptor_based() {
        let file = temp_file("snapshot-backing.img", &[0x5a; 512]);
        let directory = temp_dir("snapshot-backing-dir");
        let fifo = temp_fifo("snapshot-backing-fifo");
        let (socket, _listener) = temp_socket("snapshot-backing-socket");
        let link = TempPath {
            path: temp_path("snapshot-backing-link.img"),
        };
        std::os::unix::fs::symlink(file.as_path(), link.as_path())
            .expect("test symlink should be created");

        let (backing, identity) = BlockFileBacking::open_snapshot_read_only(file.as_path())
            .expect("regular snapshot backing should open");
        assert!(backing.is_read_only());
        assert!(!backing.uses_supplied_file());
        assert_eq!(backing.len(), 512);
        assert_eq!(
            backing
                .snapshot_identity()
                .expect("opened descriptor identity should read"),
            identity
        );
        let supplied_file = File::open(file.as_path()).expect("supplied backing should open");
        let moved = file.as_path().with_extension("opened");
        fs::rename(file.as_path(), &moved).expect("opened backing should move");
        fs::write(file.as_path(), [0xa5; 512]).expect("replacement backing should create");
        let (supplied, supplied_identity) =
            BlockFileBacking::from_snapshot_read_only_file(supplied_file)
                .expect("supplied exact backing should adopt");
        assert!(supplied.uses_supplied_file());
        let current_identity = supplied
            .snapshot_identity()
            .expect("supplied descriptor identity should read");
        assert_eq!(supplied_identity, current_identity);
        assert_eq!(supplied_identity.device(), identity.device());
        assert_eq!(supplied_identity.inode(), identity.inode());
        assert_eq!(supplied_identity.len(), identity.len());
        fs::remove_file(moved).expect("opened backing fixture should clean up");
        assert_eq!(
            BlockFileBacking::open_snapshot_read_only(link.as_path())
                .expect_err("snapshot backing symlink should reject"),
            SnapshotBlockFileBackingError::Open
        );
        for unsupported in [directory.as_path(), fifo.as_path(), socket.as_path()] {
            let error = BlockFileBacking::open_snapshot_read_only(unsupported)
                .expect_err("non-regular snapshot backing should reject");
            assert!(matches!(
                error,
                SnapshotBlockFileBackingError::Open | SnapshotBlockFileBackingError::NonRegularFile
            ));
            assert!(
                !error
                    .to_string()
                    .contains(unsupported.to_string_lossy().as_ref())
            );
        }
        assert!(!format!("{identity:?}").contains(file.as_path().to_string_lossy().as_ref()));
    }

    #[test]
    fn block_queue_snapshot_validation_checks_cursors_retry_and_overlap() {
        let mut memory = request_memory();
        let queue = block_queue();
        queue
            .validate_snapshot_state(&memory, false)
            .expect("empty queue snapshot should validate");
        assert_eq!(
            queue
                .validate_snapshot_state(&memory, true)
                .expect_err("retry without a pending descriptor should reject"),
            VirtioBlockQueueSnapshotError::RetryWithoutPendingDescriptor
        );

        write_available_heads(&mut memory, &[0]);
        queue
            .validate_snapshot_state(&memory, true)
            .expect("retry with a pending descriptor should validate");

        let overlapping = VirtioBlockQueue::new(
            VirtqueueAvailableRing::new(
                TEST_DESCRIPTOR_TABLE,
                TEST_DESCRIPTOR_TABLE,
                TEST_QUEUE_SIZE,
            )
            .expect("overlapping ring addresses remain structurally aligned"),
            VirtqueueUsedRing::new(TEST_USED_RING, TEST_QUEUE_SIZE)
                .expect("used ring should build"),
        );
        assert_eq!(
            overlapping
                .validate_snapshot_state(&memory, false)
                .expect_err("overlapping queue ranges should reject"),
            VirtioBlockQueueSnapshotError::QueueRangesOverlap
        );
    }

    #[test]
    fn block_rate_limiter_persisted_state_round_trips_at_injected_time() {
        let origin = Instant::now();
        let config =
            DriveRateLimiterConfig::new(None, Some(DriveTokenBucketConfig::new(4, Some(1), 100)));
        let mut limiter = VirtioBlockRateLimiter::new_at(config, origin)
            .expect("configured limiter should build");
        let ops = limiter.ops.as_mut().expect("ops bucket should exist");
        assert!(ops.reduce_at(1, origin));
        assert!(ops.reduce_at(2, origin));
        let capture_now = origin + Duration::from_millis(25);

        let state =
            VirtioBlockRateLimiter::persisted_state_at(Some(config), Some(&limiter), capture_now)
                .expect("limiter should capture");
        let restored = VirtioBlockRateLimiter::from_persisted_state_at(
            Some(config),
            state,
            origin + Duration::from_secs(5),
        )
        .expect("limiter should restore")
        .expect("restored limiter should remain enabled");
        let restored_state = VirtioBlockRateLimiter::persisted_state_at(
            Some(config),
            Some(&restored),
            origin + Duration::from_secs(5),
        )
        .expect("restored limiter should recapture");

        assert_eq!(
            restored_state
                .ops()
                .expect("restored ops state should exist")
                .budget(),
            2
        );
        assert_eq!(
            restored_state
                .ops()
                .expect("restored ops state should exist")
                .age_nanos(),
            25_000_000
        );
    }
}
