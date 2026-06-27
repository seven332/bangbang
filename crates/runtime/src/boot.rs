use std::collections::TryReserveError;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};

use crate::memory::{
    GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryLayout,
    GuestMemoryRange, aarch64,
};

pub const DEFAULT_KERNEL_COMMAND_LINE: &str = "reboot=k panic=1 nomodule 8250.nr_uarts=0 i8042.noaux i8042.nomux i8042.dumbkbd swiotlb=noforce";

const ARM64_IMAGE_HEADER_SIZE: usize = 64;
const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;
const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;
const ARM64_LEGACY_TEXT_OFFSET: u64 = 0x80000;
const ARM64_BASE_ALIGNMENT: u64 = 0x20_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootSource {
    kernel_image_path: PathBuf,
    initrd_path: Option<PathBuf>,
    boot_args: Option<String>,
}

impl BootSource {
    pub fn new(kernel_image_path: impl Into<PathBuf>) -> Self {
        Self {
            kernel_image_path: kernel_image_path.into(),
            initrd_path: None,
            boot_args: None,
        }
    }

    pub fn with_initrd_path(mut self, initrd_path: impl Into<PathBuf>) -> Self {
        self.initrd_path = Some(initrd_path.into());
        self
    }

    pub fn with_boot_args(mut self, boot_args: impl Into<String>) -> Self {
        self.boot_args = Some(boot_args.into());
        self
    }

    pub fn kernel_image_path(&self) -> &Path {
        &self.kernel_image_path
    }

    pub fn initrd_path(&self) -> Option<&Path> {
        self.initrd_path.as_deref()
    }

    pub fn boot_args(&self) -> Option<&str> {
        self.boot_args.as_deref()
    }

