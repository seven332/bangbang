//! Internal native-v1 device-profile state and deterministic encoding.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::block::{
    BlockFileBacking, BlockFileBackingIdentity, DriveBackendConfig, DriveCacheType, DriveConfig,
    DriveIoEngine, DriveRateLimiterConfig, DriveTokenBucketConfig, VIRTIO_BLOCK_ID_BYTES,
    VIRTIO_BLOCK_SECTOR_SHIFT, VirtioBlockConfigSpace, VirtioBlockDeviceId, VirtioBlockMmioHandler,
    VirtioBlockRateLimiterState, VirtioBlockRuntimeState, VirtioBlockTokenBucketState,
};
use crate::fdt::{ARM64_FDT_VMCLOCK_SIZE, ARM64_FDT_VMGENID_SIZE, Arm64FdtRegion};
use crate::interrupt::{DeviceInterruptStatus, GuestInterruptLine};
use crate::memory::{GuestAddress, GuestMemory, GuestMemoryRange};
use crate::mmio::{MmioRegion, MmioRegionId};
use crate::serial::{SERIAL_MMIO_DEVICE_WINDOW_SIZE, SerialConfig, SerialMmioState};
use crate::virtio_mmio::{
    VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VIRTIO_MMIO_VERSION_1_FEATURE, VirtioMmioDeviceRegisters,
    VirtioMmioQueueState, VirtioMmioTransportState,
};
use crate::vmclock::{VMCLOCK_ABI_SIZE, VmClockAbi};

pub const SNAPSHOT_V1_DEVICE_MAGIC: [u8; 8] = *b"BANGDEV\0";
pub const SNAPSHOT_V1_DEVICE_MAX_SIZE: usize = 16 * 1024;
pub const SNAPSHOT_V1_DEVICE_MAX_PATH_BYTES: usize = 4096;
pub const SNAPSHOT_V1_DEVICE_MAX_DRIVE_ID_BYTES: usize = 255;
pub const SNAPSHOT_V1_DEVICE_MAX_PARTUUID_BYTES: usize = 255;

const SNAPSHOT_V1_DEVICE_HEADER_SIZE: usize = 32;
const SNAPSHOT_V1_DEVICE_MAJOR: u16 = 1;
const SNAPSHOT_V1_DEVICE_MINOR: u16 = 1;
const SNAPSHOT_V1_DEVICE_PATCH: u16 = 0;
const SNAPSHOT_V1_DEVICE_LEGACY_MINOR: u16 = 0;
const SNAPSHOT_V1_DEVICE_PROFILE: u16 = 1;
const SNAPSHOT_V1_DEVICE_FLAGS: u32 = 0;
const SNAPSHOT_V1_DEVICE_BLOCK_COUNT: u16 = 1;
const SNAPSHOT_V1_DEVICE_FRESH_SERIAL_POLICY: u16 = 1;
const SNAPSHOT_V1_DEVICE_REGENERATE_VMGENID_POLICY: u16 = 1;
const SNAPSHOT_V1_DEVICE_SHARED_VMCLOCK_POLICY: u16 = 1;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV1MmioDeviceMetadata {
    region: MmioRegion,
    interrupt_line: GuestInterruptLine,
}

impl SnapshotV1MmioDeviceMetadata {
    pub const fn new(region: MmioRegion, interrupt_line: GuestInterruptLine) -> Self {
        Self {
            region,
            interrupt_line,
        }
    }

    pub const fn region(self) -> MmioRegion {
        self.region
    }

    pub const fn interrupt_line(self) -> GuestInterruptLine {
        self.interrupt_line
    }
}

impl fmt::Debug for SnapshotV1MmioDeviceMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotV1MmioDeviceMetadata")
            .field("metadata", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SnapshotV1PlatformDeviceMetadata {
    range: GuestMemoryRange,
    fdt_region: Arm64FdtRegion,
    interrupt_line: GuestInterruptLine,
}

impl SnapshotV1PlatformDeviceMetadata {
    pub const fn new(
        range: GuestMemoryRange,
        fdt_region: Arm64FdtRegion,
        interrupt_line: GuestInterruptLine,
    ) -> Self {
        Self {
            range,
            fdt_region,
            interrupt_line,
        }
    }

    pub const fn range(self) -> GuestMemoryRange {
        self.range
    }

    pub const fn fdt_region(self) -> Arm64FdtRegion {
        self.fdt_region
    }

    pub const fn interrupt_line(self) -> GuestInterruptLine {
        self.interrupt_line
    }
}

impl fmt::Debug for SnapshotV1PlatformDeviceMetadata {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotV1PlatformDeviceMetadata")
            .field("metadata", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV1RootBlockState {
    drive_id: String,
    path: PathBuf,
    partuuid: Option<String>,
    cache_type: DriveCacheType,
    rate_limiter_config: Option<DriveRateLimiterConfig>,
    device_id: VirtioBlockDeviceId,
    capacity_sectors: u64,
    backing_identity: BlockFileBackingIdentity,
    mmio: SnapshotV1MmioDeviceMetadata,
    runtime: VirtioBlockRuntimeState,
}

pub(crate) struct SnapshotV1RootBlockStateParts {
    pub drive_id: String,
    pub path: PathBuf,
    pub partuuid: Option<String>,
    pub cache_type: DriveCacheType,
    pub rate_limiter_config: Option<DriveRateLimiterConfig>,
    pub device_id: VirtioBlockDeviceId,
    pub capacity_sectors: u64,
    pub backing_identity: BlockFileBackingIdentity,
    pub mmio: SnapshotV1MmioDeviceMetadata,
    pub runtime: VirtioBlockRuntimeState,
}

impl SnapshotV1RootBlockState {
    pub(crate) fn from_parts(parts: SnapshotV1RootBlockStateParts) -> Self {
        Self {
            drive_id: parts.drive_id,
            path: parts.path,
            partuuid: parts.partuuid,
            cache_type: parts.cache_type,
            rate_limiter_config: parts.rate_limiter_config,
            device_id: parts.device_id,
            capacity_sectors: parts.capacity_sectors,
            backing_identity: parts.backing_identity,
            mmio: parts.mmio,
            runtime: parts.runtime,
        }
    }

    pub fn drive_id(&self) -> &str {
        &self.drive_id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn partuuid(&self) -> Option<&str> {
        self.partuuid.as_deref()
    }

    pub const fn cache_type(&self) -> DriveCacheType {
        self.cache_type
    }

    pub const fn rate_limiter_config(&self) -> Option<DriveRateLimiterConfig> {
        self.rate_limiter_config
    }

    pub const fn device_id(&self) -> VirtioBlockDeviceId {
        self.device_id
    }

    pub const fn capacity_sectors(&self) -> u64 {
        self.capacity_sectors
    }

    pub const fn backing_identity(&self) -> BlockFileBackingIdentity {
        self.backing_identity
    }

    pub const fn mmio(&self) -> SnapshotV1MmioDeviceMetadata {
        self.mmio
    }

    pub const fn runtime(&self) -> &VirtioBlockRuntimeState {
        &self.runtime
    }
}

impl fmt::Debug for SnapshotV1RootBlockState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotV1RootBlockState")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV1BlockRetryState {
    None,
    Immediate,
    After { remaining_nanos: u64 },
}

impl SnapshotV1BlockRetryState {
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

impl fmt::Debug for SnapshotV1BlockRetryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let policy = match self {
            Self::None => "none",
            Self::Immediate => "immediate",
            Self::After { .. } => "delayed",
        };
        f.debug_tuple("SnapshotV1BlockRetryState")
            .field(&policy)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct SnapshotV1DeviceState {
    root_block: SnapshotV1RootBlockState,
    block_retry: SnapshotV1BlockRetryState,
    serial_mmio: SnapshotV1MmioDeviceMetadata,
    serial_state: SerialMmioState,
    vmgenid: SnapshotV1PlatformDeviceMetadata,
    vmclock: SnapshotV1PlatformDeviceMetadata,
    vmclock_abi: Option<VmClockAbi>,
}

impl SnapshotV1DeviceState {
    pub fn new(
        root_block: SnapshotV1RootBlockState,
        block_retry: SnapshotV1BlockRetryState,
        serial_mmio: SnapshotV1MmioDeviceMetadata,
        serial_state: SerialMmioState,
        vmgenid: SnapshotV1PlatformDeviceMetadata,
        vmclock: SnapshotV1PlatformDeviceMetadata,
        vmclock_abi: Option<VmClockAbi>,
    ) -> Self {
        Self {
            root_block,
            block_retry,
            serial_mmio,
            serial_state,
            vmgenid,
            vmclock,
            vmclock_abi,
        }
    }

    pub const fn root_block(&self) -> &SnapshotV1RootBlockState {
        &self.root_block
    }

    pub const fn block_retry(&self) -> SnapshotV1BlockRetryState {
        self.block_retry
    }

    pub const fn serial_mmio(&self) -> SnapshotV1MmioDeviceMetadata {
        self.serial_mmio
    }

    pub const fn serial_state(&self) -> SerialMmioState {
        self.serial_state
    }

    pub const fn vmgenid(&self) -> SnapshotV1PlatformDeviceMetadata {
        self.vmgenid
    }

    pub const fn vmclock(&self) -> SnapshotV1PlatformDeviceMetadata {
        self.vmclock
    }

    pub const fn vmclock_abi(&self) -> Option<VmClockAbi> {
        self.vmclock_abi
    }
}

impl fmt::Debug for SnapshotV1DeviceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotV1DeviceState")
            .field("profile", &"native-v1")
            .field("state", &"<redacted>")
            .finish()
    }
}

pub struct SnapshotV1DeviceCaptureInput<'a> {
    pub drive_config: &'a DriveConfig,
    pub block_mmio: SnapshotV1MmioDeviceMetadata,
    pub block_handler: &'a VirtioBlockMmioHandler,
    pub block_retry: SnapshotV1BlockRetryState,
    pub serial_config: &'a SerialConfig,
    pub serial_mmio: SnapshotV1MmioDeviceMetadata,
    pub serial_state: SerialMmioState,
    pub vmgenid: SnapshotV1PlatformDeviceMetadata,
    pub vmclock: SnapshotV1PlatformDeviceMetadata,
    pub memory: &'a GuestMemory,
    pub now: Instant,
}

impl fmt::Debug for SnapshotV1DeviceCaptureInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotV1DeviceCaptureInput")
            .field("state", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV1DeviceCaptureError {
    UnsupportedDriveProfile,
    UnsupportedSerialProfile,
    InvalidTextField,
    BlockBacking,
    BlockBackingMismatch,
    BlockConfigurationMismatch,
    BlockRuntime,
    InvalidRetry,
    InvalidBlockMetadata,
    InvalidSerialMetadata,
    InvalidVmGenIdMetadata,
    InvalidVmClockMetadata,
    ReadVmClock,
    InvalidVmClockAbi,
}

impl fmt::Display for SnapshotV1DeviceCaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedDriveProfile => {
                f.write_str("snapshot device capture requires one read-only root block profile")
            }
            Self::UnsupportedSerialProfile => {
                f.write_str("snapshot device capture requires the default serial profile")
            }
            Self::InvalidTextField => {
                f.write_str("snapshot device capture contains an invalid bounded text field")
            }
            Self::BlockBacking => {
                f.write_str("snapshot device capture could not identify the block backing")
            }
            Self::BlockBackingMismatch => {
                f.write_str("snapshot device capture block backing identity does not match")
            }
            Self::BlockConfigurationMismatch => {
                f.write_str("snapshot device capture block configuration does not match runtime")
            }
            Self::BlockRuntime => f.write_str("snapshot device capture block runtime is invalid"),
            Self::InvalidRetry => {
                f.write_str("snapshot device capture block retry state is invalid")
            }
            Self::InvalidBlockMetadata => {
                f.write_str("snapshot device capture block metadata is invalid")
            }
            Self::InvalidSerialMetadata => {
                f.write_str("snapshot device capture serial metadata is invalid")
            }
            Self::InvalidVmGenIdMetadata => {
                f.write_str("snapshot device capture VMGenID metadata is invalid")
            }
            Self::InvalidVmClockMetadata => {
                f.write_str("snapshot device capture VMClock metadata is invalid")
            }
            Self::ReadVmClock => {
                f.write_str("snapshot device capture could not read the VMClock ABI")
            }
            Self::InvalidVmClockAbi => {
                f.write_str("snapshot device capture VMClock ABI is invalid")
            }
        }
    }
}

