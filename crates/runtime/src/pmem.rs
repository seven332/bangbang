//! Backend-neutral pmem configuration model.

use std::collections::TryReserveError;
use std::ffi::c_void;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::ptr::{self, NonNull};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::mmio::{MmioAccessBytes, MmioAccessBytesError, MmioHandlerError};
use crate::virtio_mmio::{
    VirtioMmioDeviceConfigAccess, VirtioMmioDeviceConfigError, VirtioMmioDeviceConfigHandler,
};

pub const VIRTIO_PMEM_DEVICE_ID: u32 = 27;
pub const VIRTIO_PMEM_QUEUE_COUNT: usize = 1;
pub const VIRTIO_PMEM_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_PMEM_QUEUE_SIZES: [u16; VIRTIO_PMEM_QUEUE_COUNT] = [VIRTIO_PMEM_QUEUE_SIZE];
pub const VIRTIO_PMEM_CONFIG_SPACE_SIZE: usize = 16;
pub const VIRTIO_PMEM_ALIGNMENT: u64 = 2 * 1024 * 1024;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioPmemConfigSpace {
    start: u64,
    size: u64,
}

impl VirtioPmemConfigSpace {
    pub const fn new(start: u64, size: u64) -> Self {
        Self { start, size }
    }

    pub const fn start(self) -> u64 {
        self.start
    }

    pub const fn size(self) -> u64 {
        self.size
    }

    pub const fn available_features(self) -> u64 {
        virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
    }

    pub const fn from_le_bytes(bytes: [u8; VIRTIO_PMEM_CONFIG_SPACE_SIZE]) -> Self {
        let [
            start0,
            start1,
            start2,
            start3,
            start4,
            start5,
            start6,
            start7,
            size0,
            size1,
            size2,
            size3,
            size4,
            size5,
            size6,
            size7,
        ] = bytes;
        Self {
            start: u64::from_le_bytes([
                start0, start1, start2, start3, start4, start5, start6, start7,
            ]),
            size: u64::from_le_bytes([size0, size1, size2, size3, size4, size5, size6, size7]),
        }
    }

    pub fn to_le_bytes(self) -> [u8; VIRTIO_PMEM_CONFIG_SPACE_SIZE] {
        let [
            start0,
            start1,
            start2,
            start3,
            start4,
            start5,
            start6,
            start7,
        ] = self.start.to_le_bytes();
        let [size0, size1, size2, size3, size4, size5, size6, size7] = self.size.to_le_bytes();

        [
            start0, start1, start2, start3, start4, start5, start6, start7, size0, size1, size2,
            size3, size4, size5, size6, size7,
        ]
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioPmemConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let bytes = self.to_le_bytes();
        let bytes = read_virtio_pmem_config_bytes(&bytes, access)?;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemConfigInput {
    id: String,
    path_on_host: String,
    root_device: bool,
    read_only: bool,
    rate_limiter_configured: bool,
}

impl PmemConfigInput {
    pub fn new(id: impl Into<String>, path_on_host: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            path_on_host: path_on_host.into(),
            root_device: false,
            read_only: false,
            rate_limiter_configured: false,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path_on_host(&self) -> &str {
        &self.path_on_host
    }

    pub const fn root_device(&self) -> bool {
        self.root_device
    }

    pub const fn read_only(&self) -> bool {
        self.read_only
    }

    pub const fn rate_limiter_configured(&self) -> bool {
        self.rate_limiter_configured
    }

    pub const fn with_root_device(mut self, root_device: bool) -> Self {
        self.root_device = root_device;
        self
    }

    pub const fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub const fn with_rate_limiter_configured(mut self) -> Self {
        self.rate_limiter_configured = true;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PmemConfig {
    id: String,
    path_on_host: String,
    root_device: bool,
    read_only: bool,
}

impl PmemConfig {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path_on_host(&self) -> &str {
        &self.path_on_host
    }

    pub const fn root_device(&self) -> bool {
        self.root_device
    }

    pub const fn read_only(&self) -> bool {
        self.read_only
    }
}

impl TryFrom<PmemConfigInput> for PmemConfig {
    type Error = PmemConfigError;

    fn try_from(input: PmemConfigInput) -> Result<Self, Self::Error> {
        validate_pmem_id(&input.id)?;

        if input.path_on_host.is_empty() {
            return Err(PmemConfigError::EmptyPathOnHost);
        }

        if input.rate_limiter_configured {
            return Err(PmemConfigError::UnsupportedRateLimiter);
        }

        Ok(Self {
            id: input.id,
            path_on_host: input.path_on_host,
            root_device: input.root_device,
            read_only: input.read_only,
        })
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PmemConfigs {
    configs: Vec<PmemConfig>,
}

impl PmemConfigs {
    pub const fn new() -> Self {
        Self {
            configs: Vec::new(),
        }
    }

    pub fn as_slice(&self) -> &[PmemConfig] {
        &self.configs
    }

    pub fn upsert(&mut self, config: PmemConfig) {
        if let Some(existing) = self
            .configs
            .iter_mut()
            .find(|existing| existing.id == config.id)
        {
            *existing = config;
            return;
        }

        self.configs.push(config);
    }
}

#[derive(Debug)]
pub struct PmemFileBacking {
    file: File,
    len: u64,
    read_only: bool,
}

impl PmemFileBacking {
    pub fn open(config: &PmemConfig) -> Result<Self, PmemFileBackingError> {
        let file = open_pmem_file(config.path_on_host(), config.read_only())?;
        let metadata = file
            .metadata()
            .map_err(|source| PmemFileBackingError::ReadMetadata { source })?;

        if !metadata.file_type().is_file() {
            return Err(PmemFileBackingError::NonRegularFile);
        }

        if metadata.len() == 0 {
            return Err(PmemFileBackingError::ZeroSizedFile);
        }

        Ok(Self {
            file,
            len: metadata.len(),
            read_only: config.read_only(),
        })
    }

    pub fn file(&self) -> &File {
        &self.file
    }

    pub const fn len(&self) -> u64 {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

fn open_pmem_file(path: &str, read_only: bool) -> Result<File, PmemFileBackingError> {
    let mut options = OpenOptions::new();
    options.read(true).write(!read_only);

    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NONBLOCK);
    }

    options
        .open(path)
        .map_err(|source| PmemFileBackingError::OpenFile { source })
}

#[derive(Debug)]
pub enum PmemFileBackingError {
    OpenFile { source: io::Error },
    ReadMetadata { source: io::Error },
    NonRegularFile,
    ZeroSizedFile,
}

impl fmt::Display for PmemFileBackingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenFile { source } => write!(f, "failed to open pmem backing file: {source}"),
            Self::ReadMetadata { source } => {
                write!(f, "failed to read pmem backing file metadata: {source}")
            }
            Self::NonRegularFile => {
                f.write_str("pmem backing path does not reference a regular file")
            }
            Self::ZeroSizedFile => f.write_str("pmem backing file is zero-sized"),
        }
    }
}

impl std::error::Error for PmemFileBackingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OpenFile { source } | Self::ReadMetadata { source } => Some(source),
            Self::NonRegularFile | Self::ZeroSizedFile => None,
        }
    }
}

pub struct PmemBackingMapping {
    address: NonNull<c_void>,
    file_len: u64,
    mapped_len: u64,
    host_size: usize,
    read_only: bool,
    kind: PmemBackingMappingKind,
}

// SAFETY: `PmemBackingMapping` owns a process-local mmap region. Moving the
// owner to another thread does not invalidate the mapping, and `munmap` may run
// from any thread when ownership is dropped.
unsafe impl Send for PmemBackingMapping {}

// SAFETY: Shared references expose only copyable metadata and a raw pointer.
// Safe Rust cannot mutate mapped bytes through this type, and unsafe users must
// uphold normal raw-pointer aliasing and lifetime requirements.
unsafe impl Sync for PmemBackingMapping {}

impl PmemBackingMapping {
    pub fn map(backing: &PmemFileBacking) -> Result<Self, PmemBackingMappingError> {
        let mapped_len = align_pmem_mapping_len(backing.len())?;
        let file_len = usize::try_from(backing.len())
            .map_err(|_| PmemBackingMappingError::FileLengthTooLarge { len: backing.len() })?;
        let host_size = usize::try_from(mapped_len).map_err(|_| {
            PmemBackingMappingError::MappedLengthTooLarge {
                len: backing.len(),
                mapped_len,
            }
        })?;
        let prot = pmem_mapping_protection(backing.is_read_only());
        let address = map_pmem_file(backing.file(), prot, file_len, host_size)?;

        Ok(Self {
            address,
            file_len: backing.len(),
            mapped_len,
            host_size,
            read_only: backing.is_read_only(),
            kind: PmemBackingMappingKind::System,
        })
    }