    pub fn load(
        &self,
        layout: &GuestMemoryLayout,
        memory: &mut GuestMemory,
    ) -> Result<LoadedBootSource, BootSourceLoadError> {
        load_boot_source(self, layout, memory)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelCommandLine {
    text: String,
    bytes_with_nul: Vec<u8>,
}

impl KernelCommandLine {
    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub fn as_bytes_with_nul(&self) -> &[u8] {
        &self.bytes_with_nul
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedBootSource {
    pub command_line: KernelCommandLine,
    pub kernel: LoadedKernel,
    pub initrd: Option<LoadedInitrd>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedKernel {
    pub base_address: GuestAddress,
    pub load_address: GuestAddress,
    pub entry_address: GuestAddress,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadedInitrd {
    pub address: GuestAddress,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootPayloadKind {
    Kernel,
    Initrd,
}

impl fmt::Display for BootPayloadKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel => f.write_str("kernel image"),
            Self::Initrd => f.write_str("initrd image"),
        }
    }
}

#[derive(Debug)]
pub enum BootSourceLoadError {
    EmptyPath {
        payload: BootPayloadKind,
    },
    OpenFile {
        payload: BootPayloadKind,
        source: io::Error,
    },
    ReadMetadata {
        payload: BootPayloadKind,
        source: io::Error,
    },
    NonRegularFile {
        payload: BootPayloadKind,
    },
    EmptyPayload {
        payload: BootPayloadKind,
    },
    PayloadTooLargeForHost {
        payload: BootPayloadKind,
        size: u64,
    },
    PayloadBufferAllocationFailed {
        payload: BootPayloadKind,
        size: usize,
        source: TryReserveError,
    },
    PayloadSizeChanged {
        payload: BootPayloadKind,
        expected_size: u64,
        actual_size: u64,
    },
    ReadFile {
        payload: BootPayloadKind,
        source: io::Error,
    },
    CommandLine(BootCommandLineError),
    KernelImage(KernelImageError),
    InvalidLayout {
        source: GuestMemoryError,
    },
    PayloadRangeInvalid {
        payload: BootPayloadKind,
        source: GuestMemoryError,
    },
    PayloadDoesNotFit {
        payload: BootPayloadKind,
        range: GuestMemoryRange,
    },
    PayloadOverlapsFdt {
        payload: BootPayloadKind,
        end_exclusive: GuestAddress,
        fdt_address: GuestAddress,
    },
    PayloadsOverlap {
        first_payload: BootPayloadKind,
        first_range: GuestMemoryRange,
        second_payload: BootPayloadKind,
        second_range: GuestMemoryRange,
    },
    NoInitrdSpace {
        size: u64,
    },
    GuestMemoryWrite {
        payload: BootPayloadKind,
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for BootSourceLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPath { payload } => {
                write!(f, "{payload} path must not be empty")
            }
            Self::OpenFile { payload, source } => {
                write!(f, "failed to open {payload}: {source}")
            }
            Self::ReadMetadata { payload, source } => {
                write!(f, "failed to read {payload} metadata: {source}")
            }
            Self::NonRegularFile { payload } => {
                write!(f, "{payload} path does not reference a regular file")
            }
            Self::EmptyPayload { payload } => {
                write!(f, "{payload} payload must not be empty")
            }
            Self::PayloadTooLargeForHost { payload, size } => {
                write!(
                    f,
                    "{payload} payload of {size} bytes is too large for this host"
                )
            }
            Self::PayloadBufferAllocationFailed {
                payload,
                size,
                source,
            } => {
                write!(
                    f,
                    "failed to allocate {size} byte read buffer for {payload}: {source}"
                )
            }
            Self::PayloadSizeChanged {
                payload,
                expected_size,
                actual_size,
            } => {
                write!(
                    f,
                    "{payload} size changed while loading: expected {expected_size} bytes, read {actual_size} bytes"
                )
            }
            Self::ReadFile { payload, source } => {
                write!(f, "failed to read {payload}: {source}")
            }
            Self::CommandLine(source) => {
                write!(f, "invalid kernel command line: {source}")
            }
            Self::KernelImage(source) => {
                write!(f, "invalid kernel image: {source}")
            }
            Self::InvalidLayout { source } => {
                write!(f, "invalid boot memory layout: {source}")
            }
            Self::PayloadRangeInvalid { payload, source } => {
                write!(f, "invalid guest memory range for {payload}: {source}")
            }
            Self::PayloadDoesNotFit { payload, range } => {
                write!(
                    f,
                    "{payload} guest memory range {range} is not fully backed by guest memory"
                )
            }
            Self::PayloadOverlapsFdt {
                payload,
                end_exclusive,
                fdt_address,
            } => {
                write!(
                    f,
                    "{payload} end address {end_exclusive} overlaps reserved FDT address {fdt_address}"
                )
            }
            Self::PayloadsOverlap {
                first_payload,
                first_range,
                second_payload,
                second_range,
            } => {
                write!(
                    f,
                    "{first_payload} guest memory range {first_range} overlaps {second_payload} range {second_range}"
                )
            }
            Self::NoInitrdSpace { size } => {
                write!(
                    f,
                    "initrd image payload of {size} bytes cannot fit before the reserved FDT address"
                )
            }
            Self::GuestMemoryWrite { payload, source } => {
                write!(f, "failed to write {payload} into guest memory: {source}")
            }
        }
    }
}

impl std::error::Error for BootSourceLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OpenFile { source, .. }
            | Self::ReadMetadata { source, .. }
            | Self::ReadFile { source, .. } => Some(source),
            Self::PayloadBufferAllocationFailed { source, .. } => Some(source),
            Self::CommandLine(source) => Some(source),
            Self::KernelImage(source) => Some(source),
            Self::InvalidLayout { source } | Self::PayloadRangeInvalid { source, .. } => {
                Some(source)
            }
            Self::GuestMemoryWrite { source, .. } => Some(source),
            Self::EmptyPath { .. }
            | Self::NonRegularFile { .. }
            | Self::EmptyPayload { .. }
            | Self::PayloadTooLargeForHost { .. }
            | Self::PayloadSizeChanged { .. }
            | Self::PayloadDoesNotFit { .. }
            | Self::PayloadOverlapsFdt { .. }
            | Self::PayloadsOverlap { .. }
            | Self::NoInitrdSpace { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootCommandLineError {
    ContainsNul,
    TooLarge {
        size_with_nul: usize,
        max_size: usize,
    },
}

impl fmt::Display for BootCommandLineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ContainsNul => f.write_str("contains a NUL byte"),
            Self::TooLarge {
                size_with_nul,
                max_size,
            } => {
                write!(
                    f,
                    "{size_with_nul} bytes including trailing NUL exceeds {max_size} byte limit"
                )
            }
        }
    }
}

impl std::error::Error for BootCommandLineError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelImageError {
    HeaderTooShort {
        size: u64,
    },
    InvalidMagic {
        magic: u32,
    },
    BaseAddressNotAligned {
        address: GuestAddress,
        alignment: u64,
    },
    LoadAddressOverflow {
        base_address: GuestAddress,
        text_offset: u64,
    },
}

impl fmt::Display for KernelImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::HeaderTooShort { size } => {
                write!(f, "arm64 Image header is too short: {size} bytes available")
            }
            Self::InvalidMagic { magic } => {
                write!(f, "arm64 Image magic 0x{magic:08x} is not supported")
            }
            Self::BaseAddressNotAligned { address, alignment } => {
                write!(
                    f,
                    "arm64 Image base address {address} is not aligned to {alignment} bytes"
                )
            }
            Self::LoadAddressOverflow {
                base_address,
                text_offset,
            } => {
                write!(
                    f,
                    "arm64 Image load address overflows: base={base_address}, text_offset={text_offset}"
                )
            }
        }
    }
}