impl std::error::Error for SnapshotV1DeviceCaptureError {}

pub fn capture_snapshot_v1_device_state(
    input: SnapshotV1DeviceCaptureInput<'_>,
) -> Result<SnapshotV1DeviceState, SnapshotV1DeviceCaptureError> {
    let config = input.drive_config;
    let DriveBackendConfig::File {
        path_on_host,
        is_read_only: true,
        io_engine: DriveIoEngine::Sync,
        ..
    } = config.backend()
    else {
        return Err(SnapshotV1DeviceCaptureError::UnsupportedDriveProfile);
    };
    if !config.is_root_device() {
        return Err(SnapshotV1DeviceCaptureError::UnsupportedDriveProfile);
    }
    validate_capture_text(config)?;
    if input.serial_config != &SerialConfig::default() {
        return Err(SnapshotV1DeviceCaptureError::UnsupportedSerialProfile);
    }
    validate_mmio_metadata(input.block_mmio, VIRTIO_MMIO_DEVICE_WINDOW_SIZE)
        .map_err(|_| SnapshotV1DeviceCaptureError::InvalidBlockMetadata)?;
    validate_mmio_metadata(input.serial_mmio, SERIAL_MMIO_DEVICE_WINDOW_SIZE)
        .map_err(|_| SnapshotV1DeviceCaptureError::InvalidSerialMetadata)?;
    validate_platform_metadata(input.vmgenid, ARM64_FDT_VMGENID_SIZE, input.memory)
        .map_err(|_| SnapshotV1DeviceCaptureError::InvalidVmGenIdMetadata)?;
    validate_platform_metadata(input.vmclock, ARM64_FDT_VMCLOCK_SIZE, input.memory)
        .map_err(|_| SnapshotV1DeviceCaptureError::InvalidVmClockMetadata)?;
    let mut vmclock_bytes = [0; VMCLOCK_ABI_SIZE];
    input
        .memory
        .read_slice(&mut vmclock_bytes, input.vmclock.range().start())
        .map_err(|_| SnapshotV1DeviceCaptureError::ReadVmClock)?;
    let vmclock_abi = VmClockAbi::from_bytes(vmclock_bytes)
        .map_err(|_| SnapshotV1DeviceCaptureError::InvalidVmClockAbi)?;

    if matches!(
        input.block_retry,
        SnapshotV1BlockRetryState::After { remaining_nanos: 0 }
    ) {
        return Err(SnapshotV1DeviceCaptureError::InvalidRetry);
    }

    let live_device = input.block_handler.activation_handler();
    let live_backing = live_device
        .backing()
        .ok_or(SnapshotV1DeviceCaptureError::UnsupportedDriveProfile)?;
    if !live_backing.is_read_only() || !live_backing.kind().is_regular_file() {
        return Err(SnapshotV1DeviceCaptureError::UnsupportedDriveProfile);
    }
    let live_identity = live_backing
        .snapshot_identity()
        .map_err(|_| SnapshotV1DeviceCaptureError::BlockBacking)?;
    if !live_backing.uses_supplied_file() {
        let (path_backing, path_identity) = BlockFileBacking::open_snapshot_read_only(path_on_host)
            .map_err(|_| SnapshotV1DeviceCaptureError::BlockBacking)?;
        if path_identity != live_identity || path_backing.len() != live_backing.len() {
            return Err(SnapshotV1DeviceCaptureError::BlockBackingMismatch);
        }
    }

    let expected_config_space =
        VirtioBlockConfigSpace::from_backing(live_backing, config.cache_type());
    if input.block_handler.device_config_handler() != &expected_config_space
        || !live_backing
            .snapshot_device_id_is_compatible(config.drive_id(), live_device.device_id())
    {
        return Err(SnapshotV1DeviceCaptureError::BlockConfigurationMismatch);
    }

    let runtime = input
        .block_handler
        .snapshot_block_runtime_state_at(config.rate_limiter(), input.now)
        .map_err(|_| SnapshotV1DeviceCaptureError::BlockRuntime)?;
    runtime
        .validate_guest_memory(input.memory, input.block_retry.has_retry())
        .map_err(|_| SnapshotV1DeviceCaptureError::BlockRuntime)?;

    let root_block = SnapshotV1RootBlockState::from_parts(SnapshotV1RootBlockStateParts {
        drive_id: config.drive_id().to_string(),
        path: path_on_host.to_path_buf(),
        partuuid: config.partuuid().map(ToString::to_string),
        cache_type: config.cache_type(),
        rate_limiter_config: config.rate_limiter(),
        device_id: live_device.device_id(),
        capacity_sectors: live_backing.len() >> VIRTIO_BLOCK_SECTOR_SHIFT,
        backing_identity: live_identity,
        mmio: input.block_mmio,
        runtime,
    });
    Ok(SnapshotV1DeviceState::new(
        root_block,
        input.block_retry,
        input.serial_mmio,
        input.serial_state,
        input.vmgenid,
        input.vmclock,
        Some(vmclock_abi),
    ))
}

fn validate_capture_text(config: &DriveConfig) -> Result<(), SnapshotV1DeviceCaptureError> {
    let drive_id = config.drive_id().as_bytes();
    let path = config
        .path_on_host()
        .ok_or(SnapshotV1DeviceCaptureError::UnsupportedDriveProfile)?
        .to_str()
        .ok_or(SnapshotV1DeviceCaptureError::InvalidTextField)?
        .as_bytes();
    if drive_id.is_empty()
        || drive_id.len() > SNAPSHOT_V1_DEVICE_MAX_DRIVE_ID_BYTES
        || path.is_empty()
        || path.len() > SNAPSHOT_V1_DEVICE_MAX_PATH_BYTES
        || config.partuuid().is_some_and(|partuuid| {
            partuuid.is_empty() || partuuid.len() > SNAPSHOT_V1_DEVICE_MAX_PARTUUID_BYTES
        })
    {
        Err(SnapshotV1DeviceCaptureError::InvalidTextField)
    } else {
        Ok(())
    }
}

pub(crate) fn validate_mmio_metadata(
    metadata: SnapshotV1MmioDeviceMetadata,
    expected_size: u64,
) -> Result<(), ()> {
    if metadata.region().range().size() == expected_size
        && metadata.interrupt_line().raw_value() >= 32
    {
        Ok(())
    } else {
        Err(())
    }
}

pub(crate) fn validate_platform_metadata(
    metadata: SnapshotV1PlatformDeviceMetadata,
    expected_size: u64,
    memory: &GuestMemory,
) -> Result<(), ()> {
    let range = metadata.range();
    let fdt = metadata.fdt_region();
    if range.size() != expected_size
        || fdt.base != range.start().raw_value()
        || fdt.size != range.size()
        || metadata.interrupt_line().raw_value() < 32
        || memory.validate_mapped_range(range).is_err()
    {
        Err(())
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV1DeviceEncodeError {
    Allocation,
    NonUtf8Path,
    EmptyDriveId,
    DriveIdTooLong,
    EmptyPath,
    PathTooLong,
    EmptyPartuuid,
    PartuuidTooLong,
    InvalidRetry,
    InvalidLimiterShape,
    InvalidState,
    TooLarge,
}

impl fmt::Display for SnapshotV1DeviceEncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Allocation => f.write_str("failed to allocate native-v1 device state"),
            Self::NonUtf8Path => f.write_str("native-v1 device path is not UTF-8"),
            Self::EmptyDriveId => f.write_str("native-v1 drive ID is empty"),
            Self::DriveIdTooLong => f.write_str("native-v1 drive ID exceeds 255 bytes"),
            Self::EmptyPath => f.write_str("native-v1 device path is empty"),
            Self::PathTooLong => f.write_str("native-v1 device path exceeds 4096 bytes"),
            Self::EmptyPartuuid => f.write_str("native-v1 partuuid is empty"),
            Self::PartuuidTooLong => f.write_str("native-v1 partuuid exceeds 255 bytes"),
            Self::InvalidRetry => f.write_str("native-v1 block retry state is invalid"),
            Self::InvalidLimiterShape => {
                f.write_str("native-v1 block limiter state is inconsistent")
            }
            Self::InvalidState => f.write_str("native-v1 device state is inconsistent"),
            Self::TooLarge => f.write_str("native-v1 device state exceeds 16 KiB"),
        }
    }
}

impl std::error::Error for SnapshotV1DeviceEncodeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotV1DeviceDecodeError {
    TooSmall,
    TooLarge,
    InvalidMagic,
    UnsupportedVersion,
    UnsupportedProfile,
    UnsupportedFlags,
    NonzeroReserved,
    LengthMismatch,
    Truncated,
    TrailingData,
    InvalidPolicy,
    InvalidBoolean,
    InvalidStringLength,
    InvalidUtf8,
    Allocation,
    InvalidEnum,
    InvalidMetadata,
    InvalidInterrupt,
    InvalidBackingIdentity,
    InvalidTransport,
    InvalidLimiter,
    InvalidRetry,
    InvalidVmClockAbi,
}