    #[cfg(test)]
    fn test_mapping(
        file_len: u64,
        mapped_len: u64,
        read_only: bool,
        drop_count: Arc<AtomicUsize>,
    ) -> Self {
        Self {
            address: NonNull::<u8>::dangling().cast(),
            file_len,
            mapped_len,
            host_size: usize::try_from(mapped_len).expect("test mapped length should fit in usize"),
            read_only,
            kind: PmemBackingMappingKind::Test { drop_count },
        }
    }

    pub const fn host_address(&self) -> NonNull<c_void> {
        self.address
    }

    pub const fn file_len(&self) -> u64 {
        self.file_len
    }

    pub const fn mapped_len(&self) -> u64 {
        self.mapped_len
    }

    pub const fn host_size(&self) -> usize {
        self.host_size
    }

    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

impl fmt::Debug for PmemBackingMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PmemBackingMapping")
            .field("file_len", &self.file_len)
            .field("mapped_len", &self.mapped_len)
            .field("host_size", &self.host_size)
            .field("read_only", &self.read_only)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
enum PmemBackingMappingKind {
    System,
    #[cfg(test)]
    Test {
        drop_count: Arc<AtomicUsize>,
    },
}

impl Drop for PmemBackingMapping {
    fn drop(&mut self) {
        match &self.kind {
            PmemBackingMappingKind::System => {
                // SAFETY: system mappings are constructed only after `mmap`
                // succeeds and each `PmemBackingMapping` owns one mapping.
                unsafe {
                    let _ = libc::munmap(self.address.as_ptr(), self.host_size);
                }
            }
            #[cfg(test)]
            PmemBackingMappingKind::Test { drop_count } => {
                drop_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

#[derive(Debug)]
pub enum PmemBackingMappingError {
    MappedLengthOverflow {
        len: u64,
        alignment: u64,
    },
    FileLengthTooLarge {
        len: u64,
    },
    MappedLengthTooLarge {
        len: u64,
        mapped_len: u64,
    },
    MapFile {
        len: usize,
        source: io::Error,
    },
    ReserveAlignedMapping {
        len: usize,
        source: io::Error,
    },
    MapFileOverReservation {
        file_len: usize,
        mapped_len: usize,
        source: io::Error,
        cleanup_source: Option<io::Error>,
    },
    FileMappingReturnedNull {
        file_len: usize,
        mapped_len: usize,
        cleanup_source: Option<io::Error>,
    },
    MmapReturnedNull {
        len: usize,
    },
    FixedMappingMoved {
        cleanup_source: Option<io::Error>,
    },
}

impl fmt::Display for PmemBackingMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MappedLengthOverflow { len, alignment } => write!(
                f,
                "pmem backing length {len} cannot be aligned to {alignment} bytes without overflow"
            ),
            Self::FileLengthTooLarge { len } => {
                write!(f, "pmem backing length {len} does not fit this host")
            }
            Self::MappedLengthTooLarge { len, mapped_len } => write!(
                f,
                "pmem backing length {len} maps to {mapped_len} bytes, which does not fit this host"
            ),
            Self::MapFile { len, source } => {
                write!(
                    f,
                    "failed to mmap pmem backing file with length {len}: {source}"
                )
            }
            Self::ReserveAlignedMapping { len, source } => write!(
                f,
                "failed to reserve aligned pmem mapping with length {len}: {source}"
            ),
            Self::MapFileOverReservation {
                file_len,
                mapped_len,
                source,
                cleanup_source,
            } => {
                if let Some(cleanup_source) = cleanup_source {
                    write!(
                        f,
                        "failed to mmap pmem backing file with length {file_len} over reserved length {mapped_len}: {source}; also failed to clean up the reserved mapping: {cleanup_source}"
                    )
                } else {
                    write!(
                        f,
                        "failed to mmap pmem backing file with length {file_len} over reserved length {mapped_len}: {source}"
                    )
                }
            }
            Self::FileMappingReturnedNull {
                file_len,
                mapped_len,
                cleanup_source,
            } => {
                if let Some(cleanup_source) = cleanup_source {
                    write!(
                        f,
                        "pmem backing file mapping with length {file_len} over reserved length {mapped_len} returned a null address; also failed to clean up the reserved mapping: {cleanup_source}"
                    )
                } else {
                    write!(
                        f,
                        "pmem backing file mapping with length {file_len} over reserved length {mapped_len} returned a null address"
                    )
                }
            }
            Self::MmapReturnedNull { len } => {
                write!(f, "pmem mapping with length {len} returned a null address")
            }
            Self::FixedMappingMoved { cleanup_source, .. } => {
                if let Some(cleanup_source) = cleanup_source {
                    write!(
                        f,
                        "fixed pmem file mapping did not reuse the reserved address; also failed to clean up the reserved mapping: {cleanup_source}"
                    )
                } else {
                    f.write_str("fixed pmem file mapping did not reuse the reserved address")
                }
            }
        }
    }
}

impl std::error::Error for PmemBackingMappingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapFile { source, .. } | Self::ReserveAlignedMapping { source, .. } => {
                Some(source)
            }
            Self::MapFileOverReservation { source, .. } => Some(source),
            Self::MappedLengthOverflow { .. }
            | Self::FileLengthTooLarge { .. }
            | Self::MappedLengthTooLarge { .. }
            | Self::FileMappingReturnedNull { .. }
            | Self::MmapReturnedNull { .. }
            | Self::FixedMappingMoved { .. } => None,
        }
    }
}