impl std::error::Error for KernelImageError {}

#[derive(Debug)]
struct PreparedKernelPayload {
    file: File,
    loaded: LoadedKernel,
}

#[derive(Debug)]
struct PreparedInitrdPayload {
    file: File,
    loaded: LoadedInitrd,
}

#[derive(Debug, Clone, Copy)]
struct Arm64ImageHeader {
    text_offset: u64,
    image_size: u64,
    magic: u32,
}

pub fn load_boot_source(
    source: &BootSource,
    layout: &GuestMemoryLayout,
    memory: &mut GuestMemory,
) -> Result<LoadedBootSource, BootSourceLoadError> {
    let command_line = validate_command_line(source.boot_args())?;
    let kernel = prepare_kernel_payload(source.kernel_image_path(), layout, memory)?;
    let initrd = match source.initrd_path() {
        Some(path) => Some(prepare_initrd_payload(path, layout, memory)?),
        None => None,
    };
    if let Some(initrd_payload) = &initrd {
        validate_payloads_do_not_overlap(&kernel.loaded, &initrd_payload.loaded)?;
    }

    let kernel_bytes = read_payload_file(kernel.file, BootPayloadKind::Kernel, kernel.loaded.size)?;
    let initrd_bytes = match initrd {
        Some(initrd_payload) => Some((
            read_payload_file(
                initrd_payload.file,
                BootPayloadKind::Initrd,
                initrd_payload.loaded.size,
            )?,
            initrd_payload.loaded,
        )),
        None => None,
    };

    memory
        .write_slice(&kernel_bytes, kernel.loaded.load_address)
        .map_err(|source| BootSourceLoadError::GuestMemoryWrite {
            payload: BootPayloadKind::Kernel,
            source,
        })?;

    if let Some((bytes, loaded)) = &initrd_bytes {
        memory
            .write_slice(bytes, loaded.address)
            .map_err(|source| BootSourceLoadError::GuestMemoryWrite {
                payload: BootPayloadKind::Initrd,
                source,
            })?;
    }

    Ok(LoadedBootSource {
        command_line,
        kernel: kernel.loaded,
        initrd: initrd_bytes.map(|(_, loaded)| loaded),
    })
}

fn validate_command_line(
    boot_args: Option<&str>,
) -> Result<KernelCommandLine, BootSourceLoadError> {
    let text = boot_args.unwrap_or(DEFAULT_KERNEL_COMMAND_LINE).trim();
    if text.as_bytes().contains(&0) {
        return Err(BootSourceLoadError::CommandLine(
            BootCommandLineError::ContainsNul,
        ));
    }

    let size_with_nul = text
        .len()
        .checked_add(1)
        .ok_or(BootSourceLoadError::CommandLine(
            BootCommandLineError::TooLarge {
                size_with_nul: usize::MAX,
                max_size: aarch64::CMDLINE_MAX_SIZE,
            },
        ))?;

    if size_with_nul > aarch64::CMDLINE_MAX_SIZE {
        return Err(BootSourceLoadError::CommandLine(
            BootCommandLineError::TooLarge {
                size_with_nul,
                max_size: aarch64::CMDLINE_MAX_SIZE,
            },
        ));
    }

    let mut bytes_with_nul = Vec::from(text.as_bytes());
    bytes_with_nul.push(0);

    Ok(KernelCommandLine {
        text: text.to_string(),
        bytes_with_nul,
    })
}

