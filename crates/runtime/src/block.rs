//! Backend-neutral block-device configuration model.

use std::collections::TryReserveError;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::memory::{GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryRange};
use crate::mmio::{MmioAccessBytes, MmioAccessBytesError};
use crate::virtio_mmio::{
    VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError, VirtioMmioDeviceConfigHandler,
};
use crate::virtio_queue::{
    VirtqueueAvailableRing, VirtqueueAvailableRingError, VirtqueueDescriptor,
    VirtqueueDescriptorChain, VirtqueueUsedRing, VirtqueueUsedRingError,
};

pub const VIRTIO_BLOCK_DEVICE_ID: u32 = 2;
pub const VIRTIO_BLOCK_QUEUE_COUNT: usize = 1;
pub const VIRTIO_BLOCK_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_BLOCK_QUEUE_SIZES: [u16; VIRTIO_BLOCK_QUEUE_COUNT] = [VIRTIO_BLOCK_QUEUE_SIZE];
pub const VIRTIO_BLOCK_SECTOR_SHIFT: u32 = 9;
pub const VIRTIO_BLOCK_SECTOR_SIZE: u64 = 1 << VIRTIO_BLOCK_SECTOR_SHIFT;
pub const VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE: usize = 8;
pub const VIRTIO_BLOCK_FEATURE_READ_ONLY: u32 = 5;
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
        if cache_type != DriveCacheType::Unsafe {
            return Err(DriveConfigError::UnsupportedCacheType { cache_type });
        }

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
    UnsupportedCacheType {
        cache_type: DriveCacheType,
    },
    UnsupportedIoEngine {
        io_engine: DriveIoEngine,
    },
    UnsupportedRateLimiter,
    UnsupportedSocket,
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
            Self::UnsupportedCacheType { cache_type } => {
                write!(f, "drive cache_type {cache_type} is not supported")
            }
            Self::UnsupportedIoEngine { io_engine } => {
                write!(f, "drive io_engine {io_engine} is not supported")
            }
            Self::UnsupportedRateLimiter => f.write_str("drive rate_limiter is not supported"),
            Self::UnsupportedSocket => f.write_str("drive socket is not supported"),
        }
    }
}

impl std::error::Error for DriveConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBlockConfigSpace {
    capacity_sectors: u64,
    is_read_only: bool,
}

impl VirtioBlockConfigSpace {
    pub const fn new(backing_len: u64, is_read_only: bool) -> Self {
        Self {
            capacity_sectors: backing_len >> VIRTIO_BLOCK_SECTOR_SHIFT,
            is_read_only,
        }
    }

    pub fn from_backing(backing: &BlockFileBacking) -> Self {
        Self::new(backing.len(), backing.is_read_only())
    }

    pub const fn capacity_sectors(self) -> u64 {
        self.capacity_sectors
    }

    pub const fn is_read_only(self) -> bool {
        self.is_read_only
    }

    pub const fn available_features(self) -> u64 {
        let features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
            | virtio_feature_bit(VIRTIO_RING_FEATURE_EVENT_IDX);
        if self.is_read_only {
            features | virtio_feature_bit(VIRTIO_BLOCK_FEATURE_READ_ONLY)
        } else {
            features
        }
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
            descriptor_head: header.index(),
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
        let (status_code, bytes_written_to_guest, outcome) =
            match self.execute_side_effects(memory, backing, device_id) {
                Ok(VirtioBlockRequestSideEffect::Completed {
                    bytes_written_to_guest,
                }) => (
                    VIRTIO_BLOCK_STATUS_OK,
                    bytes_written_to_guest,
                    VirtioBlockRequestExecutionOutcome::Ok,
                ),
                Ok(VirtioBlockRequestSideEffect::Unsupported { request_type }) => (
                    VIRTIO_BLOCK_STATUS_UNSUPPORTED,
                    0,
                    VirtioBlockRequestExecutionOutcome::Unsupported { request_type },
                ),
                Err(error) => (
                    VIRTIO_BLOCK_STATUS_IOERR,
                    0,
                    VirtioBlockRequestExecutionOutcome::IoError { error },
                ),
            };

        let (status_code, bytes_written_to_guest, outcome) =
            normalize_completion_status(status_code, bytes_written_to_guest, outcome);
        self.finish_execution(memory, status_code, bytes_written_to_guest, outcome)
    }