fn align_pmem_mapping_len(len: u64) -> Result<u64, PmemBackingMappingError> {
    let remainder = len % VIRTIO_PMEM_ALIGNMENT;
    if remainder == 0 {
        return Ok(len);
    }

    let padding = VIRTIO_PMEM_ALIGNMENT - remainder;
    len.checked_add(padding)
        .ok_or(PmemBackingMappingError::MappedLengthOverflow {
            len,
            alignment: VIRTIO_PMEM_ALIGNMENT,
        })
}

const fn pmem_mapping_protection(read_only: bool) -> libc::c_int {
    let mut prot = libc::PROT_READ;
    if !read_only {
        prot |= libc::PROT_WRITE;
    }
    prot
}

fn map_pmem_file(
    file: &File,
    prot: libc::c_int,
    file_len: usize,
    host_size: usize,
) -> Result<NonNull<c_void>, PmemBackingMappingError> {
    if file_len == host_size {
        return map_pmem_file_exact(file, prot, file_len);
    }

    map_pmem_file_with_aligned_reservation(file, prot, file_len, host_size)
}

fn map_pmem_file_exact(
    file: &File,
    prot: libc::c_int,
    file_len: usize,
) -> Result<NonNull<c_void>, PmemBackingMappingError> {
    // SAFETY: The call requests a new shared mapping for the already-open
    // regular backing file. Lengths are non-zero, fit usize, and the result is
    // checked before ownership is created.
    let address = unsafe {
        libc::mmap(
            ptr::null_mut(),
            file_len,
            prot,
            libc::MAP_SHARED | libc::MAP_NORESERVE,
            file.as_raw_fd(),
            0,
        )
    };

    if address == libc::MAP_FAILED {
        return Err(PmemBackingMappingError::MapFile {
            len: file_len,
            source: io::Error::last_os_error(),
        });
    }

    non_null_mapping(address, file_len)
}