impl fmt::Display for SnapshotV1DeviceDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall => f.write_str("native-v1 device state is smaller than its header"),
            Self::TooLarge => f.write_str("native-v1 device state exceeds 16 KiB"),
            Self::InvalidMagic => f.write_str("native-v1 device magic is invalid"),
            Self::UnsupportedVersion => f.write_str("native-v1 device version is unsupported"),
            Self::UnsupportedProfile => f.write_str("native-v1 device profile is unsupported"),
            Self::UnsupportedFlags => f.write_str("native-v1 device flags are unsupported"),
            Self::NonzeroReserved => f.write_str("native-v1 device reserved field is nonzero"),
            Self::LengthMismatch => f.write_str("native-v1 device length does not match header"),
            Self::Truncated => f.write_str("native-v1 device state is truncated"),
            Self::TrailingData => f.write_str("native-v1 device state has trailing data"),
            Self::InvalidPolicy => f.write_str("native-v1 device policy is invalid"),
            Self::InvalidBoolean => f.write_str("native-v1 device boolean tag is invalid"),
            Self::InvalidStringLength => f.write_str("native-v1 device string length is invalid"),
            Self::InvalidUtf8 => f.write_str("native-v1 device string is not UTF-8"),
            Self::Allocation => f.write_str("failed to allocate decoded native-v1 device state"),
            Self::InvalidEnum => f.write_str("native-v1 device enum tag is invalid"),
            Self::InvalidMetadata => f.write_str("native-v1 device metadata is invalid"),
            Self::InvalidInterrupt => f.write_str("native-v1 device interrupt is invalid"),
            Self::InvalidBackingIdentity => {
                f.write_str("native-v1 block backing identity is invalid")
            }
            Self::InvalidTransport => f.write_str("native-v1 virtio transport is invalid"),
            Self::InvalidLimiter => f.write_str("native-v1 block limiter is invalid"),
            Self::InvalidRetry => f.write_str("native-v1 block retry is invalid"),
            Self::InvalidVmClockAbi => f.write_str("native-v1 VMClock ABI is invalid"),
        }
    }
}

impl std::error::Error for SnapshotV1DeviceDecodeError {}

pub fn encode_snapshot_v1_device_state(
    state: &SnapshotV1DeviceState,
) -> Result<Vec<u8>, SnapshotV1DeviceEncodeError> {
    let root = state.root_block();
    let drive_id = root.drive_id().as_bytes();
    validate_encode_string(
        drive_id,
        SNAPSHOT_V1_DEVICE_MAX_DRIVE_ID_BYTES,
        SnapshotV1DeviceEncodeError::EmptyDriveId,
        SnapshotV1DeviceEncodeError::DriveIdTooLong,
    )?;
    let path = root
        .path()
        .to_str()
        .ok_or(SnapshotV1DeviceEncodeError::NonUtf8Path)?
        .as_bytes();
    validate_encode_string(
        path,
        SNAPSHOT_V1_DEVICE_MAX_PATH_BYTES,
        SnapshotV1DeviceEncodeError::EmptyPath,
        SnapshotV1DeviceEncodeError::PathTooLong,
    )?;
    if let Some(partuuid) = root.partuuid() {
        validate_encode_string(
            partuuid.as_bytes(),
            SNAPSHOT_V1_DEVICE_MAX_PARTUUID_BYTES,
            SnapshotV1DeviceEncodeError::EmptyPartuuid,
            SnapshotV1DeviceEncodeError::PartuuidTooLong,
        )?;
    }
    if !root.backing_identity().kind().is_regular_file() {
        return Err(SnapshotV1DeviceEncodeError::InvalidState);
    }

    validate_encode_transport(root.runtime())?;
    validate_encode_limiter(root.rate_limiter_config(), root.runtime().rate_limiter())?;
    if state.block_retry().has_retry() && root.runtime().active_queue().is_none() {
        return Err(SnapshotV1DeviceEncodeError::InvalidRetry);
    }
    if state.block_retry().has_retry() && root.runtime().rate_limiter().is_empty() {
        return Err(SnapshotV1DeviceEncodeError::InvalidRetry);
    }
    if matches!(
        state.block_retry(),
        SnapshotV1BlockRetryState::After { remaining_nanos: 0 }
    ) {
        return Err(SnapshotV1DeviceEncodeError::InvalidRetry);
    }

    let mut output = Vec::new();
    output
        .try_reserve_exact(SNAPSHOT_V1_DEVICE_MAX_SIZE)
        .map_err(|_| SnapshotV1DeviceEncodeError::Allocation)?;
    output.extend_from_slice(&SNAPSHOT_V1_DEVICE_MAGIC);
    write_u16(&mut output, SNAPSHOT_V1_DEVICE_MAJOR);
    write_u16(
        &mut output,
        if state.vmclock_abi().is_some() {
            SNAPSHOT_V1_DEVICE_MINOR
        } else {
            SNAPSHOT_V1_DEVICE_LEGACY_MINOR
        },
    );
    write_u16(&mut output, SNAPSHOT_V1_DEVICE_PATCH);
    write_u16(&mut output, SNAPSHOT_V1_DEVICE_PROFILE);
    write_u32(&mut output, SNAPSHOT_V1_DEVICE_FLAGS);
    write_u32(&mut output, 0);
    write_u64(&mut output, 0);

    write_u16(&mut output, SNAPSHOT_V1_DEVICE_BLOCK_COUNT);
    write_u16(&mut output, SNAPSHOT_V1_DEVICE_FRESH_SERIAL_POLICY);
    write_u16(&mut output, SNAPSHOT_V1_DEVICE_REGENERATE_VMGENID_POLICY);
    write_u16(&mut output, SNAPSHOT_V1_DEVICE_SHARED_VMCLOCK_POLICY);
    write_u64(&mut output, 0);

    write_bounded_string(&mut output, drive_id);
    write_bounded_string(&mut output, path);
    match root.partuuid() {
        Some(partuuid) => {
            write_u8(&mut output, 1);
            write_u8(&mut output, 0);
            write_bounded_string(&mut output, partuuid.as_bytes());
        }
        None => {
            write_u8(&mut output, 0);
            write_u8(&mut output, 0);
            write_u16(&mut output, 0);
        }
    }
    write_u8(
        &mut output,
        match root.cache_type() {
            DriveCacheType::Unsafe => 0,
            DriveCacheType::Writeback => 1,
        },
    );
    write_u8(&mut output, 0);
    write_u8(&mut output, 1);
    write_u8(&mut output, 1);
    output.extend_from_slice(root.device_id().as_bytes());
    write_u64(&mut output, root.capacity_sectors());
    encode_backing_identity(&mut output, root.backing_identity());
    encode_mmio_metadata(&mut output, root.mmio());
    encode_transport(&mut output, root.runtime())?;
    encode_limiter(
        &mut output,
        root.rate_limiter_config(),
        root.runtime().rate_limiter(),
    );
    encode_retry(&mut output, state.block_retry());
    encode_mmio_metadata(&mut output, state.serial_mmio());
    encode_serial_state(&mut output, state.serial_state());
    encode_platform_metadata(&mut output, state.vmgenid());
    encode_platform_metadata(&mut output, state.vmclock());
    if let Some(vmclock_abi) = state.vmclock_abi() {
        output.extend_from_slice(&vmclock_abi.to_bytes());
    }

    if output.len() > SNAPSHOT_V1_DEVICE_MAX_SIZE {
        return Err(SnapshotV1DeviceEncodeError::TooLarge);
    }
    let body_len = output
        .len()
        .checked_sub(SNAPSHOT_V1_DEVICE_HEADER_SIZE)
        .and_then(|len| u32::try_from(len).ok())
        .ok_or(SnapshotV1DeviceEncodeError::TooLarge)?;
    output
        .get_mut(20..24)
        .ok_or(SnapshotV1DeviceEncodeError::InvalidState)?
        .copy_from_slice(&body_len.to_le_bytes());
    Ok(output)
}

fn validate_encode_string(
    value: &[u8],
    max: usize,
    empty: SnapshotV1DeviceEncodeError,
    too_long: SnapshotV1DeviceEncodeError,
) -> Result<(), SnapshotV1DeviceEncodeError> {
    if value.is_empty() {
        Err(empty)
    } else if value.len() > max {
        Err(too_long)
    } else {
        Ok(())
    }
}

fn validate_encode_transport(
    state: &VirtioBlockRuntimeState,
) -> Result<(), SnapshotV1DeviceEncodeError> {
    let transport = state.transport();
    let queue = transport.queues().first();
    let notification = transport.pending_notifications().first();
    if transport.queue_select() != 0
        || transport.queues().len() != 1
        || transport.pending_notifications().len() != 1
        || queue.map(|queue| queue.max_size()) != Some(crate::block::VIRTIO_BLOCK_QUEUE_SIZE)
        || notification.copied() != Some(false)
    {
        return Err(SnapshotV1DeviceEncodeError::InvalidState);
    }
    if transport.is_device_activated() != state.active_queue().is_some() {
        return Err(SnapshotV1DeviceEncodeError::InvalidState);
    }
    Ok(())
}

fn validate_encode_limiter(
    config: Option<DriveRateLimiterConfig>,
    state: VirtioBlockRateLimiterState,
) -> Result<(), SnapshotV1DeviceEncodeError> {
    if config.is_none() && !state.is_empty() {
        return Err(SnapshotV1DeviceEncodeError::InvalidLimiterShape);
    }
    validate_encode_bucket(
        config.and_then(DriveRateLimiterConfig::bandwidth),
        state.bandwidth(),
    )?;
    validate_encode_bucket(config.and_then(DriveRateLimiterConfig::ops), state.ops())
}

fn validate_encode_bucket(
    config: Option<DriveTokenBucketConfig>,
    state: Option<VirtioBlockTokenBucketState>,
) -> Result<(), SnapshotV1DeviceEncodeError> {
    match (config, state) {
        (Some(config), Some(state))
            if block_bucket_is_enabled(config)
                && state.config() == config
                && state.budget() <= config.size()
                && state.remaining_burst() <= config.one_time_burst().unwrap_or(0) =>
        {
            Ok(())
        }
        (Some(config), None) if !block_bucket_is_enabled(config) => Ok(()),
        (None, None) => Ok(()),
        _ => Err(SnapshotV1DeviceEncodeError::InvalidLimiterShape),
    }
}