fn prepare_kernel_payload(
    path: &Path,
    layout: &GuestMemoryLayout,
    memory: &GuestMemory,
) -> Result<PreparedKernelPayload, BootSourceLoadError> {
    let (mut file, size) = open_payload_file(path, BootPayloadKind::Kernel)?;
    validate_kernel_range(
        layout,
        memory,
        aarch64::kernel_load_address(),
        size,
        BootPayloadKind::Kernel,
    )?;
    let header = read_arm64_image_header(&mut file, size)?;
    let base_address = aarch64::kernel_load_address();

    if !base_address
        .raw_value()
        .is_multiple_of(ARM64_BASE_ALIGNMENT)
    {
        return Err(BootSourceLoadError::KernelImage(
            KernelImageError::BaseAddressNotAligned {
                address: base_address,
                alignment: ARM64_BASE_ALIGNMENT,
            },
        ));
    }

    let text_offset = if header.image_size == 0 {
        ARM64_LEGACY_TEXT_OFFSET
    } else {
        header.text_offset
    };
    let load_address =
        base_address
            .checked_add(text_offset)
            .ok_or(BootSourceLoadError::KernelImage(
                KernelImageError::LoadAddressOverflow {
                    base_address,
                    text_offset,
                },
            ))?;

    validate_kernel_range(layout, memory, load_address, size, BootPayloadKind::Kernel)?;

    Ok(PreparedKernelPayload {
        file,
        loaded: LoadedKernel {
            base_address,
            load_address,
            entry_address: load_address,
            size,
        },
    })
}

fn prepare_initrd_payload(
    path: &Path,
    layout: &GuestMemoryLayout,
    memory: &GuestMemory,
) -> Result<PreparedInitrdPayload, BootSourceLoadError> {
    let (file, size) = open_payload_file(path, BootPayloadKind::Initrd)?;
    let address = aarch64::initrd_load_address(layout, size)
        .map_err(|source| BootSourceLoadError::InvalidLayout { source })?
        .ok_or(BootSourceLoadError::NoInitrdSpace { size })?;
    let range = payload_range(address, size, BootPayloadKind::Initrd)?;
    validate_memory_backed_range(memory, range, BootPayloadKind::Initrd)?;

    Ok(PreparedInitrdPayload {
        file,
        loaded: LoadedInitrd { address, size },
    })
}

fn open_payload_file(
    path: &Path,
    payload: BootPayloadKind,
) -> Result<(File, u64), BootSourceLoadError> {
    if path.as_os_str().is_empty() {
        return Err(BootSourceLoadError::EmptyPath { payload });
    }

    let file =
        File::open(path).map_err(|source| BootSourceLoadError::OpenFile { payload, source })?;
    let metadata = file
        .metadata()
        .map_err(|source| BootSourceLoadError::ReadMetadata { payload, source })?;

    if !metadata.file_type().is_file() {
        return Err(BootSourceLoadError::NonRegularFile { payload });
    }

    let size = metadata.len();
    if size == 0 {
        return Err(BootSourceLoadError::EmptyPayload { payload });
    }

    Ok((file, size))
}

fn read_payload_file(
    mut file: File,
    payload: BootPayloadKind,
    expected_size: u64,
) -> Result<Vec<u8>, BootSourceLoadError> {
    file.rewind()
        .map_err(|source| BootSourceLoadError::ReadFile { payload, source })?;

    let read_limit =
        expected_size
            .checked_add(1)
            .ok_or(BootSourceLoadError::PayloadTooLargeForHost {
                payload,
                size: expected_size,
            })?;
    let expected_size_usize = usize::try_from(expected_size).map_err(|_| {
        BootSourceLoadError::PayloadTooLargeForHost {
            payload,
            size: expected_size,
        }
    })?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(expected_size_usize)
        .map_err(
            |source| BootSourceLoadError::PayloadBufferAllocationFailed {
                payload,
                size: expected_size_usize,
                source,
            },
        )?;

    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|source| BootSourceLoadError::ReadFile { payload, source })?;

    let actual_size =
        u64::try_from(bytes.len()).map_err(|_| BootSourceLoadError::PayloadTooLargeForHost {
            payload,
            size: expected_size,
        })?;
    if actual_size != expected_size {
        return Err(BootSourceLoadError::PayloadSizeChanged {
            payload,
            expected_size,
            actual_size,
        });
    }

    Ok(bytes)
}

fn read_arm64_image_header(
    file: &mut File,
    size: u64,
) -> Result<Arm64ImageHeader, BootSourceLoadError> {
    if size < ARM64_IMAGE_HEADER_SIZE as u64 {
        return Err(BootSourceLoadError::KernelImage(
            KernelImageError::HeaderTooShort { size },
        ));
    }

    let mut bytes = [0; ARM64_IMAGE_HEADER_SIZE];
    file.read_exact(&mut bytes)
        .map_err(|source| BootSourceLoadError::ReadFile {
            payload: BootPayloadKind::Kernel,
            source,
        })?;

    parse_arm64_image_header(&bytes)
}