fn map_pmem_file_with_aligned_reservation(
    file: &File,
    prot: libc::c_int,
    file_len: usize,
    host_size: usize,
) -> Result<NonNull<c_void>, PmemBackingMappingError> {
    // SAFETY: The call reserves a private anonymous region with the final
    // aligned length. No Rust references are created from the raw mapping.
    let reserved = unsafe {
        libc::mmap(
            ptr::null_mut(),
            host_size,
            prot,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };

    if reserved == libc::MAP_FAILED {
        return Err(PmemBackingMappingError::ReserveAlignedMapping {
            len: host_size,
            source: io::Error::last_os_error(),
        });
    }

    let reserved = non_null_mapping(reserved, host_size)?;

    // SAFETY: `reserved` owns a live mapping of `host_size` bytes. This maps
    // the file over the prefix at the same address, matching Firecracker's
    // aligned pmem mapping shape.
    let file_address = unsafe {
        libc::mmap(
            reserved.as_ptr(),
            file_len,
            prot,
            libc::MAP_SHARED | libc::MAP_FIXED | libc::MAP_NORESERVE,
            file.as_raw_fd(),
            0,
        )
    };

    if file_address == libc::MAP_FAILED {
        let source = io::Error::last_os_error();
        let cleanup_source = unmap_pmem_region(reserved, host_size).err();
        return Err(PmemBackingMappingError::MapFileOverReservation {
            file_len,
            mapped_len: host_size,
            source,
            cleanup_source,
        });
    }

    let file_address = match NonNull::new(file_address) {
        Some(file_address) => file_address,
        None => {
            let cleanup_source = unmap_pmem_region(reserved, host_size).err();
            return Err(PmemBackingMappingError::FileMappingReturnedNull {
                file_len,
                mapped_len: host_size,
                cleanup_source,
            });
        }
    };

    if file_address != reserved {
        let cleanup_source = unmap_pmem_region(reserved, host_size).err();
        return Err(PmemBackingMappingError::FixedMappingMoved { cleanup_source });
    }

    Ok(reserved)
}

fn non_null_mapping(
    address: *mut c_void,
    len: usize,
) -> Result<NonNull<c_void>, PmemBackingMappingError> {
    let Some(address) = NonNull::new(address) else {
        // SAFETY: `mmap` reported success, so the returned address and length
        // describe a live mapping even if the address is null.
        unsafe {
            let _ = libc::munmap(address, len);
        }

        return Err(PmemBackingMappingError::MmapReturnedNull { len });
    };

    Ok(address)
}

fn unmap_pmem_region(address: NonNull<c_void>, len: usize) -> Result<(), io::Error> {
    // SAFETY: Callers pass only addresses returned by successful `mmap` calls
    // and the same length used to create the mapping.
    let result = unsafe { libc::munmap(address.as_ptr(), len) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

trait PmemBackingMapper {
    fn map(
        &mut self,
        backing: &PmemFileBacking,
    ) -> Result<PmemBackingMapping, PmemBackingMappingError>;
}

#[derive(Debug, Default)]
struct SystemPmemBackingMapper;

impl PmemBackingMapper for SystemPmemBackingMapper {
    fn map(
        &mut self,
        backing: &PmemFileBacking,
    ) -> Result<PmemBackingMapping, PmemBackingMappingError> {
        PmemBackingMapping::map(backing)
    }
}

#[derive(Debug)]
pub struct PreparedPmemDevice {
    id: String,
    backing: PmemFileBacking,
    mapping: PmemBackingMapping,
}

impl PreparedPmemDevice {
    fn from_config_with_mapper(
        config: &PmemConfig,
        mapper: &mut impl PmemBackingMapper,
    ) -> Result<Self, PreparedPmemDeviceError> {
        let backing = PmemFileBacking::open(config).map_err(|source| {
            PreparedPmemDeviceError::OpenBacking {
                pmem_id: config.id().to_string(),
                source,
            }
        })?;
        let mapping =
            mapper
                .map(&backing)
                .map_err(|source| PreparedPmemDeviceError::MapBacking {
                    pmem_id: config.id().to_string(),
                    source,
                })?;

        Ok(Self {
            id: config.id().to_string(),
            backing,
            mapping,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub const fn backing(&self) -> &PmemFileBacking {
        &self.backing
    }

    pub const fn mapping(&self) -> &PmemBackingMapping {
        &self.mapping
    }

    pub fn into_parts(self) -> (String, PmemFileBacking, PmemBackingMapping) {
        (self.id, self.backing, self.mapping)
    }
}

#[derive(Debug, Default)]
pub struct PreparedPmemDevices {
    devices: Vec<PreparedPmemDevice>,
}

impl PreparedPmemDevices {
    pub fn from_configs(configs: &PmemConfigs) -> Result<Self, PreparedPmemDeviceError> {
        Self::from_config_slice(configs.as_slice())
    }

    pub(crate) fn from_config_slice(
        configs: &[PmemConfig],
    ) -> Result<Self, PreparedPmemDeviceError> {
        let mut mapper = SystemPmemBackingMapper;
        Self::from_config_slice_with_mapper(configs, &mut mapper)
    }

    fn from_config_slice_with_mapper(
        configs: &[PmemConfig],
        mapper: &mut impl PmemBackingMapper,
    ) -> Result<Self, PreparedPmemDeviceError> {
        let mut devices = Vec::new();
        devices
            .try_reserve_exact(configs.len())
            .map_err(|source| PreparedPmemDeviceError::AllocateDevices { source })?;

        for config in configs {
            devices.push(PreparedPmemDevice::from_config_with_mapper(config, mapper)?);
        }

        Ok(Self { devices })
    }

    pub fn as_slice(&self) -> &[PreparedPmemDevice] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn into_vec(self) -> Vec<PreparedPmemDevice> {
        self.devices
    }
}

#[derive(Debug)]
pub enum PreparedPmemDeviceError {
    AllocateDevices {
        source: TryReserveError,
    },
    OpenBacking {
        pmem_id: String,
        source: PmemFileBackingError,
    },
    MapBacking {
        pmem_id: String,
        source: PmemBackingMappingError,
    },
}

impl fmt::Display for PreparedPmemDeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AllocateDevices { source } => {
                write!(f, "failed to allocate prepared pmem devices: {source}")
            }
            Self::OpenBacking { pmem_id, source } => {
                write!(f, "failed to prepare pmem device {pmem_id}: {source}")
            }
            Self::MapBacking { pmem_id, source } => {
                write!(f, "failed to map pmem device {pmem_id}: {source}")
            }
        }
    }
}

impl std::error::Error for PreparedPmemDeviceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AllocateDevices { source } => Some(source),
            Self::OpenBacking { source, .. } => Some(source),
            Self::MapBacking { source, .. } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmemConfigError {
    EmptyPmemId,
    InvalidPmemId,
    EmptyPathOnHost,
    UnsupportedRateLimiter,
}

impl fmt::Display for PmemConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPmemId => f.write_str("pmem id must not be empty"),
            Self::InvalidPmemId => {
                f.write_str("pmem id must contain only alphanumeric characters or '_'")
            }
            Self::EmptyPathOnHost => f.write_str("pmem path_on_host must not be empty"),
            Self::UnsupportedRateLimiter => f.write_str("pmem rate_limiter is not supported"),
        }
    }
}

impl std::error::Error for PmemConfigError {}

fn validate_pmem_id(id: &str) -> Result<(), PmemConfigError> {
    if id.is_empty() {
        return Err(PmemConfigError::EmptyPmemId);
    }

    if !id
        .chars()
        .all(|character| character == '_' || character.is_alphanumeric())
    {
        return Err(PmemConfigError::InvalidPmemId);
    }

    Ok(())
}

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