fn block_bucket_is_enabled(config: DriveTokenBucketConfig) -> bool {
    config.size() != 0
        && config
            .refill_time()
            .checked_mul(1_000_000)
            .is_some_and(|nanos| nanos != 0)
}

fn encode_backing_identity(output: &mut Vec<u8>, identity: BlockFileBackingIdentity) {
    write_u64(output, identity.device());
    write_u64(output, identity.inode());
    write_u64(output, identity.len());
    write_u32(output, identity.mode());
    write_u32(output, 0);
    write_i64(output, identity.modified_seconds());
    write_u32(output, identity.modified_nanos());
    write_u32(output, 0);
    write_i64(output, identity.changed_seconds());
    write_u32(output, identity.changed_nanos());
    write_u32(output, 0);
}

fn encode_mmio_metadata(output: &mut Vec<u8>, metadata: SnapshotV1MmioDeviceMetadata) {
    let region = metadata.region();
    write_u64(output, region.id().raw_value());
    write_u64(output, region.range().start().raw_value());
    write_u64(output, region.range().size());
    write_u32(output, metadata.interrupt_line().raw_value());
    write_u32(output, 0);
}

fn encode_transport(
    output: &mut Vec<u8>,
    runtime: &VirtioBlockRuntimeState,
) -> Result<(), SnapshotV1DeviceEncodeError> {
    let transport = runtime.transport();
    let queue = transport
        .queues()
        .first()
        .copied()
        .ok_or(SnapshotV1DeviceEncodeError::InvalidState)?;
    let pending_notification = transport
        .pending_notifications()
        .first()
        .copied()
        .ok_or(SnapshotV1DeviceEncodeError::InvalidState)?;
    let device = transport.device_registers();
    write_u32(output, device.device_id());
    write_u32(output, device.vendor_id());
    write_u64(output, device.device_features());
    write_u32(output, device.config_generation());
    write_u32(output, device.device_features_select());
    write_u32(output, device.driver_features_select());
    write_u32(output, device.status());
    write_u64(output, device.driver_features());
    write_u32(output, transport.queue_select());
    write_bool(output, transport.is_device_activated());
    write_u8(output, 1);
    write_u8(output, 1);
    write_u8(output, 0);
    write_u32(output, transport.interrupt_status().bits());
    write_u32(output, 0);
    write_bool(output, pending_notification);
    output.extend_from_slice(&[0; 7]);

    write_u16(output, queue.max_size());
    write_u16(output, queue.size());
    write_bool(output, queue.ready());
    write_bool(output, runtime.active_queue().is_some());
    write_u16(output, 0);
    write_u64(output, queue.descriptor_table().raw_value());
    write_u64(output, queue.driver_ring().raw_value());
    write_u64(output, queue.device_ring().raw_value());
    let (next_available, next_used) = runtime
        .active_queue()
        .map(|state| (state.next_available(), state.next_used()))
        .unwrap_or((0, 0));
    write_u16(output, next_available);
    write_u16(output, next_used);
    write_u32(output, 0);
    Ok(())
}

fn encode_limiter(
    output: &mut Vec<u8>,
    config: Option<DriveRateLimiterConfig>,
    state: VirtioBlockRateLimiterState,
) {
    write_bool(output, config.is_some());
    output.extend_from_slice(&[0; 7]);
    encode_bucket(
        output,
        config.and_then(DriveRateLimiterConfig::bandwidth),
        state.bandwidth(),
    );
    encode_bucket(
        output,
        config.and_then(DriveRateLimiterConfig::ops),
        state.ops(),
    );
}

fn encode_bucket(
    output: &mut Vec<u8>,
    config: Option<DriveTokenBucketConfig>,
    state: Option<VirtioBlockTokenBucketState>,
) {
    write_bool(output, config.is_some());
    write_bool(output, state.is_some());
    write_bool(
        output,
        config.is_some_and(|config| config.one_time_burst().is_some()),
    );
    write_u8(output, 0);
    write_u64(
        output,
        config.map(DriveTokenBucketConfig::size).unwrap_or(0),
    );
    write_u64(
        output,
        config
            .and_then(DriveTokenBucketConfig::one_time_burst)
            .unwrap_or(0),
    );
    write_u64(
        output,
        config.map(DriveTokenBucketConfig::refill_time).unwrap_or(0),
    );
    write_u64(
        output,
        state.map(VirtioBlockTokenBucketState::budget).unwrap_or(0),
    );
    write_u64(
        output,
        state
            .map(VirtioBlockTokenBucketState::remaining_burst)
            .unwrap_or(0),
    );
    write_u64(
        output,
        state
            .map(VirtioBlockTokenBucketState::age_nanos)
            .unwrap_or(0),
    );
}

fn encode_retry(output: &mut Vec<u8>, retry: SnapshotV1BlockRetryState) {
    let (tag, remaining_nanos) = match retry {
        SnapshotV1BlockRetryState::None => (0, 0),
        SnapshotV1BlockRetryState::Immediate => (1, 0),
        SnapshotV1BlockRetryState::After { remaining_nanos } => (2, remaining_nanos),
    };
    write_u8(output, tag);
    output.extend_from_slice(&[0; 7]);
    write_u64(output, remaining_nanos);
}

fn encode_serial_state(output: &mut Vec<u8>, state: SerialMmioState) {
    write_u8(output, state.interrupt_enable());
    write_u8(output, state.line_control());
    write_u8(output, state.modem_control());
    write_u8(output, state.scratch());
    write_u8(output, state.divisor_latch_low());
    write_u8(output, state.divisor_latch_high());
    write_u16(output, 0);
}

fn encode_platform_metadata(output: &mut Vec<u8>, metadata: SnapshotV1PlatformDeviceMetadata) {
    write_u64(output, metadata.range().start().raw_value());
    write_u64(output, metadata.range().size());
    write_u64(output, metadata.fdt_region().base);
    write_u64(output, metadata.fdt_region().size);
    write_u32(output, metadata.interrupt_line().raw_value());
    write_u32(output, 0);
}

fn write_bounded_string(output: &mut Vec<u8>, value: &[u8]) {
    write_u16(output, value.len() as u16);
    output.extend_from_slice(value);
}

fn write_bool(output: &mut Vec<u8>, value: bool) {
    write_u8(output, u8::from(value));
}

fn write_u8(output: &mut Vec<u8>, value: u8) {
    output.push(value);
}

fn write_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn write_i64(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&value.to_le_bytes());
}

pub fn decode_snapshot_v1_device_state(
    bytes: &[u8],
) -> Result<SnapshotV1DeviceState, SnapshotV1DeviceDecodeError> {
    if bytes.len() < SNAPSHOT_V1_DEVICE_HEADER_SIZE {
        return Err(SnapshotV1DeviceDecodeError::TooSmall);
    }
    if bytes.len() > SNAPSHOT_V1_DEVICE_MAX_SIZE {
        return Err(SnapshotV1DeviceDecodeError::TooLarge);
    }

    let mut reader = DeviceStateReader::new(bytes);
    if reader.read_array::<8>()? != SNAPSHOT_V1_DEVICE_MAGIC {
        return Err(SnapshotV1DeviceDecodeError::InvalidMagic);
    }
    let version = (reader.read_u16()?, reader.read_u16()?, reader.read_u16()?);
    let has_vmclock_abi = match version {
        (SNAPSHOT_V1_DEVICE_MAJOR, SNAPSHOT_V1_DEVICE_MINOR, SNAPSHOT_V1_DEVICE_PATCH) => true,
        (SNAPSHOT_V1_DEVICE_MAJOR, SNAPSHOT_V1_DEVICE_LEGACY_MINOR, SNAPSHOT_V1_DEVICE_PATCH) => {
            false
        }
        _ => return Err(SnapshotV1DeviceDecodeError::UnsupportedVersion),
    };
    if reader.read_u16()? != SNAPSHOT_V1_DEVICE_PROFILE {
        return Err(SnapshotV1DeviceDecodeError::UnsupportedProfile);
    }
    if reader.read_u32()? != SNAPSHOT_V1_DEVICE_FLAGS {
        return Err(SnapshotV1DeviceDecodeError::UnsupportedFlags);
    }
    let body_len = usize::try_from(reader.read_u32()?)
        .map_err(|_| SnapshotV1DeviceDecodeError::LengthMismatch)?;
    reader.read_zeroes(8)?;
    let expected_len = SNAPSHOT_V1_DEVICE_HEADER_SIZE
        .checked_add(body_len)
        .ok_or(SnapshotV1DeviceDecodeError::LengthMismatch)?;
    if expected_len != bytes.len() {
        return Err(SnapshotV1DeviceDecodeError::LengthMismatch);
    }

    if reader.read_u16()? != SNAPSHOT_V1_DEVICE_BLOCK_COUNT
        || reader.read_u16()? != SNAPSHOT_V1_DEVICE_FRESH_SERIAL_POLICY
        || reader.read_u16()? != SNAPSHOT_V1_DEVICE_REGENERATE_VMGENID_POLICY
        || reader.read_u16()? != SNAPSHOT_V1_DEVICE_SHARED_VMCLOCK_POLICY
    {
        return Err(SnapshotV1DeviceDecodeError::InvalidPolicy);
    }
    reader.read_zeroes(8)?;

    let drive_id = reader.read_string(SNAPSHOT_V1_DEVICE_MAX_DRIVE_ID_BYTES, false)?;
    let path = PathBuf::from(reader.read_string(SNAPSHOT_V1_DEVICE_MAX_PATH_BYTES, false)?);
    let partuuid_present = reader.read_bool()?;
    reader.read_zeroes(1)?;
    let partuuid_len = usize::from(reader.read_u16()?);
    let partuuid = match (partuuid_present, partuuid_len) {
        (false, 0) => None,
        (true, len) if len != 0 && len <= SNAPSHOT_V1_DEVICE_MAX_PARTUUID_BYTES => {
            Some(reader.read_string_bytes(len)?)
        }
        _ => return Err(SnapshotV1DeviceDecodeError::InvalidStringLength),
    };
    let cache_type = match reader.read_u8()? {
        0 => DriveCacheType::Unsafe,
        1 => DriveCacheType::Writeback,
        _ => return Err(SnapshotV1DeviceDecodeError::InvalidEnum),
    };
    if reader.read_u8()? != 0 || !reader.read_bool()? || !reader.read_bool()? {
        return Err(SnapshotV1DeviceDecodeError::InvalidPolicy);
    }
    let device_id =
        VirtioBlockDeviceId::new(reader.read_array::<{ VIRTIO_BLOCK_ID_BYTES as usize }>()?);
    let capacity_sectors = reader.read_u64()?;
    let backing_identity = decode_backing_identity(&mut reader)?;
    let block_mmio = decode_mmio_metadata(&mut reader)?;
    let (transport, active_queue) = decode_transport(&mut reader)?;
    let (rate_limiter_config, rate_limiter) = decode_limiter(&mut reader)?;
    let block_retry = decode_retry(&mut reader)?;
    let serial_mmio = decode_mmio_metadata(&mut reader)?;
    let serial_state = decode_serial_state(&mut reader)?;
    let vmgenid = decode_platform_metadata(&mut reader)?;
    let vmclock = decode_platform_metadata(&mut reader)?;
    let vmclock_abi = if has_vmclock_abi {
        Some(
            VmClockAbi::from_bytes(reader.read_array::<VMCLOCK_ABI_SIZE>()?)
                .map_err(|_| SnapshotV1DeviceDecodeError::InvalidVmClockAbi)?,
        )
    } else {
        None
    };

    if !reader.is_finished() {
        return Err(SnapshotV1DeviceDecodeError::TrailingData);
    }

    let runtime = VirtioBlockRuntimeState::new(transport, active_queue, rate_limiter);
    let root_block = SnapshotV1RootBlockState::from_parts(SnapshotV1RootBlockStateParts {
        drive_id,
        path,
        partuuid,
        cache_type,
        rate_limiter_config,
        device_id,
        capacity_sectors,
        backing_identity,
        mmio: block_mmio,
        runtime,
    });
    Ok(SnapshotV1DeviceState::new(
        root_block,
        block_retry,
        serial_mmio,
        serial_state,
        vmgenid,
        vmclock,
        vmclock_abi,
    ))
}

