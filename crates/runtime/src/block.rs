//! Backend-neutral block-device configuration model.

use std::collections::TryReserveError;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::interrupt::DeviceInterruptKind;
use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryRange,
};
use crate::mmio::{
    MmioAccessBytes, MmioAccessBytesError, MmioBusError, MmioDispatchError, MmioDispatcher,
    MmioHandlerError, MmioRegion, MmioRegionId,
};
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceActivation, VirtioMmioDeviceActivationError,
    VirtioMmioDeviceActivationHandler, VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError,
    VirtioMmioDeviceConfigHandler, VirtioMmioQueueRegisterError, VirtioMmioQueueState,
    VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
};
use crate::virtio_queue::{
    VirtqueueAvailableRing, VirtqueueAvailableRingError, VirtqueueDescriptor,
    VirtqueueDescriptorChain, VirtqueueDescriptorChainOptions, VirtqueueNotificationSuppression,
    VirtqueueUsedRing, VirtqueueUsedRingError, VirtqueueUsedRingPublication,
};

pub const VIRTIO_BLOCK_DEVICE_ID: u32 = 2;
pub const VIRTIO_BLOCK_QUEUE_COUNT: usize = 1;
pub const VIRTIO_BLOCK_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_BLOCK_QUEUE_SIZES: [u16; VIRTIO_BLOCK_QUEUE_COUNT] = [VIRTIO_BLOCK_QUEUE_SIZE];
pub const VIRTIO_BLOCK_SECTOR_SHIFT: u32 = 9;
pub const VIRTIO_BLOCK_SECTOR_SIZE: u64 = 1 << VIRTIO_BLOCK_SECTOR_SHIFT;
pub const VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE: usize = 8;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveConfigInput {
    path_drive_id: String,
    body_drive_id: String,
    path_on_host: PathBuf,
    is_root_device: bool,
    is_read_only: Option<bool>,
    partuuid: Option<String>,
    cache_type: Option<DriveCacheType>,
    io_engine: Option<DriveIoEngine>,
    rate_limiter_configured: bool,
    socket: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveUpdateInput {
    path_drive_id: String,
    body_drive_id: String,
    path_on_host: Option<PathBuf>,
    rate_limiter_configured: bool,
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
            rate_limiter_configured: false,
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

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
    }

    pub const fn with_rate_limiter_configured(mut self) -> Self {
        self.rate_limiter_configured = true;
        self
    }

    pub fn validate(self) -> Result<DriveUpdate, DriveUpdateError> {
        DriveUpdate::try_from(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveUpdate {
    drive_id: String,
    path_on_host: Option<PathBuf>,
}

impl DriveUpdate {
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    pub fn path_on_host(&self) -> Option<&Path> {
        self.path_on_host.as_deref()
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
            path_on_host: path_on_host.into(),
            is_root_device,
            is_read_only: None,
            partuuid: None,
            cache_type: None,
            io_engine: None,
            rate_limiter_configured: false,
            socket: None,
        }
    }

    pub fn path_drive_id(&self) -> &str {
        &self.path_drive_id
    }

    pub fn body_drive_id(&self) -> &str {
        &self.body_drive_id
    }

    pub fn path_on_host(&self) -> &Path {
        &self.path_on_host
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

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
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

    pub const fn with_rate_limiter_configured(mut self) -> Self {
        self.rate_limiter_configured = true;
        self
    }

    pub fn with_socket(mut self, socket: impl Into<PathBuf>) -> Self {
        self.socket = Some(socket.into());
        self
    }

    pub fn validate(self) -> Result<DriveConfig, DriveConfigError> {
        DriveConfig::try_from(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveConfig {
    drive_id: String,
    path_on_host: PathBuf,
    is_root_device: bool,
    is_read_only: bool,
    partuuid: Option<String>,
    cache_type: DriveCacheType,
    io_engine: DriveIoEngine,
}

impl DriveConfig {
    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    pub fn path_on_host(&self) -> &Path {
        &self.path_on_host
    }

    pub const fn is_root_device(&self) -> bool {
        self.is_root_device
    }

    pub const fn is_read_only(&self) -> bool {
        self.is_read_only
    }

    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    pub const fn cache_type(&self) -> DriveCacheType {
        self.cache_type
    }

    pub const fn io_engine(&self) -> DriveIoEngine {
        self.io_engine
    }

    fn updated(&self, update: &DriveUpdate) -> Result<Self, DriveUpdateError> {
        if self.drive_id() != update.drive_id() {
            return Err(DriveUpdateError::UnknownDrive {
                drive_id: update.drive_id().to_string(),
            });
        }

        Ok(Self {
            drive_id: self.drive_id.clone(),
            path_on_host: update
                .path_on_host()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.path_on_host.clone()),
            is_root_device: self.is_root_device,
            is_read_only: self.is_read_only,
            partuuid: self.partuuid.clone(),
            cache_type: self.cache_type,
            io_engine: self.io_engine,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DriveConfigs {
    configs: Vec<DriveConfig>,
}

impl DriveConfigs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_slice(&self) -> &[DriveConfig] {
        &self.configs
    }

    pub fn insert(&mut self, input: DriveConfigInput) -> Result<(), DriveConfigError> {
        let config = input.validate()?;
        if config.is_root_device()
            && self.configs.iter().any(|existing| {
                existing.is_root_device() && existing.drive_id() != config.drive_id()
            })
        {
            return Err(DriveConfigError::RootDeviceAlreadyConfigured);
        }

        if let Some(index) = self
            .configs
            .iter()
            .position(|existing| existing.drive_id() == config.drive_id())
        {
            self.configs.remove(index);
        }

        if config.is_root_device() {
            self.configs.insert(0, config);
        } else {
            self.configs.push(config);
        }

        Ok(())
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

        if input.path_on_host.as_os_str().is_empty() {
            return Err(DriveConfigError::EmptyPathOnHost);
        }

        let cache_type = input.cache_type.unwrap_or_default();
        let io_engine = input.io_engine.unwrap_or_default();
        if io_engine != DriveIoEngine::Sync {
            return Err(DriveConfigError::UnsupportedIoEngine { io_engine });
        }

        if input.rate_limiter_configured {
            return Err(DriveConfigError::UnsupportedRateLimiter);
        }

        if input.socket.is_some() {
            return Err(DriveConfigError::UnsupportedSocket);
        }

        Ok(Self {
            drive_id: input.path_drive_id,
            path_on_host: input.path_on_host,
            is_root_device: input.is_root_device,
            is_read_only: input.is_read_only.unwrap_or(false),
            partuuid: input.partuuid,
            cache_type,
            io_engine,
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

        if input.rate_limiter_configured {
            return Err(DriveUpdateError::UnsupportedRateLimiter);
        }

        Ok(Self {
            drive_id: input.path_drive_id,
            path_on_host: input.path_on_host,
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
    UnsupportedIoEngine {
        io_engine: DriveIoEngine,
    },
    UnsupportedRateLimiter,
    UnsupportedSocket,
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
    UnsupportedRateLimiter,
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
    ActiveSessionUnavailable,
    MmioDispatcherUnavailable,
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
            Self::UnsupportedIoEngine { io_engine } => {
                write!(f, "drive io_engine {io_engine} is not supported")
            }
            Self::UnsupportedRateLimiter => f.write_str("drive rate_limiter is not supported"),
            Self::UnsupportedSocket => f.write_str("drive socket is not supported"),
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
            Self::UnsupportedRateLimiter => f.write_str("drive rate_limiter is not supported"),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockConfigSpace {
    capacity_sectors: u64,
    is_read_only: bool,
    cache_type: DriveCacheType,
}

impl VirtioBlockConfigSpace {
    pub const fn new(backing_len: u64, is_read_only: bool, cache_type: DriveCacheType) -> Self {
        Self {
            capacity_sectors: backing_len >> VIRTIO_BLOCK_SECTOR_SHIFT,
            is_read_only,
            cache_type,
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
        let mut features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_INDIRECT_DESC)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX);
        if matches!(self.cache_type, DriveCacheType::Writeback) {
            features |= virtio_feature_bit(VIRTIO_BLOCK_FEATURE_FLUSH);
        }
        if self.is_read_only {
            features |= virtio_feature_bit(VIRTIO_BLOCK_FEATURE_READ_ONLY);
        }
        features
    }

    const fn capacity_bytes(self) -> [u8; VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE] {
        self.capacity_sectors.to_le_bytes()
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioBlockConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let capacity = self.capacity_bytes();
        let bytes = read_virtio_block_capacity_bytes(&capacity, access)?;
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

fn read_virtio_block_capacity_bytes(
    capacity: &[u8; VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE],
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

    capacity
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
    pub const fn new(min_us: u64, max_us: u64, sum_us: u64, sample_count: u64) -> Self {
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

    pub const fn sample_count(self) -> u64 {
        self.sample_count
    }

    pub const fn is_empty(self) -> bool {
        self.sample_count == 0
    }

    pub const fn from_sample(latency_us: u64) -> Self {
        Self {
            min_us: latency_us,
            max_us: latency_us,
            sum_us: latency_us,
            sample_count: 1,
        }
    }

    pub const fn merged_with(mut self, other: Self) -> Self {
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
        let mut dispatch = VirtioBlockQueueDispatch::default();
        let capacity_sectors = backing.len() >> VIRTIO_BLOCK_SECTOR_SHIFT;
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
        }
    }
}

impl std::error::Error for VirtioBlockQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::EmptyDescriptorChain { .. } => None,
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

#[derive(Debug)]
pub struct BlockFileBacking {
    file: File,
    len: u64,
    is_read_only: bool,
}

impl BlockFileBacking {
    pub fn open(config: &DriveConfig) -> Result<Self, BlockFileBackingError> {
        let file = open_block_file(config.path_on_host(), config.is_read_only())?;
        let metadata = file
            .metadata()
            .map_err(|source| BlockFileBackingError::ReadMetadata { source })?;

        if !metadata.file_type().is_file() {
            return Err(BlockFileBackingError::NonRegularFile);
        }

        Ok(Self {
            file,
            len: metadata.len(),
            is_read_only: config.is_read_only(),
        })
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
            Self::NonRegularFile
            | Self::AccessLengthTooLarge { .. }
            | Self::AccessOverflow { .. }
            | Self::AccessOutOfBounds { .. }
            | Self::ReadOnlyWrite => None,
        }
    }
}

#[derive(Debug)]
pub struct VirtioBlockDevice {
    backing: BlockFileBacking,
    device_id: VirtioBlockDeviceId,
    active_queue: Option<VirtioBlockQueue>,
}

impl VirtioBlockDevice {
    pub fn new(backing: BlockFileBacking, device_id: VirtioBlockDeviceId) -> Self {
        Self {
            backing,
            device_id,
            active_queue: None,
        }
    }

    pub fn backing(&self) -> &BlockFileBacking {
        &self.backing
    }

    pub fn refresh_backing(&mut self, backing: BlockFileBacking) {
        self.backing = backing;
    }

    pub fn device_id(&self) -> VirtioBlockDeviceId {
        self.device_id
    }

    pub fn is_activated(&self) -> bool {
        self.active_queue.is_some()
    }

    pub fn active_queue(&self) -> Option<&VirtioBlockQueue> {
        self.active_queue.as_ref()
    }

    pub fn active_queue_mut(&mut self) -> Option<&mut VirtioBlockQueue> {
        self.active_queue.as_mut()
    }

    fn dispatch_drained_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
    ) -> Result<VirtioBlockDeviceNotificationDispatch, VirtioBlockDeviceNotificationError> {
        if drained_notifications.is_empty() {
            return Ok(VirtioBlockDeviceNotificationDispatch::new(
                drained_notifications,
                None,
            ));
        }

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

        let Some(queue) = self.active_queue.as_mut() else {
            return Err(VirtioBlockDeviceNotificationError::Inactive {
                drained_notifications,
            });
        };

        let backing = &self.backing;
        let device_id = self.device_id;
        match queue.dispatch(memory, backing, device_id) {
            Ok(dispatch) => Ok(VirtioBlockDeviceNotificationDispatch::new(
                drained_notifications,
                Some(dispatch),
            )),
            Err(source) => Err(VirtioBlockDeviceNotificationError::QueueDispatch {
                drained_notifications,
                source,
            }),
        }
    }

    pub fn activate_block(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioBlockDeviceActivationError> {
        if self.active_queue.is_some() {
            return Err(VirtioBlockDeviceActivationError::AlreadyActive);
        }

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
        self.active_queue = Some(queue);

        Ok(())
    }

    pub fn reset(&mut self) {
        self.active_queue = None;
    }
}

#[derive(Debug)]
pub struct PreparedBlockDevice {
    drive_id: String,
    config_space: VirtioBlockConfigSpace,
    device: VirtioBlockDevice,
}

impl PreparedBlockDevice {
    fn from_config(config: &DriveConfig) -> Result<Self, PreparedBlockDeviceError> {
        let backing = BlockFileBacking::open(config).map_err(|source| {
            PreparedBlockDeviceError::OpenBacking {
                drive_id: config.drive_id().to_string(),
                source,
            }
        })?;
        let config_space = VirtioBlockConfigSpace::from_backing(&backing, config.cache_type());
        let device_id = VirtioBlockDeviceId::from_bytes(config.drive_id().as_bytes());
        let device = VirtioBlockDevice::new(backing, device_id);

        Ok(Self {
            drive_id: config.drive_id().to_string(),
            config_space,
            device,
        })
    }

    pub fn drive_id(&self) -> &str {
        &self.drive_id
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

    pub fn into_parts(self) -> (String, VirtioBlockConfigSpace, VirtioBlockDevice) {
        (self.drive_id, self.config_space, self.device)
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
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(configs.len())
            .map_err(|source| PreparedBlockDeviceError::AllocateDevices { source })?;

        for config in configs {
            devices.push(PreparedBlockDevice::from_config(config)?);
        }

        Ok(Self { devices })
    }

    pub fn as_slice(&self) -> &[PreparedBlockDevice] {
        &self.devices
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
}

impl fmt::Display for PreparedBlockDeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocateDevices { source } => {
                write!(f, "failed to allocate prepared block devices: {source}")
            }
            Self::OpenBacking { drive_id, source } => {
                write!(f, "failed to prepare block device {drive_id}: {source}")
            }
        }
    }
}

impl std::error::Error for PreparedBlockDeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateDevices { source } => Some(source),
            Self::OpenBacking { source, .. } => Some(source),
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
            let (drive_id, config_space, device) = prepared_device.into_parts();
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
            Err(error) => error
                .completed_dispatch()
                .is_some_and(VirtioBlockQueueDispatch::needs_queue_interrupt),
        };
        if needs_queue_interrupt {
            self.mark_interrupt_pending(DeviceInterruptKind::Queue);
        }

        dispatch
    }
}

impl VirtioMmioRegisterHandler<VirtioBlockConfigSpace, VirtioBlockDevice> {
    pub fn refresh_block_backing(&mut self, config: &DriveConfig) -> Result<(), DriveUpdateError> {
        let backing =
            BlockFileBacking::open(config).map_err(|source| DriveUpdateError::OpenBacking {
                drive_id: config.drive_id().to_string(),
                message: source.to_string(),
            })?;

        self.refresh_block_backing_with_opened(config, backing);

        Ok(())
    }

    pub fn refresh_block_backing_with_opened(
        &mut self,
        config: &DriveConfig,
        backing: BlockFileBacking,
    ) {
        let config_space = VirtioBlockConfigSpace::from_backing(&backing, config.cache_type());

        self.activation_handler_mut().refresh_backing(backing);
        *self.device_config_handler_mut() = config_space;
        self.increment_config_generation();
        self.mark_interrupt_pending(DeviceInterruptKind::Config);
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
}

impl VirtioBlockDeviceNotificationDispatch {
    const fn new(
        drained_notifications: Vec<usize>,
        queue_dispatch: Option<VirtioBlockQueueDispatch>,
    ) -> Self {
        Self {
            drained_notifications,
            queue_dispatch,
        }
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub fn queue_dispatch(&self) -> Option<&VirtioBlockQueueDispatch> {
        self.queue_dispatch.as_ref()
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.queue_dispatch
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
            } => drained_notifications,
        }
    }

    pub const fn completed_dispatch(&self) -> Option<&VirtioBlockQueueDispatch> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source.completed_dispatch()),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
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
        }
    }
}

impl std::error::Error for VirtioBlockDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum VirtioBlockDeviceActivationError {
    AlreadyActive,
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
            Self::AlreadyActive => None,
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
    use std::error::Error as _;
    use std::ffi::CString;
    use std::fs::{self, OpenOptions};
    use std::io::{self, Write};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::interrupt::DeviceInterruptKind;
    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};
    use crate::metrics::{BlockDeviceMetrics, SharedBlockDeviceMetrics};
    use crate::mmio::{
        MmioAccess, MmioAccessBytes, MmioBus, MmioDispatchOutcome, MmioOperation, MmioRegionId,
    };
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_DEVICE_STATUS_INIT, VIRTIO_MMIO_DEVICE_CONFIG_OFFSET,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_MAGIC_VALUE, VirtioMmioDeviceActivation,
        VirtioMmioDeviceActivationError, VirtioMmioDeviceActivationHandler,
        VirtioMmioDeviceRegisters, VirtioMmioQueueRegisters, VirtioMmioRegister,
        VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_INDIRECT, VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE,
        VIRTQUEUE_DESCRIPTOR_SIZE, VirtqueueAvailableRing, VirtqueueAvailableRingError,
        VirtqueueDescriptorChain, VirtqueueDescriptorChainOptions, VirtqueueUsedRing,
        VirtqueueUsedRingError, read_descriptor_chain,
    };

    use super::{
        BlockFileBacking, BlockFileBackingError, BlockMmioDevices, BlockMmioLayout,
        BlockMmioRegistrationError, DriveCacheType, DriveConfig, DriveConfigError,
        DriveConfigInput, DriveConfigs, DriveIdSource, DriveIoEngine, DriveUpdateError,
        DriveUpdateInput, PreparedBlockDeviceError, PreparedBlockDevices,
        VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE, VIRTIO_BLOCK_DEVICE_ID, VIRTIO_BLOCK_FEATURE_FLUSH,
        VIRTIO_BLOCK_FEATURE_READ_ONLY, VIRTIO_BLOCK_ID_BYTES, VIRTIO_BLOCK_QUEUE_COUNT,
        VIRTIO_BLOCK_QUEUE_SIZE, VIRTIO_BLOCK_QUEUE_SIZES, VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
        VIRTIO_BLOCK_REQUEST_TYPE_FLUSH, VIRTIO_BLOCK_REQUEST_TYPE_GET_ID,
        VIRTIO_BLOCK_REQUEST_TYPE_IN, VIRTIO_BLOCK_REQUEST_TYPE_OUT, VIRTIO_BLOCK_SECTOR_SHIFT,
        VIRTIO_BLOCK_SECTOR_SIZE, VIRTIO_BLOCK_STATUS_IOERR, VIRTIO_BLOCK_STATUS_OK,
        VIRTIO_BLOCK_STATUS_SIZE, VIRTIO_BLOCK_STATUS_UNSUPPORTED, VIRTIO_FEATURE_VERSION_1,
        VIRTIO_RING_FEATURE_EVENT_IDX, VIRTIO_RING_FEATURE_INDIRECT_DESC, VirtioBlockConfigSpace,
        VirtioBlockDevice, VirtioBlockDeviceActivationError, VirtioBlockDeviceId,
        VirtioBlockDeviceNotificationError, VirtioBlockQueue, VirtioBlockQueueBuildError,
        VirtioBlockQueueDispatchError, VirtioBlockRequest, VirtioBlockRequestCompletion,
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
        assert_eq!(config.path_on_host(), PathBuf::from("/tmp/rootfs.ext4"));
        assert!(!config.is_root_device());
        assert!(!config.is_read_only());
        assert_eq!(config.partuuid(), None);
        assert_eq!(config.cache_type(), DriveCacheType::Unsafe);
        assert_eq!(config.io_engine(), DriveIoEngine::Sync);
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
        assert!(config.is_read_only());
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
    fn accepts_writeback_cache_type() {
        let config = validate(input().with_cache_type(DriveCacheType::Writeback))
            .expect("Writeback cache should be supported");

        assert_eq!(config.cache_type(), DriveCacheType::Writeback);
    }

    #[test]
    fn rejects_unsupported_io_engine() {
        let err = validate(input().with_io_engine(DriveIoEngine::Async))
            .expect_err("Async I/O engine should be unsupported");

        assert_eq!(
            err,
            DriveConfigError::UnsupportedIoEngine {
                io_engine: DriveIoEngine::Async,
            }
        );
        assert_eq!(err.to_string(), "drive io_engine Async is not supported");
    }

    #[test]
    fn rejects_rate_limiter_and_socket_fields() {
        assert_eq!(
            validate(input().with_rate_limiter_configured()),
            Err(DriveConfigError::UnsupportedRateLimiter)
        );
        assert_eq!(
            validate(input().with_socket("/tmp/vhost-user-block.sock")),
            Err(DriveConfigError::UnsupportedSocket)
        );
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
        assert_eq!(input.path_on_host(), PathBuf::from("/tmp/rootfs.ext4"));
        assert!(!input.is_root_device());
        assert_eq!(input.is_read_only(), Some(false));
        assert_eq!(input.partuuid(), Some("part"));
        assert_eq!(input.cache_type(), Some(DriveCacheType::Unsafe));
        assert_eq!(input.io_engine(), Some(DriveIoEngine::Sync));
        assert!(!input.rate_limiter_configured());
        assert_eq!(input.socket(), None);
    }

    #[test]
    fn drive_config_errors_display_and_preserve_sources() {
        let err = DriveConfigError::UnsupportedRateLimiter;

        assert_eq!(err.to_string(), "drive rate_limiter is not supported");
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
        assert_eq!(config.path_on_host(), PathBuf::from("/tmp/rootfs.ext4"));
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
        assert_eq!(config.path_on_host(), PathBuf::from("/tmp/replaced.ext4"));
        assert!(config.is_root_device());
        assert!(config.is_read_only());
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

        let rate_limited_update =
            DriveUpdateInput::new("rootfs", "rootfs", None).with_rate_limiter_configured();
        assert!(rate_limited_update.rate_limiter_configured());
        assert_eq!(
            rate_limited_update.validate(),
            Err(DriveUpdateError::UnsupportedRateLimiter)
        );
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
        assert_eq!(updated.path_on_host(), Path::new("/tmp/data-updated.ext4"));
        assert!(!updated.is_root_device());
        configs
            .commit_update(updated)
            .expect("runtime drive update should commit");
        assert_eq!(configs.as_slice()[0].drive_id(), "rootfs");
        assert_eq!(
            configs.as_slice()[0].path_on_host(),
            Path::new("/tmp/rootfs.ext4")
        );
        assert_eq!(configs.as_slice()[1].drive_id(), "data");
        assert_eq!(
            configs.as_slice()[1].path_on_host(),
            Path::new("/tmp/data-updated.ext4")
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

        assert_eq!(updated.path_on_host(), Path::new("/tmp/rootfs.ext4"));
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
            Path::new("/tmp/rootfs.ext4")
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
        assert_eq!(device.device().backing().len(), 1024);
        assert!(device.device().backing().is_read_only());
        assert!(matches!(
            device.device().backing().write_at(0, b"x"),
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
        assert!(!device.device().backing().is_read_only());
        device
            .device()
            .backing()
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
        assert_eq!(handler.activation_handler().backing().len(), 512);
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );

        handler
            .refresh_block_backing(&replacement_config)
            .expect("replacement backing should refresh");

        assert_eq!(handler.device_config_handler().capacity_sectors(), 2);
        assert_eq!(handler.activation_handler().backing().len(), 1024);
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
        assert_eq!(handler.activation_handler().backing().len(), 512);
        assert_eq!(
            handler.read_register(VirtioMmioRegister::ConfigGeneration),
            Ok(0)
        );
        assert_eq!(read_interrupt_status(&handler), 0);
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
        assert_eq!(device.backing().len(), VIRTIO_BLOCK_SECTOR_SIZE);
        assert!(!device.backing().is_read_only());
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
}