fn read_virtio_pmem_config_bytes(
    bytes: &[u8; VIRTIO_PMEM_CONFIG_SPACE_SIZE],
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

    bytes
        .get(offset..end)
        .ok_or(VirtioMmioDeviceConfigError::UnsupportedRead {
            offset: access.offset(),
            len: access.len(),
        })
}

fn config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: MmioHandlerError::new(format!("virtio-pmem config access bytes failed: {source}")),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::ffi::CString;
    use std::fs;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::UnixListener;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use super::*;

    use crate::memory::GuestAddress;
    use crate::mmio::{MmioAccess, MmioAccessBytes, MmioBus, MmioOperation, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioAccess,
        VirtioMmioDeviceConfigError, decode_virtio_mmio_access,
    };

    const TEST_PMEM_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
    const TEST_PMEM_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(9000);
    static NEXT_TEMP_PATH_ID: AtomicU64 = AtomicU64::new(0);

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

    fn pmem_config(input: PmemConfigInput) -> PmemConfig {
        input.try_into().expect("pmem input should validate")
    }

    fn temp_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_PATH_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-pmem-test-{}-{id}-{name}",
            std::process::id(),
        ))
    }

    fn temp_file(name: &str, bytes: &[u8]) -> TempPath {
        let temp = TempPath {
            path: temp_path(name),
        };
        fs::write(temp.as_path(), bytes).expect("test file should be written");
        temp
    }

    fn temp_sized_file(name: &str, len: u64) -> TempPath {
        let temp = TempPath {
            path: temp_path(name),
        };
        let file = fs::File::create(temp.as_path()).expect("test file should be created");
        file.set_len(len).expect("test file size should be set");
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
        dir.join(format!("bb-pmem-{}-{id}-{name}", std::process::id()))
    }

    fn missing_path(name: &str) -> PathBuf {
        temp_path(name)
    }

    fn config_for_path(path: &Path, read_only: bool) -> PmemConfig {
        pmem_config(
            PmemConfigInput::new("pmem0", path.to_string_lossy().into_owned())
                .with_read_only(read_only),
        )
    }

    fn open_backing(path: &Path, read_only: bool) -> Result<PmemFileBacking, PmemFileBackingError> {
        PmemFileBacking::open(&config_for_path(path, read_only))
    }

    fn map_backing(path: &Path, read_only: bool) -> PmemBackingMapping {
        let backing = open_backing(path, read_only).expect("pmem backing should open");
        PmemBackingMapping::map(&backing).expect("pmem backing should map")
    }

    #[derive(Debug)]
    struct ScriptedPmemBackingMapper {
        drop_count: Arc<AtomicUsize>,
        calls: usize,
        fail_on_call: usize,
    }

    impl ScriptedPmemBackingMapper {
        fn new(drop_count: Arc<AtomicUsize>, fail_on_call: usize) -> Self {
            Self {
                drop_count,
                calls: 0,
                fail_on_call,
            }
        }
    }

    impl PmemBackingMapper for ScriptedPmemBackingMapper {
        fn map(
            &mut self,
            backing: &PmemFileBacking,
        ) -> Result<PmemBackingMapping, PmemBackingMappingError> {
            self.calls += 1;
            if self.calls == self.fail_on_call {
                return Err(PmemBackingMappingError::MapFile {
                    len: usize::try_from(backing.len())
                        .expect("test backing length should fit usize"),
                    source: io::Error::other("scripted pmem map failure"),
                });
            }

            Ok(PmemBackingMapping::test_mapping(
                backing.len(),
                align_pmem_mapping_len(backing.len()).expect("test mapping length should align"),
                backing.is_read_only(),
                Arc::clone(&self.drop_count),
            ))
        }
    }

    fn device_config_read_access(offset: u64, len: u64) -> VirtioMmioDeviceConfigAccess {
        let operation =
            MmioOperation::read(mmio_access(offset, len)).expect("read operation should build");
        decode_device_config_access(operation)
    }

    fn device_config_write_access(
        offset: u64,
        data: MmioAccessBytes,
    ) -> VirtioMmioDeviceConfigAccess {
        let len = u64::try_from(data.len()).expect("test write length should fit u64");
        let operation = MmioOperation::write(mmio_access(offset, len), data)
            .expect("write operation should build");
        decode_device_config_access(operation)
    }

    fn mmio_access(offset: u64, len: u64) -> MmioAccess {
        let mut bus = MmioBus::new();
        bus.insert(
            TEST_PMEM_MMIO_REGION_ID,
            TEST_PMEM_MMIO_BASE,
            VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
        )
        .expect("test MMIO region should insert");
        let start = TEST_PMEM_MMIO_BASE
            .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset)
            .expect("test MMIO address should not overflow");
        bus.lookup(start, len)
            .expect("test MMIO access should look up")
    }

    fn decode_device_config_access(operation: MmioOperation) -> VirtioMmioDeviceConfigAccess {
        match decode_virtio_mmio_access(&operation).expect("access should decode") {
            VirtioMmioAccess::DeviceConfig(access) => access,
            _ => panic!("test access should target device config"),
        }
    }

    fn read_pmem_config(
        config: &VirtioPmemConfigSpace,
        offset: u64,
        len: u64,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        config.read_device_config(device_config_read_access(offset, len))
    }

    fn write_pmem_config(
        config: &mut VirtioPmemConfigSpace,
        offset: u64,
        data: &[u8],
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        let data = MmioAccessBytes::new(data).expect("write bytes should be valid");
        config.write_device_config(device_config_write_access(offset, data), data)
    }

    #[test]
    fn virtio_pmem_constants_match_firecracker_shape() {
        assert_eq!(VIRTIO_PMEM_DEVICE_ID, 27);
        assert_eq!(VIRTIO_PMEM_QUEUE_COUNT, 1);
        assert_eq!(VIRTIO_PMEM_QUEUE_SIZE, 256);
        assert_eq!(VIRTIO_PMEM_QUEUE_SIZES, [VIRTIO_PMEM_QUEUE_SIZE]);
        assert_eq!(VIRTIO_PMEM_CONFIG_SPACE_SIZE, 16);
        assert_eq!(VIRTIO_PMEM_ALIGNMENT, 2 * 1024 * 1024);
    }

    #[test]
    fn virtio_pmem_config_space_tracks_start_and_size() {
        let config = VirtioPmemConfigSpace::new(0x1000_0000, 0x0200_0000);

        assert_eq!(config.start(), 0x1000_0000);
        assert_eq!(config.size(), 0x0200_0000);
    }

    #[test]
    fn virtio_pmem_config_space_uses_firecracker_little_endian_layout() {
        let config = VirtioPmemConfigSpace::new(0x0102_0304_0506_0708, 0x1112_1314_1516_1718);
        let bytes = [
            0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13,
            0x12, 0x11,
        ];

        assert_eq!(config.to_le_bytes(), bytes);
        assert_eq!(VirtioPmemConfigSpace::from_le_bytes(bytes), config);
    }

    #[test]
    fn virtio_pmem_config_space_preserves_u64_boundaries() {
        let config = VirtioPmemConfigSpace::new(u64::MAX, u64::MAX);

        assert_eq!(config.to_le_bytes(), [0xff; VIRTIO_PMEM_CONFIG_SPACE_SIZE]);
        assert_eq!(
            VirtioPmemConfigSpace::from_le_bytes([0xff; VIRTIO_PMEM_CONFIG_SPACE_SIZE]),
            config
        );
    }

    #[test]
    fn virtio_pmem_config_space_advertises_modern_virtio_feature() {
        let config = VirtioPmemConfigSpace::new(0, 0);

        assert_eq!(
            config.available_features(),
            1_u64 << VIRTIO_FEATURE_VERSION_1
        );
    }

    #[test]
    fn virtio_pmem_config_space_reads_within_layout() {
        let config = VirtioPmemConfigSpace::new(0x0102_0304_0506_0708, 0x1112_1314_1516_1718);

        assert_eq!(
            read_pmem_config(&config, 0, 8)
                .expect("start read should succeed")
                .as_slice(),
            &0x0102_0304_0506_0708_u64.to_le_bytes()
        );
        assert_eq!(
            read_pmem_config(&config, 8, 8)
                .expect("size read should succeed")
                .as_slice(),
            &0x1112_1314_1516_1718_u64.to_le_bytes()
        );
        assert_eq!(
            read_pmem_config(&config, 4, 4)
                .expect("partial read should succeed")
                .as_slice(),
            &[0x04, 0x03, 0x02, 0x01]
        );
        assert_eq!(
            read_pmem_config(&config, 15, 1)
                .expect("last byte read should succeed")
                .as_slice(),
            &[0x11]
        );
    }

    #[test]
    fn virtio_pmem_config_space_rejects_out_of_bounds_reads() {
        let config = VirtioPmemConfigSpace::new(0, 0);

        assert_eq!(
            read_pmem_config(&config, 16, 1),
            Err(VirtioMmioDeviceConfigError::UnsupportedRead { offset: 16, len: 1 })
        );
        assert_eq!(
            read_pmem_config(&config, 15, 2),
            Err(VirtioMmioDeviceConfigError::UnsupportedRead { offset: 15, len: 2 })
        );
    }

    #[test]
    fn virtio_pmem_config_space_rejects_guest_writes() {
        let mut config = VirtioPmemConfigSpace::new(0, 0);

        assert_eq!(
            write_pmem_config(&mut config, 0, &[1, 2, 3, 4]),
            Err(VirtioMmioDeviceConfigError::UnsupportedWrite { offset: 0, len: 4 })
        );
        assert_eq!(config, VirtioPmemConfigSpace::new(0, 0));
    }

    #[test]
    fn input_defaults_to_firecracker_pmem_defaults() {
        let input = PmemConfigInput::new("pmem0", "/tmp/pmem.img");

        assert_eq!(input.id(), "pmem0");
        assert_eq!(input.path_on_host(), "/tmp/pmem.img");
        assert!(!input.root_device());
        assert!(!input.read_only());
        assert!(!input.rate_limiter_configured());
    }

    #[test]
    fn config_accepts_firecracker_id_character_set() {
        let config = pmem_config(PmemConfigInput::new("pmem_\u{00e9}1", "/tmp/pmem.img"));

        assert_eq!(config.id(), "pmem_\u{00e9}1");
    }

    #[test]
    fn config_rejects_empty_pmem_id() {
        let err = PmemConfig::try_from(PmemConfigInput::new("", "/tmp/pmem.img"))
            .expect_err("empty pmem id should fail");

        assert_eq!(err, PmemConfigError::EmptyPmemId);
        assert_eq!(err.to_string(), "pmem id must not be empty");
    }

    #[test]
    fn config_rejects_invalid_pmem_id_without_echoing_it() {
        let invalid = "bad/id\nsecret";
        let err = PmemConfig::try_from(PmemConfigInput::new(invalid, "/tmp/pmem.img"))
            .expect_err("invalid pmem id should fail");

        assert_eq!(err, PmemConfigError::InvalidPmemId);
        assert_eq!(
            err.to_string(),
            "pmem id must contain only alphanumeric characters or '_'"
        );
        assert!(!err.to_string().contains(invalid));
    }

    #[test]
    fn config_rejects_empty_path_on_host() {
        let err = PmemConfig::try_from(PmemConfigInput::new("pmem0", ""))
            .expect_err("empty pmem path should fail");

        assert_eq!(err, PmemConfigError::EmptyPathOnHost);
        assert_eq!(err.to_string(), "pmem path_on_host must not be empty");
    }

    #[test]
    fn upsert_replaces_matching_id_without_mutating_others() {
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new("pmem0", "/tmp/old.img")));
        configs.upsert(pmem_config(PmemConfigInput::new("pmem1", "/tmp/other.img")));
        configs.upsert(pmem_config(
            PmemConfigInput::new("pmem0", "/tmp/new.img")
                .with_root_device(true)
                .with_read_only(true),
        ));

        assert_eq!(configs.as_slice().len(), 2);
        assert_eq!(configs.as_slice()[0].id(), "pmem0");
        assert_eq!(configs.as_slice()[0].path_on_host(), "/tmp/new.img");
        assert!(configs.as_slice()[0].root_device());
        assert!(configs.as_slice()[0].read_only());
        assert_eq!(configs.as_slice()[1].id(), "pmem1");
        assert_eq!(configs.as_slice()[1].path_on_host(), "/tmp/other.img");
    }

    #[test]
    fn file_backing_opens_regular_file_read_only() {
        let file = temp_file("readonly-pmem.img", b"pmem");
        let backing = open_backing(file.as_path(), true).expect("pmem backing should open");

        assert_eq!(backing.len(), 4);
        assert!(backing.is_read_only());
        assert_eq!(
            backing
                .file()
                .metadata()
                .expect("opened pmem backing should have metadata")
                .len(),
            4
        );
    }

    #[test]
    fn file_backing_opens_regular_file_writable() {
        let file = temp_file("writable-pmem.img", b"pmem");
        let backing = open_backing(file.as_path(), false).expect("pmem backing should open");

        assert_eq!(backing.len(), 4);
        assert!(!backing.is_read_only());
    }

    #[test]
    fn file_backing_rejects_missing_path_without_echoing_it() {
        let path = missing_path("secret-missing-pmem.img");
        let err = open_backing(&path, true).expect_err("missing pmem backing should fail");

        assert!(matches!(err, PmemFileBackingError::OpenFile { .. }));
        assert_eq!(
            err.source()
                .and_then(|source| source.downcast_ref::<io::Error>())
                .map(io::Error::kind),
            Some(io::ErrorKind::NotFound)
        );
        assert!(!err.to_string().contains("secret-missing-pmem"));
    }

    #[test]
    fn file_backing_rejects_directory_path() {
        let dir = temp_dir("dir-pmem.img");
        let err = open_backing(dir.as_path(), true).expect_err("directory backing should fail");

        assert!(matches!(err, PmemFileBackingError::NonRegularFile));
        assert_eq!(
            err.to_string(),
            "pmem backing path does not reference a regular file"
        );
        assert!(err.source().is_none());
    }

    #[test]
    fn file_backing_rejects_fifo_path_without_blocking() {
        let fifo = temp_fifo("fifo-pmem.img");
        let err = open_backing(fifo.as_path(), true).expect_err("FIFO backing should fail");

        assert!(matches!(err, PmemFileBackingError::NonRegularFile));
    }

    #[test]
    fn file_backing_rejects_socket_path_without_blocking() {
        let (socket, listener) = temp_socket("socket-pmem.img");
        let err = open_backing(socket.as_path(), true).expect_err("socket backing should fail");
        drop(listener);

        assert!(matches!(
            err,
            PmemFileBackingError::OpenFile { .. } | PmemFileBackingError::NonRegularFile
        ));
        assert!(!err.to_string().contains("socket-pmem"));
    }

    #[test]
    fn file_backing_rejects_zero_sized_file() {
        let file = temp_file("empty-pmem.img", b"");
        let err = open_backing(file.as_path(), true).expect_err("empty pmem backing should fail");

        assert!(matches!(err, PmemFileBackingError::ZeroSizedFile));
        assert_eq!(err.to_string(), "pmem backing file is zero-sized");
        assert!(err.source().is_none());
    }

    #[test]
    fn backing_mapping_maps_unaligned_file_to_2m_region() {
        let file = temp_file("mapped-unaligned-pmem.img", b"pmem");
        let mapping = map_backing(file.as_path(), false);
        let mut bytes = [0; 4];
        let last_offset = mapping
            .host_size()
            .checked_sub(1)
            .expect("mapping should be non-empty");

        // SAFETY: `mapping` owns a live mapping whose first `file_len` bytes
        // are backed by the test file. The final byte is inside the retained
        // aligned reservation, and `bytes` is a valid destination.
        unsafe {
            std::ptr::copy_nonoverlapping(
                mapping.host_address().as_ptr().cast::<u8>(),
                bytes.as_mut_ptr(),
                bytes.len(),
            );
            assert_eq!(
                mapping
                    .host_address()
                    .as_ptr()
                    .cast::<u8>()
                    .add(last_offset)
                    .read(),
                0
            );
        }

        assert_eq!(mapping.file_len(), 4);
        assert_eq!(mapping.mapped_len(), VIRTIO_PMEM_ALIGNMENT);
        assert_eq!(
            mapping.host_size(),
            usize::try_from(VIRTIO_PMEM_ALIGNMENT).expect("alignment should fit usize")
        );
        assert!(!mapping.is_read_only());
        assert_eq!(&bytes, b"pmem");
    }

    #[test]
    fn backing_mapping_keeps_aligned_file_length() {
        let file = temp_sized_file("mapped-aligned-pmem.img", VIRTIO_PMEM_ALIGNMENT);
        let mapping = map_backing(file.as_path(), true);

        assert_eq!(mapping.file_len(), VIRTIO_PMEM_ALIGNMENT);
        assert_eq!(mapping.mapped_len(), VIRTIO_PMEM_ALIGNMENT);
        assert_eq!(
            mapping.host_size(),
            usize::try_from(VIRTIO_PMEM_ALIGNMENT).expect("alignment should fit usize")
        );
        assert!(mapping.is_read_only());
    }

    #[test]
    fn backing_mapping_writable_mapping_updates_file() {
        let file = temp_file("mapped-writable-pmem.img", b"pmem");
        let mapping = map_backing(file.as_path(), false);

        // SAFETY: `mapping` owns a live writable mapping because the backing
        // was opened with read_only=false. The write stays within file_len.
        unsafe {
            mapping.host_address().as_ptr().cast::<u8>().write(b'P');
            let result = libc::msync(
                mapping.host_address().as_ptr(),
                usize::try_from(mapping.file_len()).expect("file length should fit usize"),
                libc::MS_SYNC,
            );
            assert_eq!(result, 0, "msync failed: {}", io::Error::last_os_error());
        }
        drop(mapping);

        assert_eq!(
            fs::read(file.as_path()).expect("test file should read"),
            b"Pmem"
        );
    }

    #[test]
    fn backing_mapping_debug_omits_host_address_and_path() {
        let file = temp_file("secret-debug-pmem.img", b"pmem");
        let mapping = map_backing(file.as_path(), true);
        let debug = format!("{mapping:?}");
        let host_address = format!("{:p}", mapping.host_address().as_ptr());

        assert!(!debug.contains(&host_address));
        assert!(!debug.contains("secret-debug-pmem"));
        assert!(debug.contains("file_len"));
        assert!(debug.contains("mapped_len"));
    }

    #[test]
    fn backing_mapping_alignment_rejects_overflow() {
        let err = align_pmem_mapping_len(u64::MAX)
            .expect_err("maximum length should not align without overflow");

        assert!(matches!(
            err,
            PmemBackingMappingError::MappedLengthOverflow {
                len: u64::MAX,
                alignment: VIRTIO_PMEM_ALIGNMENT,
            }
        ));
    }

    #[test]
    fn prepared_devices_open_all_configured_backings() {
        let first = temp_file("first-pmem.img", b"first");
        let second = temp_file("second-pmem.img", b"second");
        let configs = [
            pmem_config(PmemConfigInput::new(
                "pmem0",
                first.as_path().to_string_lossy().into_owned(),
            )),
            pmem_config(
                PmemConfigInput::new("pmem1", second.as_path().to_string_lossy().into_owned())
                    .with_read_only(true),
            ),
        ];

        let prepared =
            PreparedPmemDevices::from_config_slice(&configs).expect("pmem devices should prepare");

        assert_eq!(prepared.len(), 2);
        assert!(!prepared.is_empty());
        assert_eq!(prepared.as_slice()[0].id(), "pmem0");
        assert_eq!(prepared.as_slice()[0].backing().len(), 5);
        assert!(!prepared.as_slice()[0].backing().is_read_only());
        assert_eq!(prepared.as_slice()[0].mapping().file_len(), 5);
        assert_eq!(
            prepared.as_slice()[0].mapping().mapped_len(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert!(!prepared.as_slice()[0].mapping().is_read_only());
        assert_eq!(prepared.as_slice()[1].id(), "pmem1");
        assert_eq!(prepared.as_slice()[1].backing().len(), 6);
        assert!(prepared.as_slice()[1].backing().is_read_only());
        assert_eq!(prepared.as_slice()[1].mapping().file_len(), 6);
        assert_eq!(
            prepared.as_slice()[1].mapping().mapped_len(),
            VIRTIO_PMEM_ALIGNMENT
        );
        assert!(prepared.as_slice()[1].mapping().is_read_only());
    }

    #[test]
    fn prepared_devices_report_id_without_echoing_path() {
        let valid = temp_file("valid-pmem.img", b"valid");
        let missing = missing_path("secret-prepared-pmem.img");
        let configs = [
            pmem_config(PmemConfigInput::new(
                "pmem0",
                valid.as_path().to_string_lossy().into_owned(),
            )),
            pmem_config(PmemConfigInput::new(
                "pmem1",
                missing.to_string_lossy().into_owned(),
            )),
        ];

        let err = PreparedPmemDevices::from_config_slice(&configs)
            .expect_err("missing pmem backing should fail preparation");

        assert!(matches!(
            err,
            PreparedPmemDeviceError::OpenBacking {
                ref pmem_id,
                source: PmemFileBackingError::OpenFile { .. },
            } if pmem_id == "pmem1"
        ));
        assert!(err.to_string().contains("pmem1"));
        assert!(!err.to_string().contains("secret-prepared-pmem"));
    }

    #[test]
    fn prepared_devices_cleanup_previous_mappings_after_later_map_failure() {
        let first = temp_file("first-cleanup-pmem.img", b"first");
        let second = temp_file("second-cleanup-pmem.img", b"second");
        let configs = [
            pmem_config(PmemConfigInput::new(
                "pmem0",
                first.as_path().to_string_lossy().into_owned(),
            )),
            pmem_config(PmemConfigInput::new(
                "pmem1",
                second.as_path().to_string_lossy().into_owned(),
            )),
        ];
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = ScriptedPmemBackingMapper::new(Arc::clone(&drop_count), 2);

        let err = PreparedPmemDevices::from_config_slice_with_mapper(&configs, &mut mapper)
            .expect_err("second pmem mapping should fail");

        assert!(matches!(
            err,
            PreparedPmemDeviceError::MapBacking {
                ref pmem_id,
                source: PmemBackingMappingError::MapFile { .. },
            } if pmem_id == "pmem1"
        ));
        assert_eq!(mapper.calls, 2);
        assert_eq!(drop_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn prepared_devices_report_mapping_error_without_echoing_path() {
        let file = temp_file("secret-map-failure-pmem.img", b"pmem");
        let configs = [pmem_config(PmemConfigInput::new(
            "pmem0",
            file.as_path().to_string_lossy().into_owned(),
        ))];
        let drop_count = Arc::new(AtomicUsize::new(0));
        let mut mapper = ScriptedPmemBackingMapper::new(drop_count, 1);

        let err = PreparedPmemDevices::from_config_slice_with_mapper(&configs, &mut mapper)
            .expect_err("pmem mapping should fail");

        assert!(matches!(
            err,
            PreparedPmemDeviceError::MapBacking {
                ref pmem_id,
                source: PmemBackingMappingError::MapFile { .. },
            } if pmem_id == "pmem0"
        ));
        assert!(err.to_string().contains("pmem0"));
        assert!(!err.to_string().contains("secret-map-failure-pmem"));
    }
}