fn decode_backing_identity(
    reader: &mut DeviceStateReader<'_>,
) -> Result<BlockFileBackingIdentity, SnapshotV1DeviceDecodeError> {
    let file = [reader.read_u64()?, reader.read_u64()?, reader.read_u64()?];
    let mode = reader.read_u32()?;
    reader.read_zeroes(4)?;
    let modified = [reader.read_i64()?, i64::from(reader.read_u32()?)];
    reader.read_zeroes(4)?;
    let changed = [reader.read_i64()?, i64::from(reader.read_u32()?)];
    reader.read_zeroes(4)?;
    BlockFileBackingIdentity::new(file, mode, modified, changed)
        .ok_or(SnapshotV1DeviceDecodeError::InvalidBackingIdentity)
}

fn decode_mmio_metadata(
    reader: &mut DeviceStateReader<'_>,
) -> Result<SnapshotV1MmioDeviceMetadata, SnapshotV1DeviceDecodeError> {
    let region_id = MmioRegionId::new(reader.read_u64()?);
    let base = GuestAddress::new(reader.read_u64()?);
    let size = reader.read_u64()?;
    let interrupt_line = GuestInterruptLine::new(reader.read_u32()?)
        .map_err(|_| SnapshotV1DeviceDecodeError::InvalidInterrupt)?;
    reader.read_zeroes(4)?;
    let region = MmioRegion::new(region_id, base, size)
        .map_err(|_| SnapshotV1DeviceDecodeError::InvalidMetadata)?;
    Ok(SnapshotV1MmioDeviceMetadata::new(region, interrupt_line))
}

fn decode_transport(
    reader: &mut DeviceStateReader<'_>,
) -> Result<
    (
        VirtioMmioTransportState,
        Option<crate::block::VirtioBlockQueueState>,
    ),
    SnapshotV1DeviceDecodeError,
> {
    let device_id = reader.read_u32()?;
    let vendor_id = reader.read_u32()?;
    let device_features = reader.read_u64()?;
    let config_generation = reader.read_u32()?;
    let device_features_select = reader.read_u32()?;
    let driver_features_select = reader.read_u32()?;
    let status = reader.read_u32()?;
    let driver_features = reader.read_u64()?;
    let queue_select = reader.read_u32()?;
    let device_activated = reader.read_bool()?;
    if reader.read_u8()? != 1 || reader.read_u8()? != 1 {
        return Err(SnapshotV1DeviceDecodeError::UnsupportedProfile);
    }
    reader.read_zeroes(1)?;
    let interrupt_status = DeviceInterruptStatus::from_bits(reader.read_u32()?)
        .map_err(|_| SnapshotV1DeviceDecodeError::InvalidTransport)?;
    reader.read_zeroes(4)?;
    let pending_notification = reader.read_bool()?;
    reader.read_zeroes(7)?;

    let max_size = reader.read_u16()?;
    let size = reader.read_u16()?;
    let ready = reader.read_bool()?;
    let cursor_present = reader.read_bool()?;
    reader.read_zeroes(2)?;
    let descriptor_table = GuestAddress::new(reader.read_u64()?);
    let driver_ring = GuestAddress::new(reader.read_u64()?);
    let device_ring = GuestAddress::new(reader.read_u64()?);
    let next_available = reader.read_u16()?;
    let next_used = reader.read_u16()?;
    reader.read_zeroes(4)?;
    let active_queue = if cursor_present {
        Some(crate::block::VirtioBlockQueueState::new(
            next_available,
            next_used,
        ))
    } else if next_available == 0 && next_used == 0 {
        None
    } else {
        return Err(SnapshotV1DeviceDecodeError::InvalidTransport);
    };

    if device_features & VIRTIO_MMIO_VERSION_1_FEATURE == 0 {
        return Err(SnapshotV1DeviceDecodeError::InvalidTransport);
    }
    let device = VirtioMmioDeviceRegisters::with_vendor_id_and_config_generation(
        device_id,
        vendor_id,
        device_features,
        config_generation,
    )
    .with_runtime_state(
        [device_features_select, driver_features_select],
        driver_features,
        status,
    );
    let queue = VirtioMmioQueueState::from_parts(
        max_size,
        size,
        ready,
        descriptor_table,
        driver_ring,
        device_ring,
    );
    let mut queues = Vec::new();
    queues
        .try_reserve_exact(1)
        .map_err(|_| SnapshotV1DeviceDecodeError::Allocation)?;
    queues.push(queue);
    let mut pending_notifications = Vec::new();
    pending_notifications
        .try_reserve_exact(1)
        .map_err(|_| SnapshotV1DeviceDecodeError::Allocation)?;
    pending_notifications.push(pending_notification);
    let transport = VirtioMmioTransportState::from_parts(
        device,
        queue_select,
        queues,
        pending_notifications,
        interrupt_status,
        device_activated,
    );
    Ok((transport, active_queue))
}

fn decode_limiter(
    reader: &mut DeviceStateReader<'_>,
) -> Result<
    (Option<DriveRateLimiterConfig>, VirtioBlockRateLimiterState),
    SnapshotV1DeviceDecodeError,
> {
    let limiter_present = reader.read_bool()?;
    reader.read_zeroes(7)?;
    let (bandwidth_config, bandwidth_state) = decode_bucket(reader)?;
    let (ops_config, ops_state) = decode_bucket(reader)?;
    if !limiter_present
        && (bandwidth_config.is_some()
            || bandwidth_state.is_some()
            || ops_config.is_some()
            || ops_state.is_some())
    {
        return Err(SnapshotV1DeviceDecodeError::InvalidLimiter);
    }
    let config =
        limiter_present.then_some(DriveRateLimiterConfig::new(bandwidth_config, ops_config));
    Ok((
        config,
        VirtioBlockRateLimiterState::new(bandwidth_state, ops_state),
    ))
}

fn decode_bucket(
    reader: &mut DeviceStateReader<'_>,
) -> Result<
    (
        Option<DriveTokenBucketConfig>,
        Option<VirtioBlockTokenBucketState>,
    ),
    SnapshotV1DeviceDecodeError,
> {
    let config_present = reader.read_bool()?;
    let runtime_present = reader.read_bool()?;
    let burst_present = reader.read_bool()?;
    reader.read_zeroes(1)?;
    let size = reader.read_u64()?;
    let configured_burst = reader.read_u64()?;
    let refill_time = reader.read_u64()?;
    let budget = reader.read_u64()?;
    let remaining_burst = reader.read_u64()?;
    let age_nanos = reader.read_u64()?;

    if !config_present {
        if runtime_present
            || burst_present
            || size != 0
            || configured_burst != 0
            || refill_time != 0
            || budget != 0
            || remaining_burst != 0
            || age_nanos != 0
        {
            return Err(SnapshotV1DeviceDecodeError::InvalidLimiter);
        }
        return Ok((None, None));
    }
    if !burst_present && configured_burst != 0 {
        return Err(SnapshotV1DeviceDecodeError::InvalidLimiter);
    }
    let config =
        DriveTokenBucketConfig::new(size, burst_present.then_some(configured_burst), refill_time);
    if runtime_present {
        if !block_bucket_is_enabled(config) || budget > size || remaining_burst > configured_burst {
            return Err(SnapshotV1DeviceDecodeError::InvalidLimiter);
        }
        Ok((
            Some(config),
            Some(VirtioBlockTokenBucketState::new(
                config,
                budget,
                remaining_burst,
                age_nanos,
            )),
        ))
    } else if budget == 0
        && remaining_burst == 0
        && age_nanos == 0
        && !block_bucket_is_enabled(config)
    {
        Ok((Some(config), None))
    } else {
        Err(SnapshotV1DeviceDecodeError::InvalidLimiter)
    }
}

fn decode_retry(
    reader: &mut DeviceStateReader<'_>,
) -> Result<SnapshotV1BlockRetryState, SnapshotV1DeviceDecodeError> {
    let tag = reader.read_u8()?;
    reader.read_zeroes(7)?;
    let remaining_nanos = reader.read_u64()?;
    match (tag, remaining_nanos) {
        (0, 0) => Ok(SnapshotV1BlockRetryState::None),
        (1, 0) => Ok(SnapshotV1BlockRetryState::Immediate),
        (2, remaining_nanos) if remaining_nanos != 0 => {
            Ok(SnapshotV1BlockRetryState::After { remaining_nanos })
        }
        _ => Err(SnapshotV1DeviceDecodeError::InvalidRetry),
    }
}

fn decode_serial_state(
    reader: &mut DeviceStateReader<'_>,
) -> Result<SerialMmioState, SnapshotV1DeviceDecodeError> {
    let state = SerialMmioState::new(
        reader.read_u8()?,
        reader.read_u8()?,
        reader.read_u8()?,
        reader.read_u8()?,
        reader.read_u8()?,
        reader.read_u8()?,
    );
    reader.read_zeroes(2)?;
    Ok(state)
}