    fn execute_side_effects(
        &self,
        memory: &mut GuestMemory,
        backing: &BlockFileBacking,
        device_id: VirtioBlockDeviceId,
    ) -> Result<VirtioBlockRequestSideEffect, VirtioBlockRequestExecutionError> {
        match self.request_type {
            VirtioBlockRequestType::In => Ok(VirtioBlockRequestSideEffect::Completed {
                bytes_written_to_guest: self.execute_in(memory, backing)?,
            }),
            VirtioBlockRequestType::Out => Ok(VirtioBlockRequestSideEffect::Completed {
                bytes_written_to_guest: self.execute_out(memory, backing)?,
            }),
            VirtioBlockRequestType::Flush => Ok(VirtioBlockRequestSideEffect::Completed {
                bytes_written_to_guest: self.execute_flush(backing)?,
            }),
            VirtioBlockRequestType::GetDeviceId => Ok(VirtioBlockRequestSideEffect::Completed {
                bytes_written_to_guest: self.execute_get_device_id(memory, device_id)?,
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
    ) -> VirtioBlockRequestExecution {
        match write_request_status(memory, self.status, status_code) {
            Ok(()) => {
                let completion = VirtioBlockRequestCompletion::new(
                    self.descriptor_head,
                    bytes_written_to_guest + VIRTIO_BLOCK_STATUS_SIZE,
                );
                VirtioBlockRequestExecution::new(completion, status_code, outcome)
            }
            Err(source) => {
                let completion = VirtioBlockRequestCompletion::new(self.descriptor_head, 0);
                VirtioBlockRequestExecution::new(
                    completion,
                    status_code,
                    VirtioBlockRequestExecutionOutcome::StatusWriteFailed {
                        status_code,
                        source,
                    },
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VirtioBlockRequestSideEffect {
    Completed { bytes_written_to_guest: u32 },
    Unsupported { request_type: u32 },
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

#[derive(Debug)]
pub struct VirtioBlockRequestExecution {
    completion: VirtioBlockRequestCompletion,
    status_code: u8,
    outcome: VirtioBlockRequestExecutionOutcome,
}

impl VirtioBlockRequestExecution {
    pub const fn new(
        completion: VirtioBlockRequestCompletion,
        status_code: u8,
        outcome: VirtioBlockRequestExecutionOutcome,
    ) -> Self {
        Self {
            completion,
            status_code,
            outcome,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBlockQueue {
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
}

impl VirtioBlockQueue {
    pub const fn new(available: VirtqueueAvailableRing, used: VirtqueueUsedRing) -> Self {
        Self { available, used }
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
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
            .map_err(|source| VirtioBlockQueueDispatchError::AvailableRing { source })?
        {
            let descriptor_head = descriptor_chain_head(&chain)?;
            let (completion, outcome) =
                match VirtioBlockRequest::parse(memory, &chain, capacity_sectors) {
                    Ok(request) => {
                        let execution = request.execute(memory, backing, device_id);
                        (
                            execution.completion(),
                            VirtioBlockQueueDispatchOutcome::from_execution(&execution),
                        )
                    }
                    Err(source) => (
                        VirtioBlockRequestCompletion::new(descriptor_head, 0),
                        VirtioBlockQueueDispatchOutcome::ParseError(source),
                    ),
                };

            self.used
                .publish_used_element(
                    memory,
                    completion.descriptor_head(),
                    completion.bytes_written_to_guest(),
                )
                .map_err(|source| VirtioBlockQueueDispatchError::UsedRing {
                    descriptor_head: completion.descriptor_head(),
                    bytes_written_to_guest: completion.bytes_written_to_guest(),
                    source,
                })?;
            dispatch.record(outcome);
        }

        Ok(dispatch)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VirtioBlockQueueDispatch {
    processed_requests: usize,
    successful_requests: usize,
    parse_failures: usize,
    io_errors: usize,
    unsupported_requests: usize,
    status_write_failures: usize,
    first_parse_failure: Option<VirtioBlockRequestError>,
}

impl VirtioBlockQueueDispatch {
    pub const fn processed_requests(&self) -> usize {
        self.processed_requests
    }

    pub const fn successful_requests(&self) -> usize {
        self.successful_requests
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
        self.processed_requests != 0
    }

    fn record(&mut self, outcome: VirtioBlockQueueDispatchOutcome) {
        self.processed_requests += 1;
        match outcome {
            VirtioBlockQueueDispatchOutcome::Ok => {
                self.successful_requests += 1;
            }
            VirtioBlockQueueDispatchOutcome::ParseError(source) => {
                self.parse_failures += 1;
                if self.first_parse_failure.is_none() {
                    self.first_parse_failure = Some(source);
                }
            }
            VirtioBlockQueueDispatchOutcome::IoError => {
                self.io_errors += 1;
            }
            VirtioBlockQueueDispatchOutcome::Unsupported => {
                self.unsupported_requests += 1;
            }
            VirtioBlockQueueDispatchOutcome::StatusWriteFailed => {
                self.status_write_failures += 1;
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum VirtioBlockQueueDispatchOutcome {
    Ok,
    ParseError(VirtioBlockRequestError),
    IoError,
    Unsupported,
    StatusWriteFailed,
}

impl VirtioBlockQueueDispatchOutcome {
    const fn from_execution(execution: &VirtioBlockRequestExecution) -> Self {
        match execution.outcome() {
            VirtioBlockRequestExecutionOutcome::Ok => Self::Ok,
            VirtioBlockRequestExecutionOutcome::IoError { .. } => Self::IoError,
            VirtioBlockRequestExecutionOutcome::Unsupported { .. } => Self::Unsupported,
            VirtioBlockRequestExecutionOutcome::StatusWriteFailed { .. } => Self::StatusWriteFailed,
        }
    }
}

#[derive(Debug)]
pub enum VirtioBlockQueueDispatchError {
    AvailableRing {
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain,
    UsedRing {
        descriptor_head: u16,
        bytes_written_to_guest: u32,
        source: VirtqueueUsedRingError,
    },
}

impl fmt::Display for VirtioBlockQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AvailableRing { source } => {
                write!(
                    f,
                    "failed to pop virtio-block available descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain => {
                f.write_str("virtio-block queue produced an empty descriptor chain")
            }
            Self::UsedRing {
                descriptor_head,
                bytes_written_to_guest,
                source,
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
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::EmptyDescriptorChain => None,
        }
    }
}

fn descriptor_chain_head(
    chain: &VirtqueueDescriptorChain,
) -> Result<u16, VirtioBlockQueueDispatchError> {
    chain
        .descriptors()
        .first()
        .map(|descriptor| descriptor.index())
        .ok_or(VirtioBlockQueueDispatchError::EmptyDescriptorChain)
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

    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange};
    use crate::mmio::{MmioAccess, MmioAccessBytes, MmioBus, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioRegister,
        VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
        VirtqueueAvailableRing, VirtqueueDescriptorChain, VirtqueueUsedRing, read_descriptor_chain,
    };

    use super::{
        BlockFileBacking, BlockFileBackingError, DriveCacheType, DriveConfig, DriveConfigError,
        DriveConfigInput, DriveIdSource, DriveIoEngine, VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE,
        VIRTIO_BLOCK_DEVICE_ID, VIRTIO_BLOCK_FEATURE_READ_ONLY, VIRTIO_BLOCK_ID_BYTES,
        VIRTIO_BLOCK_QUEUE_COUNT, VIRTIO_BLOCK_QUEUE_SIZE, VIRTIO_BLOCK_QUEUE_SIZES,
        VIRTIO_BLOCK_REQUEST_HEADER_SIZE, VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
        VIRTIO_BLOCK_REQUEST_TYPE_GET_ID, VIRTIO_BLOCK_REQUEST_TYPE_IN,
        VIRTIO_BLOCK_REQUEST_TYPE_OUT, VIRTIO_BLOCK_SECTOR_SHIFT, VIRTIO_BLOCK_SECTOR_SIZE,
        VIRTIO_BLOCK_STATUS_IOERR, VIRTIO_BLOCK_STATUS_OK, VIRTIO_BLOCK_STATUS_SIZE,
        VIRTIO_BLOCK_STATUS_UNSUPPORTED, VIRTIO_FEATURE_VERSION_1, VIRTIO_RING_FEATURE_EVENT_IDX,
        VirtioBlockConfigSpace, VirtioBlockDeviceId, VirtioBlockQueue,
        VirtioBlockQueueDispatchError, VirtioBlockRequest, VirtioBlockRequestCompletion,
        VirtioBlockRequestError, VirtioBlockRequestExecutionError,
        VirtioBlockRequestExecutionOutcome, VirtioBlockRequestType, normalize_completion_status,
    };

    static NEXT_TEMP_PATH_ID: AtomicUsize = AtomicUsize::new(0);
    const TEST_MMIO_BASE: u64 = 0x1000_0000;
    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x1000);
    const TEST_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x5000);
    const TEST_USED_RING: GuestAddress = GuestAddress::new(0x6000);
    const TEST_QUEUE_SIZE: u16 = 8;
    const TEST_MEMORY_SIZE: u64 = 0x10_000;
    const TEST_AVAILABLE_RING_IDX_OFFSET: u64 = 2;
    const TEST_AVAILABLE_RING_RING_OFFSET: u64 = 4;
    const TEST_AVAILABLE_RING_ENTRY_SIZE: u64 = 2;
    const TEST_USED_RING_IDX_OFFSET: u64 = 2;
    const TEST_USED_RING_RING_OFFSET: u64 = 4;
    const TEST_USED_RING_ELEMENT_SIZE: u64 = 8;
    const HEADER_ADDR: GuestAddress = GuestAddress::new(0x2000);
    const DATA_ADDR: GuestAddress = GuestAddress::new(0x3000);
    const STATUS_ADDR: GuestAddress = GuestAddress::new(0x4000);
    const TEST_DEVICE_ID: VirtioBlockDeviceId = VirtioBlockDeviceId::new(*b"bangbang-test-id-000");

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
        DriveConfigInput::new("rootfs", "rootfs", path, false)
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
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_DESCRIPTOR_TABLE
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

    fn read_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, used_ring_idx_address())
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
    fn rejects_unsupported_cache_type() {
        let err = validate(input().with_cache_type(DriveCacheType::Writeback))
            .expect_err("Writeback cache should be unsupported");

        assert_eq!(
            err,
            DriveConfigError::UnsupportedCacheType {
                cache_type: DriveCacheType::Writeback,
            }
        );
        assert_eq!(
            err.to_string(),
            "drive cache_type Writeback is not supported"
        );
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
    fn virtio_block_constants_match_firecracker_shape() {
        assert_eq!(VIRTIO_BLOCK_DEVICE_ID, 2);
        assert_eq!(VIRTIO_BLOCK_QUEUE_COUNT, 1);
        assert_eq!(VIRTIO_BLOCK_QUEUE_SIZE, 256);
        assert_eq!(VIRTIO_BLOCK_QUEUE_SIZES, [256]);
        assert_eq!(VIRTIO_BLOCK_SECTOR_SHIFT, 9);
        assert_eq!(VIRTIO_BLOCK_SECTOR_SIZE, 512);
        assert_eq!(VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE, 8);
        assert_eq!(VIRTIO_BLOCK_FEATURE_READ_ONLY, 5);
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
        let config = VirtioBlockConfigSpace::new(4096, false);

        assert_eq!(config.capacity_sectors(), 8);
        assert!(!config.is_read_only());
    }

    #[test]
    fn config_space_truncates_unaligned_tail() {
        assert_eq!(
            VirtioBlockConfigSpace::new(511, false).capacity_sectors(),
            0
        );
        assert_eq!(
            VirtioBlockConfigSpace::new(512, false).capacity_sectors(),
            1
        );
        assert_eq!(
            VirtioBlockConfigSpace::new(4097, false).capacity_sectors(),
            8
        );
    }

    #[test]
    fn config_space_tracks_read_only_feature() {
        let base_features =
            (1_u64 << VIRTIO_FEATURE_VERSION_1) | (1_u64 << VIRTIO_RING_FEATURE_EVENT_IDX);

        assert_eq!(
            VirtioBlockConfigSpace::new(512, false).available_features(),
            base_features
        );
        assert_eq!(
            VirtioBlockConfigSpace::new(512, true).available_features(),
            base_features | (1_u64 << VIRTIO_BLOCK_FEATURE_READ_ONLY)
        );
    }

    #[test]
    fn config_space_can_be_derived_from_backing() {
        let file = temp_file("config-space.img", &[0; 1024]);
        let backing = open_backing(file.as_path(), true).expect("backing should open");
        let config = VirtioBlockConfigSpace::from_backing(&backing);

        assert_eq!(config.capacity_sectors(), 2);
        assert!(config.is_read_only());
    }

    #[test]
    fn config_space_reads_full_and_partial_capacity() {
        let sectors = 0x0102_0304_u64;
        let config = VirtioBlockConfigSpace::new(sectors << VIRTIO_BLOCK_SECTOR_SHIFT, false);
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
        let config = VirtioBlockConfigSpace::new(u64::MAX, false);
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
        let config = VirtioBlockConfigSpace::new(512, false);

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
        let config = VirtioBlockConfigSpace::new(512, false);

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
        assert_eq!(dispatch.parse_failures(), 1);
        assert!(matches!(
            dispatch.first_parse_failure(),
            Some(VirtioBlockRequestError::InvalidDataLength {
                request_type: VirtioBlockRequestType::In,
                len: 1,
            })
        ));
        assert_eq!(dispatch.io_errors(), 0);
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
        assert_eq!(dispatch.io_errors(), 1);
        assert_eq!(dispatch.parse_failures(), 0);
        assert_eq!(read_used_element(&memory, 0), (0, VIRTIO_BLOCK_STATUS_SIZE));
        assert_eq!(read_status(&memory), VIRTIO_BLOCK_STATUS_IOERR);
        assert_eq!(
            fs::read(file.as_path()).expect("file should read"),
            original
        );
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