fn parse_arm64_image_header(bytes: &[u8]) -> Result<Arm64ImageHeader, BootSourceLoadError> {
    let available_size =
        u64::try_from(bytes.len()).map_err(|_| BootSourceLoadError::PayloadTooLargeForHost {
            payload: BootPayloadKind::Kernel,
            size: u64::MAX,
        })?;
    if bytes.len() < ARM64_IMAGE_HEADER_SIZE {
        return Err(BootSourceLoadError::KernelImage(
            KernelImageError::HeaderTooShort {
                size: available_size,
            },
        ));
    }

    let header = Arm64ImageHeader {
        text_offset: read_u64_le(bytes, ARM64_IMAGE_TEXT_OFFSET_OFFSET)?,
        image_size: read_u64_le(bytes, ARM64_IMAGE_SIZE_OFFSET)?,
        magic: read_u32_le(bytes, ARM64_IMAGE_MAGIC_OFFSET)?,
    };
    if header.magic != ARM64_IMAGE_MAGIC {
        return Err(BootSourceLoadError::KernelImage(
            KernelImageError::InvalidMagic {
                magic: header.magic,
            },
        ));
    }

    Ok(header)
}

fn read_u64_le(bytes: &[u8], offset: usize) -> Result<u64, BootSourceLoadError> {
    let chunk = fixed_chunk(bytes, offset, std::mem::size_of::<u64>())?;
    let array = <[u8; 8]>::try_from(chunk).map_err(|_| {
        BootSourceLoadError::KernelImage(KernelImageError::HeaderTooShort {
            size: bytes_len_for_error(bytes),
        })
    })?;

    Ok(u64::from_le_bytes(array))
}

fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, BootSourceLoadError> {
    let chunk = fixed_chunk(bytes, offset, std::mem::size_of::<u32>())?;
    let array = <[u8; 4]>::try_from(chunk).map_err(|_| {
        BootSourceLoadError::KernelImage(KernelImageError::HeaderTooShort {
            size: bytes_len_for_error(bytes),
        })
    })?;

    Ok(u32::from_le_bytes(array))
}

fn fixed_chunk(bytes: &[u8], offset: usize, size: usize) -> Result<&[u8], BootSourceLoadError> {
    let end = offset
        .checked_add(size)
        .ok_or(BootSourceLoadError::KernelImage(
            KernelImageError::HeaderTooShort {
                size: bytes_len_for_error(bytes),
            },
        ))?;

    bytes
        .get(offset..end)
        .ok_or(BootSourceLoadError::KernelImage(
            KernelImageError::HeaderTooShort {
                size: bytes_len_for_error(bytes),
            },
        ))
}

fn bytes_len_for_error(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}

fn validate_payloads_do_not_overlap(
    kernel: &LoadedKernel,
    initrd: &LoadedInitrd,
) -> Result<(), BootSourceLoadError> {
    let kernel_range = payload_range(kernel.load_address, kernel.size, BootPayloadKind::Kernel)?;
    let initrd_range = payload_range(initrd.address, initrd.size, BootPayloadKind::Initrd)?;

    if kernel_range.overlaps(initrd_range) {
        Err(BootSourceLoadError::PayloadsOverlap {
            first_payload: BootPayloadKind::Kernel,
            first_range: kernel_range,
            second_payload: BootPayloadKind::Initrd,
            second_range: initrd_range,
        })
    } else {
        Ok(())
    }
}

fn validate_kernel_range(
    layout: &GuestMemoryLayout,
    memory: &GuestMemory,
    load_address: GuestAddress,
    size: u64,
    payload: BootPayloadKind,
) -> Result<(), BootSourceLoadError> {
    let range = payload_range(load_address, size, payload)?;
    validate_memory_backed_range(memory, range, payload)?;

    let fdt_address = aarch64::fdt_address(layout)
        .map_err(|source| BootSourceLoadError::InvalidLayout { source })?;
    if range.end_exclusive() > fdt_address {
        return Err(BootSourceLoadError::PayloadOverlapsFdt {
            payload,
            end_exclusive: range.end_exclusive(),
            fdt_address,
        });
    }

    Ok(())
}

fn payload_range(
    address: GuestAddress,
    size: u64,
    payload: BootPayloadKind,
) -> Result<GuestMemoryRange, BootSourceLoadError> {
    GuestMemoryRange::new(address, size)
        .map_err(|source| BootSourceLoadError::PayloadRangeInvalid { payload, source })
}

fn validate_memory_backed_range(
    memory: &GuestMemory,
    range: GuestMemoryRange,
    payload: BootPayloadKind,
) -> Result<(), BootSourceLoadError> {
    if memory_contains_range(memory, range) {
        Ok(())
    } else {
        Err(BootSourceLoadError::PayloadDoesNotFit { payload, range })
    }
}