fn decode_platform_metadata(
    reader: &mut DeviceStateReader<'_>,
) -> Result<SnapshotV1PlatformDeviceMetadata, SnapshotV1DeviceDecodeError> {
    let range = GuestMemoryRange::new(GuestAddress::new(reader.read_u64()?), reader.read_u64()?)
        .map_err(|_| SnapshotV1DeviceDecodeError::InvalidMetadata)?;
    let fdt_region = Arm64FdtRegion {
        base: reader.read_u64()?,
        size: reader.read_u64()?,
    };
    let interrupt_line = GuestInterruptLine::new(reader.read_u32()?)
        .map_err(|_| SnapshotV1DeviceDecodeError::InvalidInterrupt)?;
    reader.read_zeroes(4)?;
    Ok(SnapshotV1PlatformDeviceMetadata::new(
        range,
        fdt_region,
        interrupt_line,
    ))
}

struct DeviceStateReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> DeviceStateReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    const fn is_finished(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn read_u8(&mut self) -> Result<u8, SnapshotV1DeviceDecodeError> {
        Ok(self.read_array::<1>()?[0])
    }

    fn read_u16(&mut self) -> Result<u16, SnapshotV1DeviceDecodeError> {
        Ok(u16::from_le_bytes(self.read_array()?))
    }

    fn read_u32(&mut self) -> Result<u32, SnapshotV1DeviceDecodeError> {
        Ok(u32::from_le_bytes(self.read_array()?))
    }

    fn read_u64(&mut self) -> Result<u64, SnapshotV1DeviceDecodeError> {
        Ok(u64::from_le_bytes(self.read_array()?))
    }

    fn read_i64(&mut self) -> Result<i64, SnapshotV1DeviceDecodeError> {
        Ok(i64::from_le_bytes(self.read_array()?))
    }

    fn read_bool(&mut self) -> Result<bool, SnapshotV1DeviceDecodeError> {
        match self.read_u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(SnapshotV1DeviceDecodeError::InvalidBoolean),
        }
    }

    fn read_zeroes(&mut self, len: usize) -> Result<(), SnapshotV1DeviceDecodeError> {
        if self.read_bytes(len)?.iter().copied().any(|byte| byte != 0) {
            Err(SnapshotV1DeviceDecodeError::NonzeroReserved)
        } else {
            Ok(())
        }
    }

    fn read_string(
        &mut self,
        max: usize,
        allow_empty: bool,
    ) -> Result<String, SnapshotV1DeviceDecodeError> {
        let len = usize::from(self.read_u16()?);
        if len > max || (!allow_empty && len == 0) {
            return Err(SnapshotV1DeviceDecodeError::InvalidStringLength);
        }
        self.read_string_bytes(len)
    }

    fn read_string_bytes(&mut self, len: usize) -> Result<String, SnapshotV1DeviceDecodeError> {
        let value = std::str::from_utf8(self.read_bytes(len)?)
            .map_err(|_| SnapshotV1DeviceDecodeError::InvalidUtf8)?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(value.len())
            .map_err(|_| SnapshotV1DeviceDecodeError::Allocation)?;
        owned.push_str(value);
        Ok(owned)
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N], SnapshotV1DeviceDecodeError> {
        self.read_bytes(N)?
            .try_into()
            .map_err(|_| SnapshotV1DeviceDecodeError::Truncated)
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], SnapshotV1DeviceDecodeError> {
        let end = self
            .position
            .checked_add(len)
            .ok_or(SnapshotV1DeviceDecodeError::Truncated)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(SnapshotV1DeviceDecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs::{self, File, OpenOptions};
    use std::io::Write as _;
    use std::os::unix::ffi::OsStringExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    use crate::block::{
        BlockFileBacking, BlockFileBackingIdentity, DriveCacheType, DriveRateLimiterConfig,
        DriveTokenBucketConfig, VIRTIO_BLOCK_DEVICE_ID, VIRTIO_BLOCK_QUEUE_SIZE,
        VirtioBlockConfigSpace, VirtioBlockDeviceId, VirtioBlockQueueState,
        VirtioBlockRateLimiterState, VirtioBlockRuntimeState, VirtioBlockTokenBucketState,
    };
    use crate::fdt::Arm64FdtRegion;
    use crate::interrupt::{DeviceInterruptKind, DeviceInterruptStatus, GuestInterruptLine};
    use crate::machine::MachineConfig;
    use crate::memory::{GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange, aarch64};
    use crate::mmio::{MmioRegion, MmioRegionId};
    use crate::rtc::{
        Pl031RtcDevice, RTC_CONTROL_REGISTER_OFFSET, RTC_DATA_REGISTER_OFFSET,
        RTC_INTERRUPT_MASK_REGISTER_OFFSET, RTC_MASKED_INTERRUPT_STATUS_REGISTER_OFFSET,
        RTC_MATCH_REGISTER_OFFSET, RTC_RAW_INTERRUPT_STATUS_REGISTER_OFFSET, RtcMmioLayout,
    };
    use crate::serial::SerialMmioState;
    use crate::startup::{
        PrepareSnapshotV1DeviceProfileError, install_snapshot_v1_runtime,
        prepare_snapshot_v1_device_profile, prepare_snapshot_v1_device_profile_with_root_backing,
        prepare_snapshot_v1_root_backing_file,
    };
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_MMIO_DEVICE_WINDOW_SIZE, VirtioMmioDeviceRegisters, VirtioMmioQueueState,
        VirtioMmioTransportState,
    };
    use crate::vmclock::VmClockAbi;

    use super::{
        SNAPSHOT_V1_DEVICE_HEADER_SIZE, SNAPSHOT_V1_DEVICE_MAGIC,
        SNAPSHOT_V1_DEVICE_MAX_PATH_BYTES, SnapshotV1BlockRetryState, SnapshotV1DeviceDecodeError,
        SnapshotV1DeviceEncodeError, SnapshotV1DeviceState, SnapshotV1MmioDeviceMetadata,
        SnapshotV1PlatformDeviceMetadata, SnapshotV1RootBlockState, SnapshotV1RootBlockStateParts,
        decode_snapshot_v1_device_state, encode_snapshot_v1_device_state,
    };

    static NEXT_TEMP_PATH_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn new(bytes: &[u8]) -> Self {
            let id = NEXT_TEMP_PATH_ID.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos());
            let path = std::env::temp_dir().join(format!(
                "bangbang-snapshot-device-test-{}-{timestamp}-{id}.img",
                std::process::id()
            ));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .expect("snapshot device test file should create");
            file.write_all(bytes)
                .expect("snapshot device test file should write");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    fn line(value: u32) -> GuestInterruptLine {
        GuestInterruptLine::new(value).expect("test interrupt line should be valid")
    }

    fn mmio(id: u64, base: u64, line_value: u32) -> SnapshotV1MmioDeviceMetadata {
        SnapshotV1MmioDeviceMetadata::new(
            MmioRegion::new(
                MmioRegionId::new(id),
                GuestAddress::new(base),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .expect("test MMIO region should be valid"),
            line(line_value),
        )
    }

    fn platform(base: u64, size: u64, line_value: u32) -> SnapshotV1PlatformDeviceMetadata {
        SnapshotV1PlatformDeviceMetadata::new(
            GuestMemoryRange::new(GuestAddress::new(base), size)
                .expect("test platform range should be valid"),
            Arm64FdtRegion { base, size },
            line(line_value),
        )
    }

    fn fixture_with_path(
        path: PathBuf,
        identity: BlockFileBackingIdentity,
    ) -> SnapshotV1DeviceState {
        let config_space = VirtioBlockConfigSpace::new(512, true, DriveCacheType::Unsafe);
        let device = VirtioMmioDeviceRegisters::new(
            VIRTIO_BLOCK_DEVICE_ID,
            config_space.available_features(),
        );
        let queue = VirtioMmioQueueState::from_parts(
            VIRTIO_BLOCK_QUEUE_SIZE,
            0,
            false,
            GuestAddress::new(0),
            GuestAddress::new(0),
            GuestAddress::new(0),
        );
        let transport = VirtioMmioTransportState::from_parts(
            device,
            0,
            vec![queue],
            vec![false],
            DeviceInterruptStatus::empty(),
            false,
        );
        let runtime = VirtioBlockRuntimeState::new(
            transport,
            None,
            VirtioBlockRateLimiterState::new(None, None),
        );
        let root = SnapshotV1RootBlockState::from_parts(SnapshotV1RootBlockStateParts {
            drive_id: "rootfs".to_string(),
            path,
            partuuid: Some("root-part".to_string()),
            cache_type: DriveCacheType::Unsafe,
            rate_limiter_config: None,
            device_id: VirtioBlockDeviceId::from_bytes(b"rootfs"),
            capacity_sectors: 1,
            backing_identity: identity,
            mmio: mmio(1, 0x5000_0000, 32),
            runtime,
        });
        SnapshotV1DeviceState::new(
            root,
            SnapshotV1BlockRetryState::None,
            mmio(20, 0x4000_2000, 33),
            SerialMmioState::new(1, 0x83, 3, 0x5a, 0x34, 0x12),
            platform(0x1000, 16, 34),
            platform(0x2000, 4096, 35),
            Some(VmClockAbi::initial()),
        )
    }

    fn fixture() -> SnapshotV1DeviceState {
        fixture_with_path(
            PathBuf::from("/tmp/rootfs.img"),
            BlockFileBackingIdentity::new(
                [1, 2, 512],
                u32::from(libc::S_IFREG | 0o444),
                [3, 4],
                [5, 6],
            )
            .expect("test backing identity should be valid"),
        )
    }

    fn write_fixture_vmclock(memory: &mut GuestMemory, state: &SnapshotV1DeviceState) {
        memory
            .write_slice(
                &state
                    .vmclock_abi()
                    .expect("current fixture should carry VMClock state")
                    .to_bytes(),
                state.vmclock().range().start(),
            )
            .expect("fixture VMClock ABI should write");
    }

    #[test]
    fn native_v1_device_codec_round_trips_deterministically() {
        let state = fixture();

        let first = encode_snapshot_v1_device_state(&state).expect("fixture should encode");
        let second = encode_snapshot_v1_device_state(&state).expect("fixture should re-encode");
        let decoded = decode_snapshot_v1_device_state(&first).expect("fixture should decode");

        assert_eq!(first, second);
        assert_eq!(decoded, state);
        assert!(
            decoded
                .root_block()
                .backing_identity()
                .kind()
                .is_regular_file()
        );
        assert_eq!(&first[..8], &SNAPSHOT_V1_DEVICE_MAGIC);
        assert_eq!(first.len(), 678);
        assert_eq!(
            u32::from_le_bytes(first[20..24].try_into().expect("body length should exist")),
            u32::try_from(first.len() - SNAPSHOT_V1_DEVICE_HEADER_SIZE)
                .expect("fixture length should fit")
        );
    }

    #[test]
    fn native_v1_device_codec_retains_legacy_vmclock_page_policy() {
        let mut state = fixture();
        state.vmclock_abi = None;

        let encoded =
            encode_snapshot_v1_device_state(&state).expect("legacy fixture should encode");
        let decoded =
            decode_snapshot_v1_device_state(&encoded).expect("legacy fixture should decode");

        assert_eq!(encoded.len(), 566);
        assert_eq!(u16::from_le_bytes([encoded[10], encoded[11]]), 0);
        assert_eq!(decoded, state);
        assert_eq!(decoded.vmclock_abi(), None);
    }

    #[test]
    fn native_v1_device_encoder_rejects_in_memory_block_backing_identity() {
        let block_identity = BlockFileBackingIdentity::new_block_device(
            [1, 2, 512],
            3,
            512,
            u32::from(libc::S_IFBLK | 0o444),
            [4, 5],
            [6, 7],
        )
        .expect("synthetic block identity should validate");
        let state = fixture_with_path(PathBuf::from("/tmp/block-device"), block_identity);

        assert_eq!(
            encode_snapshot_v1_device_state(&state)
                .expect_err("native-v1 must remain regular-file-only"),
            SnapshotV1DeviceEncodeError::InvalidState
        );
    }

    #[test]
    fn native_v1_device_codec_round_trips_active_limited_retry_state() {
        let mut state = fixture();
        let config_space = VirtioBlockConfigSpace::new(512, true, DriveCacheType::Unsafe);
        let features = config_space.available_features();
        let status = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
            | VIRTIO_DEVICE_STATUS_DRIVER
            | VIRTIO_DEVICE_STATUS_FEATURES_OK
            | VIRTIO_DEVICE_STATUS_DRIVER_OK;
        let device = VirtioMmioDeviceRegisters::new(VIRTIO_BLOCK_DEVICE_ID, features)
            .with_runtime_state([1, 1], features, status);
        let queue = VirtioMmioQueueState::from_parts(
            VIRTIO_BLOCK_QUEUE_SIZE,
            8,
            true,
            GuestAddress::new(0x4000),
            GuestAddress::new(0x5000),
            GuestAddress::new(0x6000),
        );
        let mut interrupts = DeviceInterruptStatus::empty();
        interrupts.insert(DeviceInterruptKind::Queue);
        let transport = VirtioMmioTransportState::from_parts(
            device,
            0,
            vec![queue],
            vec![false],
            interrupts,
            true,
        );
        let bucket_config = DriveTokenBucketConfig::new(100, Some(10), 1000);
        state.root_block.rate_limiter_config =
            Some(DriveRateLimiterConfig::new(Some(bucket_config), None));
        state.root_block.runtime = VirtioBlockRuntimeState::new(
            transport,
            Some(VirtioBlockQueueState::new(u16::MAX, 7)),
            VirtioBlockRateLimiterState::new(
                Some(VirtioBlockTokenBucketState::new(bucket_config, 41, 3, 99)),
                None,
            ),
        );
        state.block_retry = SnapshotV1BlockRetryState::Immediate;

        let encoded = encode_snapshot_v1_device_state(&state)
            .expect("active limited retry state should encode");
        let decoded = decode_snapshot_v1_device_state(&encoded)
            .expect("active limited retry state should decode");

        assert_eq!(decoded, state);
        assert!(decoded.root_block().runtime().active_queue().is_some());
        assert_eq!(decoded.block_retry(), SnapshotV1BlockRetryState::Immediate);

        state.root_block.rate_limiter_config = None;
        let transport = state.root_block.runtime().transport().clone();
        let active_queue = state.root_block.runtime().active_queue();
        state.root_block.runtime = VirtioBlockRuntimeState::new(
            transport,
            active_queue,
            VirtioBlockRateLimiterState::new(None, None),
        );
        assert_eq!(
            encode_snapshot_v1_device_state(&state)
                .expect_err("retry without a live limiter should reject"),
            SnapshotV1DeviceEncodeError::InvalidRetry
        );
    }

    #[test]
    fn native_v1_device_decoder_rejects_header_mutations_and_trailing_data() {
        let encoded = encode_snapshot_v1_device_state(&fixture()).expect("fixture should encode");
        for (offset, expected) in [
            (0, SnapshotV1DeviceDecodeError::InvalidMagic),
            (8, SnapshotV1DeviceDecodeError::UnsupportedVersion),
            (16, SnapshotV1DeviceDecodeError::UnsupportedFlags),
            (24, SnapshotV1DeviceDecodeError::NonzeroReserved),
        ] {
            let mut mutated = encoded.clone();
            mutated[offset] ^= 0xff;
            assert_eq!(
                decode_snapshot_v1_device_state(&mutated)
                    .expect_err("mutated header should reject"),
                expected
            );
        }

        for len in 0..SNAPSHOT_V1_DEVICE_HEADER_SIZE {
            assert_eq!(
                decode_snapshot_v1_device_state(&encoded[..len])
                    .expect_err("short header should reject"),
                SnapshotV1DeviceDecodeError::TooSmall
            );
        }

        let mut trailing = encoded.clone();
        trailing.push(0);
        let body_len = u32::try_from(trailing.len() - SNAPSHOT_V1_DEVICE_HEADER_SIZE)
            .expect("trailing length should fit");
        trailing[20..24].copy_from_slice(&body_len.to_le_bytes());
        assert_eq!(
            decode_snapshot_v1_device_state(&trailing).expect_err("trailing byte should reject"),
            SnapshotV1DeviceDecodeError::TrailingData
        );

        let mut invalid_vmclock = encoded.clone();
        let vmclock_offset = invalid_vmclock.len() - super::VMCLOCK_ABI_SIZE;
        invalid_vmclock[vmclock_offset] ^= 0xff;
        assert_eq!(
            decode_snapshot_v1_device_state(&invalid_vmclock)
                .expect_err("invalid VMClock payload should reject"),
            SnapshotV1DeviceDecodeError::InvalidVmClockAbi
        );
    }

    #[test]
    fn native_v1_device_encoder_enforces_path_bounds_and_utf8() {
        let mut state = fixture();
        state.root_block.path = PathBuf::from("a".repeat(SNAPSHOT_V1_DEVICE_MAX_PATH_BYTES));
        state.root_block.drive_id = "d".repeat(super::SNAPSHOT_V1_DEVICE_MAX_DRIVE_ID_BYTES);
        state.root_block.partuuid = Some("p".repeat(super::SNAPSHOT_V1_DEVICE_MAX_PARTUUID_BYTES));
        let maximum =
            encode_snapshot_v1_device_state(&state).expect("maximum bounded strings should encode");
        assert!(maximum.len() < super::SNAPSHOT_V1_DEVICE_MAX_SIZE);

        state.root_block.path = PathBuf::from("a".repeat(SNAPSHOT_V1_DEVICE_MAX_PATH_BYTES + 1));
        assert_eq!(
            encode_snapshot_v1_device_state(&state).expect_err("oversized path should reject"),
            SnapshotV1DeviceEncodeError::PathTooLong
        );

        state.root_block.path = PathBuf::from(OsString::from_vec(vec![0xff]));
        assert_eq!(
            encode_snapshot_v1_device_state(&state).expect_err("non-UTF-8 path should reject"),
            SnapshotV1DeviceEncodeError::NonUtf8Path
        );
    }

    #[test]
    fn device_state_and_errors_redact_sensitive_values() {
        let state = fixture();
        let debug = format!("{state:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("rootfs"));
        assert!(!debug.contains("/tmp/rootfs.img"));
        assert!(
            !SnapshotV1DeviceDecodeError::InvalidTransport
                .to_string()
                .contains("0x")
        );
    }

    #[test]
    fn preparation_reopens_backing_and_builds_fresh_serial_resources_off_side() {
        let file = TempFile::new(&[0x5a; 512]);
        let (observed_backing, identity) = BlockFileBacking::open_snapshot_read_only(file.path())
            .expect("test backing should identify");
        let backing_device_id = observed_backing.device_id();
        let mut state = fixture_with_path(file.path().to_path_buf(), identity);
        let vmclock_base = aarch64::SYSTEM_MEM_START + aarch64::SYSTEM_MEM_SIZE - 4096;
        let vmgenid_base = vmclock_base - 16;
        state.vmgenid = platform(vmgenid_base, 16, 34);
        state.vmclock = platform(vmclock_base, 4096, 35);
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(
                GuestAddress::new(aarch64::SYSTEM_MEM_START),
                aarch64::SYSTEM_MEM_SIZE,
            )
            .expect("test memory range should be valid"),
        ])
        .expect("test memory layout should be valid");
        let mut memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
        let generation_id = [0x7a; 16];
        memory
            .write_slice(&generation_id, state.vmgenid().range().start())
            .expect("source generation should write");
        write_fixture_vmclock(&mut memory, &state);

        let prepared = prepare_snapshot_v1_device_profile(&state, &memory, Instant::now())
            .expect("supported device state should prepare");

        assert_eq!(prepared.drive_config().path_on_host(), Some(file.path()));
        assert_eq!(
            prepared
                .block_handler()
                .activation_handler()
                .backing()
                .expect("snapshot file device should retain backing")
                .len(),
            512
        );
        assert!(!prepared.block_handler().is_device_activated());
        assert_eq!(prepared.serial_handler().state(), state.serial_state());
        assert_eq!(
            prepared
                .serial_output_buffer()
                .bytes()
                .expect("fresh serial buffer should read"),
            Vec::<u8>::new()
        );
        assert!(prepared.serial_handler().output().metrics().is_empty());
        assert_eq!(prepared.vmgenid_device().generation_id, generation_id);
        assert_eq!(prepared.vmclock_device().abi, VmClockAbi::initial());
        assert!(format!("{prepared:?}").contains("<redacted>"));
        assert!(!format!("{prepared:?}").contains(file.path().to_string_lossy().as_ref()));

        assert_eq!(
            prepared.block_handler().activation_handler().device_id(),
            VirtioBlockDeviceId::from_bytes(b"rootfs"),
            "legacy native-v1 state should preserve its persisted guest ID"
        );

        let mut current_identity_state = state.clone();
        current_identity_state.root_block.device_id = backing_device_id;
        let current =
            prepare_snapshot_v1_device_profile(&current_identity_state, &memory, Instant::now())
                .expect("metadata-derived native-v1 state should prepare");
        assert_eq!(
            current.block_handler().activation_handler().device_id(),
            backing_device_id
        );

        let mut unrelated_identity_state = state.clone();
        unrelated_identity_state.root_block.device_id = VirtioBlockDeviceId::new([0xff; 20]);
        assert_eq!(
            prepare_snapshot_v1_device_profile(&unrelated_identity_state, &memory, Instant::now(),)
                .expect_err("unrelated persisted block ID should reject"),
            PrepareSnapshotV1DeviceProfileError::BlockDeviceIdMismatch
        );

        let mut legacy_vmclock_state = state.clone();
        legacy_vmclock_state.vmclock_abi = None;
        let legacy =
            prepare_snapshot_v1_device_profile(&legacy_vmclock_state, &memory, Instant::now())
                .expect("legacy device state should recover VMClock from memory");
        assert_eq!(legacy.vmclock_device().abi, VmClockAbi::initial());

        let mut different_vmclock_bytes = VmClockAbi::initial().to_bytes();
        different_vmclock_bytes[104..112].copy_from_slice(&1_u64.to_le_bytes());
        memory
            .write_slice(&different_vmclock_bytes, state.vmclock().range().start())
            .expect("different valid VMClock should write");
        assert_eq!(
            prepare_snapshot_v1_device_profile(&state, &memory, Instant::now())
                .expect_err("encoded and memory VMClock mismatch should reject"),
            PrepareSnapshotV1DeviceProfileError::VmClockStateMismatch
        );

        let mut duplicate_region_id = state.clone();
        duplicate_region_id.serial_mmio = mmio(
            state.root_block().mmio().region().id().raw_value(),
            state.serial_mmio().region().range().start().raw_value(),
            state.serial_mmio().interrupt_line().raw_value(),
        );
        assert_eq!(
            prepare_snapshot_v1_device_profile(&duplicate_region_id, &memory, Instant::now())
                .expect_err("duplicate MMIO region IDs should reject"),
            PrepareSnapshotV1DeviceProfileError::ConflictingMetadata
        );

        let mut duplicate_interrupt = state.clone();
        duplicate_interrupt.serial_mmio = mmio(
            state.serial_mmio().region().id().raw_value(),
            state.serial_mmio().region().range().start().raw_value(),
            state.root_block().mmio().interrupt_line().raw_value(),
        );
        assert_eq!(
            prepare_snapshot_v1_device_profile(&duplicate_interrupt, &memory, Instant::now())
                .expect_err("duplicate device interrupts should reject"),
            PrepareSnapshotV1DeviceProfileError::ConflictingMetadata
        );
    }

    #[test]
    fn installation_consumes_prepared_state_without_boot_writes() {
        let file = TempFile::new(&[0x5a; 512]);
        let (_, identity) = BlockFileBacking::open_snapshot_read_only(file.path())
            .expect("test backing should identify");
        let mut state = fixture_with_path(file.path().to_path_buf(), identity);
        let vmclock_base = aarch64::SYSTEM_MEM_START + aarch64::SYSTEM_MEM_SIZE - 4096;
        let vmgenid_base = vmclock_base - 16;
        state.vmgenid = platform(vmgenid_base, 16, 34);
        state.vmclock = platform(vmclock_base, 4096, 35);
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(
                GuestAddress::new(aarch64::SYSTEM_MEM_START),
                aarch64::SYSTEM_MEM_SIZE,
            )
            .expect("test memory range should be valid"),
        ])
        .expect("test memory layout should be valid");
        let mut memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
        let boot_area = vec![0xa5; 16 * 1024];
        memory
            .write_slice(&boot_area, GuestAddress::new(aarch64::SYSTEM_MEM_START))
            .expect("source boot area should write");
        memory
            .write_slice(&[0x7a; 16], state.vmgenid().range().start())
            .expect("source generation should write");
        write_fixture_vmclock(&mut memory, &state);

        let prepared = prepare_snapshot_v1_device_profile(&state, &memory, Instant::now())
            .expect("supported device state should prepare");
        let mut installed = install_snapshot_v1_runtime(
            prepared,
            MachineConfig::default(),
            memory,
            RtcMmioLayout::new(GuestAddress::new(0x4000_1000), MmioRegionId::new(10)),
        )
        .expect("prepared state should install");

        let mut restored_boot_area = vec![0; boot_area.len()];
        installed
            .memory
            .read_slice(
                &mut restored_boot_area,
                GuestAddress::new(aarch64::SYSTEM_MEM_START),
            )
            .expect("installed memory should remain readable");
        assert_eq!(restored_boot_area, boot_area);
        assert!(installed.runtime_resources.boot_origin.is_none());
        assert_eq!(installed.runtime_resources.block_devices.len(), 1);
        assert!(installed.runtime_resources.serial_device.is_some());
        assert!(installed.runtime_resources.rtc_device.is_some());
        assert_eq!(installed.drive_config.path_on_host(), Some(file.path()));
        assert_eq!(installed.block_retry, SnapshotV1BlockRetryState::None);
        assert_eq!(installed.mmio_dispatcher.regions().len(), 3);
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_secs();
        let before = u32::try_from(before).expect("PL031 test time should fit u32");
        let rtc = installed
            .mmio_dispatcher
            .handler_mut::<Pl031RtcDevice>(MmioRegionId::new(10))
            .expect("restored runtime should reconstruct PL031");
        let observed = rtc
            .read_register(RTC_DATA_REGISTER_OFFSET)
            .expect("restored PL031 wall clock should read");
        let after = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_secs();
        let after = u32::try_from(after).expect("PL031 test time should fit u32");
        assert!((before..=after).contains(&observed));
        for offset in [
            RTC_MATCH_REGISTER_OFFSET,
            RTC_CONTROL_REGISTER_OFFSET,
            RTC_INTERRUPT_MASK_REGISTER_OFFSET,
            RTC_RAW_INTERRUPT_STATUS_REGISTER_OFFSET,
            RTC_MASKED_INTERRUPT_STATUS_REGISTER_OFFSET,
        ] {
            assert_eq!(
                rtc.read_register(offset)
                    .expect("fresh PL031 no-alarm register should read"),
                0
            );
        }
        assert_eq!(
            installed
                .serial_output_buffer
                .bytes()
                .expect("fresh serial output should read"),
            Vec::<u8>::new()
        );
    }

    #[test]
    fn preparation_adopts_exact_supplied_backing_without_reopening_persisted_selector() {
        let file = TempFile::new(&[0x5a; 512]);
        let supplied = File::open(file.path()).expect("supplied backing should open");
        let moved = file.path().with_extension("opened");
        fs::rename(file.path(), &moved).expect("opened backing should move");
        fs::write(file.path(), [0x7b; 512]).expect("replacement path should create");
        let (_, identity) = BlockFileBacking::from_snapshot_read_only_file(
            supplied
                .try_clone()
                .expect("supplied backing should duplicate"),
        )
        .expect("supplied backing should identify after move");
        let mut state = fixture_with_path(PathBuf::from("bangbang-grant:root"), identity);
        let vmclock_base = aarch64::SYSTEM_MEM_START + aarch64::SYSTEM_MEM_SIZE - 4096;
        let vmgenid_base = vmclock_base - 16;
        state.vmgenid = platform(vmgenid_base, 16, 34);
        state.vmclock = platform(vmclock_base, 4096, 35);
        let supplied = prepare_snapshot_v1_root_backing_file(&state, Some(supplied))
            .expect("exact supplied backing should validate before memory preparation");
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(
                GuestAddress::new(aarch64::SYSTEM_MEM_START),
                aarch64::SYSTEM_MEM_SIZE,
            )
            .expect("test memory range should be valid"),
        ])
        .expect("test memory layout should be valid");
        let mut memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
        memory
            .write_slice(&[0x6b; 16], state.vmgenid().range().start())
            .expect("source generation should write");
        write_fixture_vmclock(&mut memory, &state);

        let prepared = prepare_snapshot_v1_device_profile_with_root_backing(
            &state,
            &memory,
            Instant::now(),
            Some(supplied),
        )
        .expect("exact supplied backing should prepare without selector lookup");

        assert_eq!(
            prepared.drive_config().path_on_host(),
            Some(Path::new("bangbang-grant:root"))
        );
        assert!(
            prepared
                .block_handler()
                .activation_handler()
                .backing()
                .expect("snapshot file device should retain backing")
                .uses_supplied_file()
        );
        assert_eq!(
            prepared
                .block_handler()
                .activation_handler()
                .backing()
                .expect("snapshot file device should retain backing")
                .snapshot_identity()
                .expect("prepared backing identity should read"),
            identity
        );
        fs::remove_file(moved).expect("moved backing fixture should clean up");
    }

    #[test]
    fn preparation_rejects_replaced_backing_without_mutating_memory() {
        let file = TempFile::new(&[0x5a; 512]);
        let (_, identity) = BlockFileBacking::open_snapshot_read_only(file.path())
            .expect("test backing should identify");
        let mut state = fixture_with_path(file.path().to_path_buf(), identity);
        let vmclock_base = aarch64::SYSTEM_MEM_START + aarch64::SYSTEM_MEM_SIZE - 4096;
        let vmgenid_base = vmclock_base - 16;
        state.vmgenid = platform(vmgenid_base, 16, 34);
        state.vmclock = platform(vmclock_base, 4096, 35);
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(
                GuestAddress::new(aarch64::SYSTEM_MEM_START),
                aarch64::SYSTEM_MEM_SIZE,
            )
            .expect("test memory range should be valid"),
        ])
        .expect("test memory layout should be valid");
        let mut memory = GuestMemory::allocate(&layout).expect("test memory should allocate");
        let generation_id = [0x6b; 16];
        memory
            .write_slice(&generation_id, state.vmgenid().range().start())
            .expect("source generation should write");

        fs::remove_file(file.path()).expect("source backing should remove");
        let mut replacement = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(file.path())
            .expect("replacement backing should create");
        replacement
            .write_all(&[0x7b; 1024])
            .expect("replacement backing should write");
        drop(replacement);

        assert_eq!(
            prepare_snapshot_v1_device_profile(&state, &memory, Instant::now())
                .expect_err("replaced backing should reject"),
            PrepareSnapshotV1DeviceProfileError::BlockBackingMismatch
        );
        let mut observed = [0; 16];
        memory
            .read_slice(&mut observed, state.vmgenid().range().start())
            .expect("source generation should remain readable");
        assert_eq!(observed, generation_id);
    }
}
