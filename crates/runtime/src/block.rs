//! Backend-neutral block-device configuration model.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crate::mmio::{MmioAccessBytes, MmioAccessBytesError};
use crate::virtio_mmio::{
    VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError, VirtioMmioDeviceConfigHandler,
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
        }
    }
}

impl std::error::Error for BlockFileBackingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OpenFile { source }
            | Self::ReadMetadata { source }
            | Self::ReadFile { source }
            | Self::WriteFile { source } => Some(source),
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

    use crate::memory::GuestAddress;
    use crate::mmio::{MmioAccess, MmioAccessBytes, MmioBus, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioRegister,
        VirtioMmioRegisterHandler, VirtioMmioRegisterHandlerError,
    };

    use super::{
        BlockFileBacking, BlockFileBackingError, DriveCacheType, DriveConfig, DriveConfigError,
        DriveConfigInput, DriveIdSource, DriveIoEngine, VIRTIO_BLOCK_CONFIG_CAPACITY_SIZE,
        VIRTIO_BLOCK_DEVICE_ID, VIRTIO_BLOCK_FEATURE_READ_ONLY, VIRTIO_BLOCK_QUEUE_COUNT,
        VIRTIO_BLOCK_QUEUE_SIZE, VIRTIO_BLOCK_QUEUE_SIZES, VIRTIO_BLOCK_SECTOR_SHIFT,
        VIRTIO_BLOCK_SECTOR_SIZE, VIRTIO_FEATURE_VERSION_1, VIRTIO_RING_FEATURE_EVENT_IDX,
        VirtioBlockConfigSpace,
    };

    static NEXT_TEMP_PATH_ID: AtomicUsize = AtomicUsize::new(0);
    const TEST_MMIO_BASE: u64 = 0x1000_0000;

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