fn memory_contains_range(memory: &GuestMemory, range: GuestMemoryRange) -> bool {
    let mut current = range.start();
    for region in memory.regions() {
        let region_range = region.range();
        if region_range.end_exclusive() <= current {
            continue;
        }
        if !region_range.contains(current) {
            return false;
        }

        current = GuestAddress::new(
            region_range
                .end_exclusive()
                .raw_value()
                .min(range.end_exclusive().raw_value()),
        );
        if current == range.end_exclusive() {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        ARM64_IMAGE_MAGIC, ARM64_IMAGE_MAGIC_OFFSET, ARM64_IMAGE_SIZE_OFFSET,
        ARM64_IMAGE_TEXT_OFFSET_OFFSET, ARM64_LEGACY_TEXT_OFFSET, BootCommandLineError,
        BootPayloadKind, BootSource, BootSourceLoadError, DEFAULT_KERNEL_COMMAND_LINE,
        KernelImageError,
    };
    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, aarch64};

    const TEST_MEMORY_SIZE: u64 = 64 << 20;
    const TEST_KERNEL_TEXT_OFFSET: u64 = ARM64_LEGACY_TEXT_OFFSET;

    static NEXT_TEMP_PATH_ID: AtomicUsize = AtomicUsize::new(0);

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

    fn temp_path(name: &str) -> PathBuf {
        let id = NEXT_TEMP_PATH_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-boot-test-{}-{id}-{name}",
            std::process::id()
        ))
    }

    fn temp_file(name: &str, bytes: &[u8]) -> TempPath {
        let path = temp_path(name);
        fs::write(&path, bytes).expect("test file should be written");
        TempPath { path }
    }

    fn temp_sparse_file(name: &str, size: u64) -> TempPath {
        let path = temp_path(name);
        let file = File::create(&path).expect("sparse test file should be created");
        file.set_len(size)
            .expect("sparse test file size should be set");
        TempPath { path }
    }

    fn temp_sparse_arm64_image(name: &str, size: u64, text_offset: u64) -> TempPath {
        let path = temp_path(name);
        let mut file = File::create(&path).expect("sparse Image test file should be created");
        let header = arm64_image(text_offset, size, 64);
        file.write_all(&header)
            .expect("sparse Image header should be written");
        file.set_len(size)
            .expect("sparse Image test file size should be set");
        TempPath { path }
    }

    fn temp_dir(name: &str) -> TempPath {
        let path = temp_path(name);
        fs::create_dir(&path).expect("test directory should be created");
        TempPath { path }
    }

    fn missing_path(name: &str) -> PathBuf {
        temp_path(name)
    }

    fn boot_layout() -> GuestMemoryLayout {
        aarch64::dram_layout(TEST_MEMORY_SIZE).expect("test guest memory layout should be valid")
    }

    fn boot_memory(layout: &GuestMemoryLayout) -> GuestMemory {
        GuestMemory::allocate(layout).expect("test guest memory should allocate")
    }

    fn arm64_image(text_offset: u64, image_size: u64, payload_size: usize) -> Vec<u8> {
        let size = payload_size.max(64);
        let mut bytes = vec![0xaa; size];
        write_u64_le(&mut bytes, ARM64_IMAGE_TEXT_OFFSET_OFFSET, text_offset);
        write_u64_le(&mut bytes, ARM64_IMAGE_SIZE_OFFSET, image_size);
        write_u32_le(&mut bytes, ARM64_IMAGE_MAGIC_OFFSET, ARM64_IMAGE_MAGIC);
        bytes
    }

    fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) {
        let end = offset + std::mem::size_of::<u64>();
        bytes[offset..end].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) {
        let end = offset + std::mem::size_of::<u32>();
        bytes[offset..end].copy_from_slice(&value.to_le_bytes());
    }

    fn guest_address_add(address: GuestAddress, offset: u64) -> GuestAddress {
        address
            .checked_add(offset)
            .expect("test guest address addition should not overflow")
    }

    #[test]
    fn loads_kernel_without_initrd() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let source = BootSource::new(kernel_file.as_path());

        let loaded = source
            .load(&layout, &mut memory)
            .expect("boot source should load");

        let expected_load_address =
            guest_address_add(aarch64::kernel_load_address(), TEST_KERNEL_TEXT_OFFSET);
        assert_eq!(loaded.kernel.base_address, aarch64::kernel_load_address());
        assert_eq!(loaded.kernel.load_address, expected_load_address);
        assert_eq!(loaded.kernel.entry_address, expected_load_address);
        assert_eq!(loaded.kernel.size, kernel_bytes.len() as u64);
        assert_eq!(loaded.initrd, None);
        assert_eq!(loaded.command_line.as_str(), DEFAULT_KERNEL_COMMAND_LINE);

        let mut read_back = vec![0; kernel_bytes.len()];
        memory
            .read_slice(&mut read_back, expected_load_address)
            .expect("loaded kernel should be readable");
        assert_eq!(read_back, kernel_bytes);
    }

    #[test]
    fn loads_kernel_and_initrd() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let initrd_bytes = b"initrd bytes";
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let initrd_file = temp_file("initrd", initrd_bytes);
        let source = BootSource::new(kernel_file.as_path())
            .with_initrd_path(initrd_file.as_path())
            .with_boot_args("console=hvc0");

        let loaded = source
            .load(&layout, &mut memory)
            .expect("boot source should load with initrd");

        assert_eq!(loaded.command_line.as_str(), "console=hvc0");
        assert_eq!(loaded.command_line.as_bytes_with_nul(), b"console=hvc0\0");

        let initrd = loaded.initrd.expect("initrd should be loaded");
        assert_eq!(initrd.size, initrd_bytes.len() as u64);

        let mut read_back = vec![0; initrd_bytes.len()];
        memory
            .read_slice(&mut read_back, initrd.address)
            .expect("loaded initrd should be readable");
        assert_eq!(read_back, initrd_bytes);
    }

    #[test]
    fn accepts_empty_custom_command_line() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let source = BootSource::new(kernel_file.as_path()).with_boot_args("   ");

        let loaded = source
            .load(&layout, &mut memory)
            .expect("empty custom command line should load");

        assert_eq!(loaded.command_line.as_str(), "");
        assert_eq!(loaded.command_line.as_bytes_with_nul(), b"\0");
    }

    #[test]
    fn accepts_exact_command_line_limit() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let boot_args = "a".repeat(aarch64::CMDLINE_MAX_SIZE - 1);
        let source = BootSource::new(kernel_file.as_path()).with_boot_args(boot_args.clone());

        let loaded = source
            .load(&layout, &mut memory)
            .expect("exact command line limit should load");

        assert_eq!(loaded.command_line.as_str(), boot_args);
        assert_eq!(
            loaded.command_line.as_bytes_with_nul().len(),
            aarch64::CMDLINE_MAX_SIZE
        );
    }

    #[test]
    fn rejects_command_line_over_limit() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let boot_args = "a".repeat(aarch64::CMDLINE_MAX_SIZE);
        let source = BootSource::new(kernel_file.as_path()).with_boot_args(boot_args);

        let err = source
            .load(&layout, &mut memory)
            .expect_err("oversized command line should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::CommandLine(BootCommandLineError::TooLarge { .. })
        ));
    }

    #[test]
    fn rejects_command_line_with_nul() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let source = BootSource::new(kernel_file.as_path()).with_boot_args("console=hvc0\0debug");

        let err = source
            .load(&layout, &mut memory)
            .expect_err("NUL command line should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::CommandLine(BootCommandLineError::ContainsNul)
        ));
    }

    #[test]
    fn rejects_missing_kernel_without_path_leak() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let path = missing_path("secret-kernel");
        let source = BootSource::new(&path);

        let err = source
            .load(&layout, &mut memory)
            .expect_err("missing kernel should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::OpenFile {
                payload: BootPayloadKind::Kernel,
                ..
            }
        ));
        assert!(!err.to_string().contains(path.to_string_lossy().as_ref()));
    }

    #[test]
    fn rejects_empty_kernel_path() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let source = BootSource::new(PathBuf::new());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("empty kernel path should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::EmptyPath {
                payload: BootPayloadKind::Kernel
            }
        ));
    }

    #[test]
    fn rejects_non_regular_kernel_file() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_dir = temp_dir("kernel-dir");
        let source = BootSource::new(kernel_dir.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("directory kernel path should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::NonRegularFile {
                payload: BootPayloadKind::Kernel
            }
        ));
    }

    #[test]
    fn rejects_zero_sized_kernel() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_file = temp_file("empty-kernel", &[]);
        let source = BootSource::new(kernel_file.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("empty kernel should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::EmptyPayload {
                payload: BootPayloadKind::Kernel
            }
        ));
    }

    #[test]
    fn rejects_too_short_kernel_image() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_file = temp_file("short-kernel", &[0xaa; 63]);
        let source = BootSource::new(kernel_file.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("short kernel image should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::KernelImage(KernelImageError::HeaderTooShort { size: 63 })
        ));
    }

    #[test]
    fn rejects_invalid_kernel_magic() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let mut kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        write_u32_le(&mut kernel_bytes, ARM64_IMAGE_MAGIC_OFFSET, 0);
        let kernel_file = temp_file("bad-magic-kernel", &kernel_bytes);
        let source = BootSource::new(kernel_file.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("invalid kernel magic should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::KernelImage(KernelImageError::InvalidMagic { magic: 0 })
        ));
    }

    #[test]
    fn loads_legacy_arm64_image_with_default_text_offset() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(0, 0, 4096);
        let kernel_file = temp_file("legacy-kernel", &kernel_bytes);
        let source = BootSource::new(kernel_file.as_path());

        let loaded = source
            .load(&layout, &mut memory)
            .expect("legacy arm64 Image should load");

        assert_eq!(
            loaded.kernel.load_address,
            guest_address_add(aarch64::kernel_load_address(), ARM64_LEGACY_TEXT_OFFSET)
        );
    }

    #[test]
    fn rejects_kernel_that_would_overlap_fdt() {
        let layout =
            aarch64::dram_layout(4 << 20).expect("small test memory layout should be valid");
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(0, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let source = BootSource::new(kernel_file.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("kernel overlapping FDT should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::PayloadOverlapsFdt {
                payload: BootPayloadKind::Kernel,
                ..
            }
        ));
    }

    #[test]
    fn rejects_zero_sized_initrd() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let initrd_file = temp_file("empty-initrd", &[]);
        let source = BootSource::new(kernel_file.as_path()).with_initrd_path(initrd_file.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("empty initrd should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::EmptyPayload {
                payload: BootPayloadKind::Initrd
            }
        ));
    }

    #[test]
    fn rejects_non_regular_initrd_file() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let initrd_dir = temp_dir("initrd-dir");
        let source = BootSource::new(kernel_file.as_path()).with_initrd_path(initrd_dir.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("directory initrd path should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::NonRegularFile {
                payload: BootPayloadKind::Initrd
            }
        ));
    }

    #[test]
    fn rejects_initrd_that_cannot_fit() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let initrd_file = temp_sparse_file("huge-initrd", TEST_MEMORY_SIZE);
        let source = BootSource::new(kernel_file.as_path()).with_initrd_path(initrd_file.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("oversized initrd should fail");

        assert!(matches!(err, BootSourceLoadError::NoInitrdSpace { .. }));
    }

    #[test]
    fn rejects_kernel_and_initrd_overlap() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_file = temp_sparse_arm64_image("large-kernel", 60_u64 << 20, 0);
        let initrd_file = temp_file("initrd", b"initrd");
        let source = BootSource::new(kernel_file.as_path()).with_initrd_path(initrd_file.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("overlapping kernel and initrd should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::PayloadsOverlap {
                first_payload: BootPayloadKind::Kernel,
                second_payload: BootPayloadKind::Initrd,
                ..
            }
        ));
    }

    #[test]
    fn invalid_initrd_does_not_write_kernel() {
        let layout = boot_layout();
        let mut memory = boot_memory(&layout);
        let kernel_bytes = arm64_image(TEST_KERNEL_TEXT_OFFSET, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let missing_initrd = missing_path("missing-initrd");
        let source = BootSource::new(kernel_file.as_path()).with_initrd_path(&missing_initrd);

        let err = source
            .load(&layout, &mut memory)
            .expect_err("missing initrd should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::OpenFile {
                payload: BootPayloadKind::Initrd,
                ..
            }
        ));
        assert!(
            !err.to_string()
                .contains(missing_initrd.to_string_lossy().as_ref())
        );

        let expected_load_address =
            guest_address_add(aarch64::kernel_load_address(), TEST_KERNEL_TEXT_OFFSET);
        let mut read_back = vec![0; kernel_bytes.len()];
        memory
            .read_slice(&mut read_back, expected_load_address)
            .expect("kernel range should be readable");
        assert_eq!(read_back, vec![0; kernel_bytes.len()]);
    }

    #[test]
    fn rejects_memory_without_kernel_range() {
        let layout = boot_layout();
        let memory_layout =
            aarch64::dram_layout(aarch64::SYSTEM_MEM_SIZE).expect("tiny memory layout is valid");
        let mut memory = boot_memory(&memory_layout);
        let kernel_bytes = arm64_image(0, 4096, 4096);
        let kernel_file = temp_file("kernel", &kernel_bytes);
        let source = BootSource::new(kernel_file.as_path());

        let err = source
            .load(&layout, &mut memory)
            .expect_err("mismatched guest memory should fail");

        assert!(matches!(
            err,
            BootSourceLoadError::PayloadDoesNotFit {
                payload: BootPayloadKind::Kernel,
                ..
            }
        ));
    }
}
