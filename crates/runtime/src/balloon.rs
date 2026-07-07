//! Backend-neutral virtio-balloon configuration model.

use std::collections::TryReserveError;
use std::fmt;

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
    VirtqueueDescriptorChain, VirtqueueNotificationSuppression, VirtqueueUsedRing,
    VirtqueueUsedRingError, VirtqueueUsedRingPublication,
};

pub const VIRTIO_BALLOON_DEVICE_ID: u32 = 5;
pub const VIRTIO_BALLOON_QUEUE_SIZE: u16 = 256;
pub const VIRTIO_BALLOON_MIN_QUEUE_COUNT: usize = 2;
pub const VIRTIO_BALLOON_MAX_QUEUE_COUNT: usize = 5;
pub const VIRTIO_BALLOON_INFLATE_QUEUE_INDEX: usize = 0;
pub const VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX: usize = 1;
pub const VIRTIO_BALLOON_STATS_QUEUE_INDEX: usize = 2;
pub const VIRTIO_BALLOON_PFN_SIZE: usize = 4;
pub const VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR: usize = 256;
pub const VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE: usize =
    VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR * VIRTIO_BALLOON_PFN_SIZE;
pub const VIRTIO_BALLOON_MIB_TO_4K_PAGES: u32 = 256;
pub const VIRTIO_BALLOON_MAX_AMOUNT_MIB: u32 = u32::MAX / VIRTIO_BALLOON_MIB_TO_4K_PAGES;
pub const VIRTIO_BALLOON_CONFIG_SPACE_SIZE: usize = 12;
pub const VIRTIO_BALLOON_FREE_PAGE_HINT_STOP: u32 = 0;
pub const VIRTIO_BALLOON_FREE_PAGE_HINT_DONE: u32 = 1;
pub const VIRTIO_BALLOON_F_STATS_VQ: u32 = 1;
pub const VIRTIO_BALLOON_F_DEFLATE_ON_OOM: u32 = 2;
pub const VIRTIO_BALLOON_F_FREE_PAGE_HINTING: u32 = 3;
pub const VIRTIO_BALLOON_F_FREE_PAGE_REPORTING: u32 = 5;
pub const VIRTIO_FEATURE_VERSION_1: u32 = 32;

pub type VirtioBalloonMmioHandler =
    VirtioMmioRegisterHandler<VirtioBalloonConfigSpace, VirtioBalloonDevice>;

const fn virtio_feature_bit(feature: u32) -> u64 {
    1_u64 << feature
}

pub fn mib_to_4k_pages(amount_mib: u32) -> Result<u32, BalloonPageCountOverflow> {
    amount_mib
        .checked_mul(VIRTIO_BALLOON_MIB_TO_4K_PAGES)
        .ok_or(BalloonPageCountOverflow { amount_mib })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonPageCountOverflow {
    amount_mib: u32,
}

impl BalloonPageCountOverflow {
    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn max_amount_mib(self) -> u32 {
        VIRTIO_BALLOON_MAX_AMOUNT_MIB
    }
}

impl fmt::Display for BalloonPageCountOverflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "balloon amount_mib {} exceeds maximum {} MiB representable as 4 KiB pages",
            self.amount_mib,
            self.max_amount_mib()
        )
    }
}

impl std::error::Error for BalloonPageCountOverflow {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonConfigInput {
    amount_mib: u32,
    deflate_on_oom: bool,
    stats_polling_interval_s: u16,
    free_page_hinting: bool,
    free_page_reporting: bool,
}

impl BalloonConfigInput {
    pub const fn new(amount_mib: u32, deflate_on_oom: bool) -> Self {
        Self {
            amount_mib,
            deflate_on_oom,
            stats_polling_interval_s: 0,
            free_page_hinting: false,
            free_page_reporting: false,
        }
    }

    pub const fn with_stats_polling_interval_s(mut self, stats_polling_interval_s: u16) -> Self {
        self.stats_polling_interval_s = stats_polling_interval_s;
        self
    }

    pub const fn with_free_page_hinting(mut self, free_page_hinting: bool) -> Self {
        self.free_page_hinting = free_page_hinting;
        self
    }

    pub const fn with_free_page_reporting(mut self, free_page_reporting: bool) -> Self {
        self.free_page_reporting = free_page_reporting;
        self
    }

    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn deflate_on_oom(self) -> bool {
        self.deflate_on_oom
    }

    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }

    pub const fn free_page_hinting(self) -> bool {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> bool {
        self.free_page_reporting
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonConfig {
    amount_mib: u32,
    deflate_on_oom: bool,
    stats_polling_interval_s: u16,
    free_page_hinting: bool,
    free_page_reporting: bool,
}

impl BalloonConfig {
    pub const fn amount_mib(self) -> u32 {
        self.amount_mib
    }

    pub const fn deflate_on_oom(self) -> bool {
        self.deflate_on_oom
    }

    pub const fn stats_polling_interval_s(self) -> u16 {
        self.stats_polling_interval_s
    }

    pub const fn free_page_hinting(self) -> bool {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> bool {
        self.free_page_reporting
    }
}

impl From<BalloonConfigInput> for BalloonConfig {
    fn from(input: BalloonConfigInput) -> Self {
        Self {
            amount_mib: input.amount_mib,
            deflate_on_oom: input.deflate_on_oom,
            stats_polling_interval_s: input.stats_polling_interval_s,
            free_page_hinting: input.free_page_hinting,
            free_page_reporting: input.free_page_reporting,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioBalloonConfigSpace {
    num_pages: u32,
    actual_pages: u32,
    free_page_hint_cmd_id: u32,
}

impl VirtioBalloonConfigSpace {
    pub const fn new(num_pages: u32, actual_pages: u32, free_page_hint_cmd_id: u32) -> Self {
        Self {
            num_pages,
            actual_pages,
            free_page_hint_cmd_id,
        }
    }

    pub fn from_config(config: BalloonConfig) -> Result<Self, BalloonPageCountOverflow> {
        Ok(Self::new(
            mib_to_4k_pages(config.amount_mib())?,
            0,
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP,
        ))
    }

    pub const fn num_pages(self) -> u32 {
        self.num_pages
    }

    pub const fn actual_pages(self) -> u32 {
        self.actual_pages
    }

    pub const fn free_page_hint_cmd_id(self) -> u32 {
        self.free_page_hint_cmd_id
    }

    pub const fn from_le_bytes(bytes: [u8; VIRTIO_BALLOON_CONFIG_SPACE_SIZE]) -> Self {
        let [
            num_pages0,
            num_pages1,
            num_pages2,
            num_pages3,
            actual_pages0,
            actual_pages1,
            actual_pages2,
            actual_pages3,
            cmd0,
            cmd1,
            cmd2,
            cmd3,
        ] = bytes;
        Self {
            num_pages: u32::from_le_bytes([num_pages0, num_pages1, num_pages2, num_pages3]),
            actual_pages: u32::from_le_bytes([
                actual_pages0,
                actual_pages1,
                actual_pages2,
                actual_pages3,
            ]),
            free_page_hint_cmd_id: u32::from_le_bytes([cmd0, cmd1, cmd2, cmd3]),
        }
    }

    pub fn to_le_bytes(self) -> [u8; VIRTIO_BALLOON_CONFIG_SPACE_SIZE] {
        let [num_pages0, num_pages1, num_pages2, num_pages3] = self.num_pages.to_le_bytes();
        let [actual_pages0, actual_pages1, actual_pages2, actual_pages3] =
            self.actual_pages.to_le_bytes();
        let [cmd0, cmd1, cmd2, cmd3] = self.free_page_hint_cmd_id.to_le_bytes();

        [
            num_pages0,
            num_pages1,
            num_pages2,
            num_pages3,
            actual_pages0,
            actual_pages1,
            actual_pages2,
            actual_pages3,
            cmd0,
            cmd1,
            cmd2,
            cmd3,
        ]
    }
}

impl VirtioMmioDeviceConfigHandler for VirtioBalloonConfigSpace {
    fn read_device_config(
        &self,
        access: VirtioMmioDeviceConfigAccess,
    ) -> Result<MmioAccessBytes, VirtioMmioDeviceConfigError> {
        let bytes = self.to_le_bytes();
        let bytes = balloon_config_access_bytes(&bytes, access)?;
        MmioAccessBytes::new(bytes).map_err(balloon_config_bytes_error)
    }

    fn write_device_config(
        &mut self,
        access: VirtioMmioDeviceConfigAccess,
        data: MmioAccessBytes,
    ) -> Result<(), VirtioMmioDeviceConfigError> {
        let mut bytes = self.to_le_bytes();
        let destination = balloon_config_access_bytes_mut(&mut bytes, access)?;
        destination.copy_from_slice(data.as_slice());
        *self = Self::from_le_bytes(bytes);
        Ok(())
    }
}

impl TryFrom<BalloonConfig> for VirtioBalloonConfigSpace {
    type Error = BalloonPageCountOverflow;

    fn try_from(config: BalloonConfig) -> Result<Self, Self::Error> {
        Self::from_config(config)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonPfnPayload {
    pfns: Vec<u32>,
}

impl VirtioBalloonPfnPayload {
    pub fn parse(bytes: &[u8]) -> Result<Self, VirtioBalloonPfnPayloadParseError> {
        if bytes.is_empty() {
            return Err(VirtioBalloonPfnPayloadParseError::EmptyPayload);
        }
        if !bytes.len().is_multiple_of(VIRTIO_BALLOON_PFN_SIZE) {
            return Err(VirtioBalloonPfnPayloadParseError::UnalignedLength { len: bytes.len() });
        }

        let count = bytes.len() / VIRTIO_BALLOON_PFN_SIZE;
        if count > VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR {
            return Err(VirtioBalloonPfnPayloadParseError::TooManyPfns {
                count,
                max: VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR,
            });
        }

        let mut pfns = Vec::new();
        pfns.try_reserve_exact(count)
            .map_err(|source| VirtioBalloonPfnPayloadParseError::PfnAllocation { count, source })?;
        for chunk in bytes.chunks_exact(VIRTIO_BALLOON_PFN_SIZE) {
            let mut pfn = [0; VIRTIO_BALLOON_PFN_SIZE];
            pfn.copy_from_slice(chunk);
            pfns.push(u32::from_le_bytes(pfn));
        }

        Ok(Self { pfns })
    }

    pub fn pfns(&self) -> &[u32] {
        &self.pfns
    }

    pub fn into_vec(self) -> Vec<u32> {
        self.pfns
    }

    pub fn len(&self) -> usize {
        self.pfns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pfns.is_empty()
    }

    pub fn into_page_ranges(
        self,
    ) -> Result<VirtioBalloonPfnRanges, VirtioBalloonPfnRangeCompactError> {
        VirtioBalloonPfnRanges::from_pfns(self.pfns)
    }
}

#[derive(Debug)]
pub enum VirtioBalloonPfnPayloadParseError {
    EmptyPayload,
    UnalignedLength {
        len: usize,
    },
    TooManyPfns {
        count: usize,
        max: usize,
    },
    PfnAllocation {
        count: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtioBalloonPfnPayloadParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPayload => f.write_str("virtio-balloon PFN payload cannot be empty"),
            Self::UnalignedLength { len } => write!(
                f,
                "virtio-balloon PFN payload length {len} is not a multiple of {VIRTIO_BALLOON_PFN_SIZE}"
            ),
            Self::TooManyPfns { count, max } => write!(
                f,
                "virtio-balloon PFN payload contains {count} PFNs, exceeding maximum {max}"
            ),
            Self::PfnAllocation { count, source } => write!(
                f,
                "failed to allocate virtio-balloon PFN payload with {count} PFNs: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioBalloonPfnPayloadParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PfnAllocation { source, .. } => Some(source),
            Self::EmptyPayload | Self::UnalignedLength { .. } | Self::TooManyPfns { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonPfnDescriptorPayload {
    bytes: Vec<u8>,
}

impl VirtioBalloonPfnDescriptorPayload {
    pub fn read(
        memory: &GuestMemory,
        chain: &VirtqueueDescriptorChain,
    ) -> Result<Self, VirtioBalloonPfnDescriptorPayloadReadError> {
        let payload_len = validate_balloon_pfn_descriptor_chain(memory, chain)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(payload_len).map_err(|source| {
            VirtioBalloonPfnDescriptorPayloadReadError::PayloadAllocation {
                len: payload_len,
                source,
            }
        })?;
        bytes.resize(payload_len, 0);

        let mut offset = 0;
        for descriptor in chain.descriptors().iter().copied() {
            offset = read_balloon_pfn_descriptor_segment(memory, descriptor, &mut bytes, offset)?;
        }

        Ok(Self { bytes })
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    pub fn parse_pfn_payload(
        &self,
    ) -> Result<VirtioBalloonPfnPayload, VirtioBalloonPfnPayloadParseError> {
        VirtioBalloonPfnPayload::parse(&self.bytes)
    }

    pub fn into_pfn_payload(
        self,
    ) -> Result<VirtioBalloonPfnPayload, VirtioBalloonPfnPayloadParseError> {
        VirtioBalloonPfnPayload::parse(&self.bytes)
    }
}

#[derive(Debug)]
pub enum VirtioBalloonPfnDescriptorPayloadReadError {
    EmptyDescriptorChain,
    DescriptorWriteOnly {
        index: u16,
    },
    DescriptorEmpty {
        index: u16,
    },
    DescriptorLengthTooLarge {
        index: u16,
        len: u32,
    },
    DescriptorRange {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryError,
    },
    DescriptorAccess {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
    PayloadLengthOverflow {
        current: usize,
        len: u32,
    },
    PayloadLengthTooLarge {
        len: usize,
        max: usize,
    },
    PayloadAllocation {
        len: usize,
        source: TryReserveError,
    },
    PayloadBufferRange {
        offset: usize,
        len: usize,
        buffer_len: usize,
    },
    DescriptorRead {
        index: u16,
        address: GuestAddress,
        len: u32,
        source: GuestMemoryAccessError,
    },
}

impl fmt::Display for VirtioBalloonPfnDescriptorPayloadReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDescriptorChain => {
                f.write_str("virtio-balloon PFN descriptor chain cannot be empty")
            }
            Self::DescriptorWriteOnly { index } => {
                write!(f, "virtio-balloon PFN descriptor {index} is write-only")
            }
            Self::DescriptorEmpty { index } => {
                write!(f, "virtio-balloon PFN descriptor {index} is empty")
            }
            Self::DescriptorLengthTooLarge { index, len } => write!(
                f,
                "virtio-balloon PFN descriptor {index} length {len} is too large to represent"
            ),
            Self::DescriptorRange {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "virtio-balloon PFN descriptor {index} range address={address}, len={len} is invalid: {source}"
            ),
            Self::DescriptorAccess {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "virtio-balloon PFN descriptor {index} range address={address}, len={len} is not readable: {source}"
            ),
            Self::PayloadLengthOverflow { current, len } => write!(
                f,
                "virtio-balloon PFN descriptor payload length overflows: current={current}, len={len}"
            ),
            Self::PayloadLengthTooLarge { len, max } => write!(
                f,
                "virtio-balloon PFN descriptor payload length {len} exceeds maximum {max}"
            ),
            Self::PayloadAllocation { len, source } => write!(
                f,
                "failed to allocate virtio-balloon PFN descriptor payload with {len} bytes: {source}"
            ),
            Self::PayloadBufferRange {
                offset,
                len,
                buffer_len,
            } => write!(
                f,
                "internal virtio-balloon PFN payload buffer range offset={offset}, len={len}, buffer_len={buffer_len} is invalid"
            ),
            Self::DescriptorRead {
                index,
                address,
                len,
                source,
            } => write!(
                f,
                "failed to read virtio-balloon PFN descriptor {index} at address={address}, len={len}: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioBalloonPfnDescriptorPayloadReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DescriptorRange { source, .. } => Some(source),
            Self::DescriptorAccess { source, .. } => Some(source),
            Self::PayloadAllocation { source, .. } => Some(source),
            Self::DescriptorRead { source, .. } => Some(source),
            Self::EmptyDescriptorChain
            | Self::DescriptorWriteOnly { .. }
            | Self::DescriptorEmpty { .. }
            | Self::DescriptorLengthTooLarge { .. }
            | Self::PayloadLengthOverflow { .. }
            | Self::PayloadLengthTooLarge { .. }
            | Self::PayloadBufferRange { .. } => None,
        }
    }
}

fn validate_balloon_pfn_descriptor_chain(
    memory: &GuestMemory,
    chain: &VirtqueueDescriptorChain,
) -> Result<usize, VirtioBalloonPfnDescriptorPayloadReadError> {
    if chain.is_empty() {
        return Err(VirtioBalloonPfnDescriptorPayloadReadError::EmptyDescriptorChain);
    }

    let mut payload_len: usize = 0;
    for descriptor in chain.descriptors().iter().copied() {
        validate_balloon_pfn_descriptor_header(descriptor)?;
        let segment_len = balloon_pfn_descriptor_len(descriptor)?;
        payload_len = payload_len.checked_add(segment_len).ok_or(
            VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthOverflow {
                current: payload_len,
                len: descriptor.len(),
            },
        )?;
        if payload_len > VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE {
            return Err(
                VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthTooLarge {
                    len: payload_len,
                    max: VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE,
                },
            );
        }
        validate_balloon_pfn_descriptor_range(memory, descriptor)?;
    }

    Ok(payload_len)
}

fn validate_balloon_pfn_descriptor_header(
    descriptor: VirtqueueDescriptor,
) -> Result<(), VirtioBalloonPfnDescriptorPayloadReadError> {
    if descriptor.is_write_only() {
        return Err(
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorWriteOnly {
                index: descriptor.index(),
            },
        );
    }
    if descriptor.is_empty() {
        return Err(
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorEmpty {
                index: descriptor.index(),
            },
        );
    }

    Ok(())
}

fn validate_balloon_pfn_descriptor_range(
    memory: &GuestMemory,
    descriptor: VirtqueueDescriptor,
) -> Result<(), VirtioBalloonPfnDescriptorPayloadReadError> {
    let range = GuestMemoryRange::new(descriptor.address(), u64::from(descriptor.len())).map_err(
        |source| VirtioBalloonPfnDescriptorPayloadReadError::DescriptorRange {
            index: descriptor.index(),
            address: descriptor.address(),
            len: descriptor.len(),
            source,
        },
    )?;
    memory.validate_mapped_range(range).map_err(|source| {
        VirtioBalloonPfnDescriptorPayloadReadError::DescriptorAccess {
            index: descriptor.index(),
            address: descriptor.address(),
            len: descriptor.len(),
            source,
        }
    })
}

fn balloon_pfn_descriptor_len(
    descriptor: VirtqueueDescriptor,
) -> Result<usize, VirtioBalloonPfnDescriptorPayloadReadError> {
    usize::try_from(descriptor.len()).map_err(|_| {
        VirtioBalloonPfnDescriptorPayloadReadError::DescriptorLengthTooLarge {
            index: descriptor.index(),
            len: descriptor.len(),
        }
    })
}

fn read_balloon_pfn_descriptor_segment(
    memory: &GuestMemory,
    descriptor: VirtqueueDescriptor,
    bytes: &mut [u8],
    offset: usize,
) -> Result<usize, VirtioBalloonPfnDescriptorPayloadReadError> {
    let segment_len = balloon_pfn_descriptor_len(descriptor)?;
    let end = offset.checked_add(segment_len).ok_or(
        VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthOverflow {
            current: offset,
            len: descriptor.len(),
        },
    )?;
    let buffer_len = bytes.len();
    let destination = bytes.get_mut(offset..end).ok_or(
        VirtioBalloonPfnDescriptorPayloadReadError::PayloadBufferRange {
            offset,
            len: segment_len,
            buffer_len,
        },
    )?;

    memory
        .read_slice(destination, descriptor.address())
        .map_err(
            |source| VirtioBalloonPfnDescriptorPayloadReadError::DescriptorRead {
                index: descriptor.index(),
                address: descriptor.address(),
                len: descriptor.len(),
                source,
            },
        )?;

    Ok(end)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonPfnRange {
    start_pfn: u32,
    page_count: u32,
}

impl VirtioBalloonPfnRange {
    const fn new(start_pfn: u32, page_count: u32) -> Self {
        Self {
            start_pfn,
            page_count,
        }
    }

    pub const fn start_pfn(self) -> u32 {
        self.start_pfn
    }

    pub const fn page_count(self) -> u32 {
        self.page_count
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonPfnRanges {
    ranges: Vec<VirtioBalloonPfnRange>,
}

impl VirtioBalloonPfnRanges {
    pub fn from_pfns(mut pfns: Vec<u32>) -> Result<Self, VirtioBalloonPfnRangeCompactError> {
        let pfn_count = pfns.len();
        if pfn_count > VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR {
            return Err(VirtioBalloonPfnRangeCompactError::TooManyPfns {
                count: pfn_count,
                max: VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR,
            });
        }
        if pfns.is_empty() {
            return Ok(Self { ranges: Vec::new() });
        }

        pfns.sort_unstable();

        let mut ranges = Vec::new();
        ranges.try_reserve_exact(pfn_count).map_err(|source| {
            VirtioBalloonPfnRangeCompactError::RangeAllocation { pfn_count, source }
        })?;

        let mut iter = pfns.into_iter();
        let Some(mut start_pfn) = iter.next() else {
            return Ok(Self { ranges });
        };
        let mut previous_pfn = start_pfn;
        let mut page_count = 1;

        for pfn in iter {
            if pfn == previous_pfn {
                continue;
            }

            if previous_pfn.checked_add(1) == Some(pfn) {
                page_count += 1;
            } else {
                ranges.push(VirtioBalloonPfnRange::new(start_pfn, page_count));
                start_pfn = pfn;
                page_count = 1;
            }
            previous_pfn = pfn;
        }

        ranges.push(VirtioBalloonPfnRange::new(start_pfn, page_count));

        Ok(Self { ranges })
    }

    pub fn ranges(&self) -> &[VirtioBalloonPfnRange] {
        &self.ranges
    }

    pub fn into_vec(self) -> Vec<VirtioBalloonPfnRange> {
        self.ranges
    }

    pub fn len(&self) -> usize {
        self.ranges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

#[derive(Debug)]
pub enum VirtioBalloonPfnRangeCompactError {
    TooManyPfns {
        count: usize,
        max: usize,
    },
    RangeAllocation {
        pfn_count: usize,
        source: TryReserveError,
    },
}

impl fmt::Display for VirtioBalloonPfnRangeCompactError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyPfns { count, max } => write!(
                f,
                "virtio-balloon PFN compaction input contains {count} PFNs, exceeding maximum {max}"
            ),
            Self::RangeAllocation { pfn_count, source } => write!(
                f,
                "failed to allocate virtio-balloon PFN ranges for {pfn_count} PFNs: {source}"
            ),
        }
    }
}

impl std::error::Error for VirtioBalloonPfnRangeCompactError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::RangeAllocation { source, .. } => Some(source),
            Self::TooManyPfns { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtioBalloonQueueKind {
    Inflate,
    Deflate,
    Statistics,
    FreePageHinting,
    FreePageReporting,
}

impl fmt::Display for VirtioBalloonQueueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inflate => f.write_str("inflate"),
            Self::Deflate => f.write_str("deflate"),
            Self::Statistics => f.write_str("statistics"),
            Self::FreePageHinting => f.write_str("free-page-hinting"),
            Self::FreePageReporting => f.write_str("free-page-reporting"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonQueueConfig {
    kind: VirtioBalloonQueueKind,
    index: usize,
    size: u16,
}

impl VirtioBalloonQueueConfig {
    pub const fn new(kind: VirtioBalloonQueueKind, index: usize, size: u16) -> Self {
        Self { kind, index, size }
    }

    pub const fn kind(self) -> VirtioBalloonQueueKind {
        self.kind
    }

    pub const fn index(self) -> usize {
        self.index
    }

    pub const fn size(self) -> u16 {
        self.size
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonQueueLayout {
    inflate: VirtioBalloonQueueConfig,
    deflate: VirtioBalloonQueueConfig,
    statistics: Option<VirtioBalloonQueueConfig>,
    free_page_hinting: Option<VirtioBalloonQueueConfig>,
    free_page_reporting: Option<VirtioBalloonQueueConfig>,
}

impl VirtioBalloonQueueLayout {
    pub fn from_config(config: BalloonConfig) -> Self {
        let inflate = VirtioBalloonQueueConfig::new(
            VirtioBalloonQueueKind::Inflate,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
            VIRTIO_BALLOON_QUEUE_SIZE,
        );
        let deflate = VirtioBalloonQueueConfig::new(
            VirtioBalloonQueueKind::Deflate,
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
            VIRTIO_BALLOON_QUEUE_SIZE,
        );
        let mut next_queue_index = VIRTIO_BALLOON_STATS_QUEUE_INDEX;

        let statistics = if config.stats_polling_interval_s() > 0 {
            let queue = VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::Statistics,
                next_queue_index,
                VIRTIO_BALLOON_QUEUE_SIZE,
            );
            next_queue_index += 1;
            Some(queue)
        } else {
            None
        };

        let free_page_hinting = if config.free_page_hinting() {
            let queue = VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageHinting,
                next_queue_index,
                VIRTIO_BALLOON_QUEUE_SIZE,
            );
            next_queue_index += 1;
            Some(queue)
        } else {
            None
        };

        let free_page_reporting = if config.free_page_reporting() {
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageReporting,
                next_queue_index,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        } else {
            None
        };

        Self {
            inflate,
            deflate,
            statistics,
            free_page_hinting,
            free_page_reporting,
        }
    }

    pub const fn inflate(self) -> VirtioBalloonQueueConfig {
        self.inflate
    }

    pub const fn deflate(self) -> VirtioBalloonQueueConfig {
        self.deflate
    }

    pub const fn statistics(self) -> Option<VirtioBalloonQueueConfig> {
        self.statistics
    }

    pub const fn free_page_hinting(self) -> Option<VirtioBalloonQueueConfig> {
        self.free_page_hinting
    }

    pub const fn free_page_reporting(self) -> Option<VirtioBalloonQueueConfig> {
        self.free_page_reporting
    }

    pub fn queue_count(self) -> usize {
        self.iter().count()
    }

    pub fn queue_sizes(self) -> VirtioBalloonQueueSizes {
        VirtioBalloonQueueSizes::from_layout(self)
    }

    pub fn iter(self) -> impl Iterator<Item = VirtioBalloonQueueConfig> {
        [
            Some(self.inflate),
            Some(self.deflate),
            self.statistics,
            self.free_page_hinting,
            self.free_page_reporting,
        ]
        .into_iter()
        .flatten()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtioBalloonQueueSizes {
    sizes: [u16; VIRTIO_BALLOON_MAX_QUEUE_COUNT],
    len: usize,
}

impl VirtioBalloonQueueSizes {
    pub fn from_layout(layout: VirtioBalloonQueueLayout) -> Self {
        let mut sizes = [0; VIRTIO_BALLOON_MAX_QUEUE_COUNT];
        let mut len = 0;
        for queue in layout.iter() {
            if let Some(size) = sizes.get_mut(len) {
                *size = queue.size();
                len += 1;
            }
        }

        Self { sizes, len }
    }

    pub const fn len(self) -> usize {
        self.len
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub fn as_slice(&self) -> &[u16] {
        let (used, _) = self.sizes.split_at(self.len);
        used
    }
}

#[derive(Debug)]
pub enum VirtioBalloonQueueBuildError {
    QueueNotReady,
    AvailableRing { source: VirtqueueAvailableRingError },
    UsedRing { source: VirtqueueUsedRingError },
}

impl fmt::Display for VirtioBalloonQueueBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueNotReady => f.write_str("virtio-balloon queue is not ready"),
            Self::AvailableRing { source } => {
                write!(
                    f,
                    "failed to build virtio-balloon available ring from queue state: {source}"
                )
            }
            Self::UsedRing { source } => {
                write!(
                    f,
                    "failed to build virtio-balloon used ring from queue state: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonQueueBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source } => Some(source),
            Self::UsedRing { source } => Some(source),
            Self::QueueNotReady => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonQueue {
    available: VirtqueueAvailableRing,
    used: VirtqueueUsedRing,
}

impl VirtioBalloonQueue {
    pub const fn new(available: VirtqueueAvailableRing, used: VirtqueueUsedRing) -> Self {
        Self { available, used }
    }

    pub fn from_mmio_queue_state(
        queue: &VirtioMmioQueueState,
    ) -> Result<Self, VirtioBalloonQueueBuildError> {
        if !queue.ready() {
            return Err(VirtioBalloonQueueBuildError::QueueNotReady);
        }

        let available = VirtqueueAvailableRing::new(
            queue.descriptor_table(),
            queue.driver_ring(),
            queue.size(),
        )
        .map_err(|source| VirtioBalloonQueueBuildError::AvailableRing { source })?;
        let used = VirtqueueUsedRing::new(queue.device_ring(), queue.size())
            .map_err(|source| VirtioBalloonQueueBuildError::UsedRing { source })?;

        Ok(Self { available, used })
    }

    pub const fn available_ring(&self) -> &VirtqueueAvailableRing {
        &self.available
    }

    pub const fn used_ring(&self) -> &VirtqueueUsedRing {
        &self.used
    }

    pub fn dispatch_deflate(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut dispatch = VirtioBalloonQueueDispatch::default();

        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioBalloonQueueDispatchError::AvailableRing {
                queue: VirtioBalloonQueueKind::Deflate,
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioBalloonQueueDispatchError::EmptyDescriptorChain {
                    queue: VirtioBalloonQueueKind::Deflate,
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;
            let publication = self
                .used
                .publish_used_element_with_notification(
                    memory,
                    descriptor_head,
                    0,
                    VirtqueueNotificationSuppression::Disabled,
                )
                .map_err(|source| VirtioBalloonQueueDispatchError::UsedRing {
                    queue: VirtioBalloonQueueKind::Deflate,
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head,
                    source,
                })?;
            dispatch.record_deflate_descriptor(publication);
        }

        Ok(dispatch)
    }

    pub fn dispatch_inflate(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBalloonQueueDispatch, VirtioBalloonQueueDispatchError> {
        let mut dispatch = VirtioBalloonQueueDispatch::default();

        while let Some(chain) = self
            .available
            .pop_descriptor_chain(memory)
            .map_err(|source| VirtioBalloonQueueDispatchError::AvailableRing {
                queue: VirtioBalloonQueueKind::Inflate,
                completed_dispatch: Box::new(dispatch.clone()),
                source,
            })?
        {
            let descriptor_head = descriptor_chain_head(&chain).ok_or_else(|| {
                VirtioBalloonQueueDispatchError::EmptyDescriptorChain {
                    queue: VirtioBalloonQueueKind::Inflate,
                    completed_dispatch: Box::new(dispatch.clone()),
                }
            })?;
            let payload =
                VirtioBalloonPfnDescriptorPayload::read(memory, &chain).map_err(|source| {
                    VirtioBalloonQueueDispatchError::PfnDescriptorRead {
                        completed_dispatch: Box::new(dispatch.clone()),
                        descriptor_head,
                        source,
                    }
                })?;
            let pfn_payload = payload.into_pfn_payload().map_err(|source| {
                VirtioBalloonQueueDispatchError::PfnPayloadParse {
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head,
                    source,
                }
            })?;
            let pfn_ranges = pfn_payload.into_page_ranges().map_err(|source| {
                VirtioBalloonQueueDispatchError::PfnRangeCompact {
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head,
                    source,
                }
            })?;
            let range_count = pfn_ranges.len();
            dispatch
                .reserve_inflated_page_ranges(range_count)
                .map_err(
                    |source| VirtioBalloonQueueDispatchError::InflatedRangeAllocation {
                        completed_dispatch: Box::new(dispatch.clone()),
                        descriptor_head,
                        range_count,
                        source,
                    },
                )?;
            let publication = self
                .used
                .publish_used_element_with_notification(
                    memory,
                    descriptor_head,
                    0,
                    VirtqueueNotificationSuppression::Disabled,
                )
                .map_err(|source| VirtioBalloonQueueDispatchError::UsedRing {
                    queue: VirtioBalloonQueueKind::Inflate,
                    completed_dispatch: Box::new(dispatch.clone()),
                    descriptor_head,
                    source,
                })?;
            dispatch.record_inflate_descriptor(pfn_ranges.ranges(), publication);
        }

        Ok(dispatch)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VirtioBalloonQueueDispatch {
    completed_descriptors: usize,
    needs_queue_interrupt: bool,
    inflated_page_ranges: Vec<VirtioBalloonPfnRange>,
}

impl VirtioBalloonQueueDispatch {
    pub const fn completed_descriptors(&self) -> usize {
        self.completed_descriptors
    }

    pub const fn needs_queue_interrupt(&self) -> bool {
        self.needs_queue_interrupt
    }

    pub fn inflated_page_ranges(&self) -> &[VirtioBalloonPfnRange] {
        &self.inflated_page_ranges
    }

    fn record_deflate_descriptor(&mut self, publication: VirtqueueUsedRingPublication) {
        self.completed_descriptors += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
    }

    fn reserve_inflated_page_ranges(&mut self, range_count: usize) -> Result<(), TryReserveError> {
        self.inflated_page_ranges.try_reserve(range_count)
    }

    fn record_inflate_descriptor(
        &mut self,
        ranges: &[VirtioBalloonPfnRange],
        publication: VirtqueueUsedRingPublication,
    ) {
        self.completed_descriptors += 1;
        self.needs_queue_interrupt |= publication.needs_queue_interrupt();
        self.inflated_page_ranges.extend_from_slice(ranges);
    }
}

#[derive(Debug)]
pub enum VirtioBalloonQueueDispatchError {
    AvailableRing {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        source: VirtqueueAvailableRingError,
    },
    EmptyDescriptorChain {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
    },
    UsedRing {
        queue: VirtioBalloonQueueKind,
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtqueueUsedRingError,
    },
    PfnDescriptorRead {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtioBalloonPfnDescriptorPayloadReadError,
    },
    PfnPayloadParse {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtioBalloonPfnPayloadParseError,
    },
    PfnRangeCompact {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        source: VirtioBalloonPfnRangeCompactError,
    },
    InflatedRangeAllocation {
        completed_dispatch: Box<VirtioBalloonQueueDispatch>,
        descriptor_head: u16,
        range_count: usize,
        source: TryReserveError,
    },
}

impl VirtioBalloonQueueDispatchError {
    pub const fn completed_dispatch(&self) -> &VirtioBalloonQueueDispatch {
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
            | Self::PfnDescriptorRead {
                completed_dispatch, ..
            }
            | Self::PfnPayloadParse {
                completed_dispatch, ..
            }
            | Self::PfnRangeCompact {
                completed_dispatch, ..
            }
            | Self::InflatedRangeAllocation {
                completed_dispatch, ..
            } => completed_dispatch,
        }
    }
}

impl fmt::Display for VirtioBalloonQueueDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AvailableRing { queue, source, .. } => {
                write!(
                    f,
                    "failed to pop virtio-balloon {queue} descriptor chain: {source}"
                )
            }
            Self::EmptyDescriptorChain { queue, .. } => {
                write!(f, "virtio-balloon {queue} descriptor chain cannot be empty")
            }
            Self::UsedRing {
                queue,
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to publish virtio-balloon {queue} descriptor {descriptor_head}: {source}"
                )
            }
            Self::PfnDescriptorRead {
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to read virtio-balloon inflate descriptor {descriptor_head}: {source}"
                )
            }
            Self::PfnPayloadParse {
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to parse virtio-balloon inflate descriptor {descriptor_head} PFNs: {source}"
                )
            }
            Self::PfnRangeCompact {
                descriptor_head,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to compact virtio-balloon inflate descriptor {descriptor_head} PFNs: {source}"
                )
            }
            Self::InflatedRangeAllocation {
                descriptor_head,
                range_count,
                source,
                ..
            } => {
                write!(
                    f,
                    "failed to reserve {range_count} inflated page range(s) for virtio-balloon inflate descriptor {descriptor_head}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonQueueDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::AvailableRing { source, .. } => Some(source),
            Self::UsedRing { source, .. } => Some(source),
            Self::PfnDescriptorRead { source, .. } => Some(source),
            Self::PfnPayloadParse { source, .. } => Some(source),
            Self::PfnRangeCompact { source, .. } => Some(source),
            Self::InflatedRangeAllocation { source, .. } => Some(source),
            Self::EmptyDescriptorChain { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioBalloonActiveQueues {
    inflate: VirtioBalloonQueue,
    deflate: VirtioBalloonQueue,
    statistics: Option<VirtioBalloonQueue>,
    free_page_hinting: Option<VirtioBalloonQueue>,
    free_page_reporting: Option<VirtioBalloonQueue>,
}

impl VirtioBalloonActiveQueues {
    fn from_activation(
        layout: VirtioBalloonQueueLayout,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<Self, VirtioBalloonDeviceActivationError> {
        Ok(Self {
            inflate: active_queue_from_activation(layout.inflate(), activation)?,
            deflate: active_queue_from_activation(layout.deflate(), activation)?,
            statistics: active_optional_queue_from_activation(layout.statistics(), activation)?,
            free_page_hinting: active_optional_queue_from_activation(
                layout.free_page_hinting(),
                activation,
            )?,
            free_page_reporting: active_optional_queue_from_activation(
                layout.free_page_reporting(),
                activation,
            )?,
        })
    }

    pub const fn inflate(&self) -> &VirtioBalloonQueue {
        &self.inflate
    }

    pub fn inflate_mut(&mut self) -> &mut VirtioBalloonQueue {
        &mut self.inflate
    }

    pub const fn deflate(&self) -> &VirtioBalloonQueue {
        &self.deflate
    }

    pub fn deflate_mut(&mut self) -> &mut VirtioBalloonQueue {
        &mut self.deflate
    }

    pub const fn statistics(&self) -> Option<&VirtioBalloonQueue> {
        self.statistics.as_ref()
    }

    pub const fn free_page_hinting(&self) -> Option<&VirtioBalloonQueue> {
        self.free_page_hinting.as_ref()
    }

    pub const fn free_page_reporting(&self) -> Option<&VirtioBalloonQueue> {
        self.free_page_reporting.as_ref()
    }

    pub fn queue_count(&self) -> usize {
        [
            true,
            true,
            self.statistics.is_some(),
            self.free_page_hinting.is_some(),
            self.free_page_reporting.is_some(),
        ]
        .into_iter()
        .filter(|included| *included)
        .count()
    }
}

#[derive(Debug)]
pub struct VirtioBalloonDevice {
    queue_layout: VirtioBalloonQueueLayout,
    active_queues: Option<VirtioBalloonActiveQueues>,
}

impl VirtioBalloonDevice {
    pub const fn new(queue_layout: VirtioBalloonQueueLayout) -> Self {
        Self {
            queue_layout,
            active_queues: None,
        }
    }

    pub const fn queue_layout(&self) -> VirtioBalloonQueueLayout {
        self.queue_layout
    }

    pub fn is_activated(&self) -> bool {
        self.active_queues.is_some()
    }

    pub const fn active_queues(&self) -> Option<&VirtioBalloonActiveQueues> {
        self.active_queues.as_ref()
    }

    pub fn active_queues_mut(&mut self) -> Option<&mut VirtioBalloonActiveQueues> {
        self.active_queues.as_mut()
    }

    pub fn activate_balloon(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioBalloonDeviceActivationError> {
        if self.active_queues.is_some() {
            return Err(VirtioBalloonDeviceActivationError::AlreadyActive);
        }

        let expected = self.queue_layout.queue_count();
        let actual = activation.queue_count();
        if actual != expected {
            return Err(VirtioBalloonDeviceActivationError::QueueCountMismatch {
                expected,
                actual,
            });
        }

        self.active_queues = Some(VirtioBalloonActiveQueues::from_activation(
            self.queue_layout,
            activation,
        )?);

        Ok(())
    }

    fn dispatch_drained_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
        drained_notifications: Vec<usize>,
    ) -> Result<VirtioBalloonDeviceNotificationDispatch, VirtioBalloonDeviceNotificationError> {
        if drained_notifications.is_empty() {
            return Ok(VirtioBalloonDeviceNotificationDispatch::new(
                drained_notifications,
                0,
                0,
                None,
                None,
            ));
        }

        if let Some(queue_index) = drained_notifications
            .iter()
            .copied()
            .find(|queue_index| !is_inflate_or_deflate_queue(*queue_index))
        {
            return Err(VirtioBalloonDeviceNotificationError::UnsupportedQueue {
                drained_notifications,
                queue_index,
            });
        }

        let Some(active_queues) = self.active_queues.as_mut() else {
            return Err(VirtioBalloonDeviceNotificationError::Inactive {
                drained_notifications,
            });
        };

        let mut inflate_notifications = 0;
        let mut deflate_notifications = 0;
        for queue_index in &drained_notifications {
            match *queue_index {
                VIRTIO_BALLOON_INFLATE_QUEUE_INDEX => inflate_notifications += 1,
                VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX => deflate_notifications += 1,
                _ => {}
            }
        }
        let mut dispatch = VirtioBalloonDeviceNotificationDispatch::new(
            drained_notifications,
            inflate_notifications,
            deflate_notifications,
            None,
            None,
        );

        if inflate_notifications > 0 {
            match active_queues.inflate_mut().dispatch_inflate(memory) {
                Ok(inflate_dispatch) => {
                    dispatch.inflate_queue_dispatch = Some(inflate_dispatch);
                }
                Err(source) => {
                    dispatch.inflate_queue_dispatch = Some(source.completed_dispatch().clone());
                    return Err(VirtioBalloonDeviceNotificationError::QueueDispatch {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            }
        }

        if deflate_notifications > 0 {
            match active_queues.deflate_mut().dispatch_deflate(memory) {
                Ok(deflate_dispatch) => {
                    dispatch.deflate_queue_dispatch = Some(deflate_dispatch);
                }
                Err(source) => {
                    dispatch.deflate_queue_dispatch = Some(source.completed_dispatch().clone());
                    return Err(VirtioBalloonDeviceNotificationError::QueueDispatch {
                        completed_dispatch: Box::new(dispatch),
                        source,
                    });
                }
            }
        }

        Ok(dispatch)
    }

    pub fn reset(&mut self) {
        self.active_queues = None;
    }
}

impl VirtioMmioRegisterHandler<VirtioBalloonConfigSpace, VirtioBalloonDevice> {
    pub fn dispatch_balloon_queue_notifications(
        &mut self,
        memory: &mut GuestMemory,
    ) -> Result<VirtioBalloonDeviceNotificationDispatch, VirtioBalloonDeviceNotificationError> {
        let drained_notifications = self.take_pending_queue_notifications();
        let dispatch = self
            .activation_handler_mut()
            .dispatch_drained_queue_notifications(memory, drained_notifications);
        let needs_queue_interrupt = match &dispatch {
            Ok(dispatch) => dispatch.needs_queue_interrupt(),
            Err(error) => error
                .completed_notification_dispatch()
                .is_some_and(VirtioBalloonDeviceNotificationDispatch::needs_queue_interrupt),
        };
        if needs_queue_interrupt {
            self.mark_interrupt_pending(DeviceInterruptKind::Queue);
        }

        dispatch
    }
}

impl VirtioMmioDeviceActivationHandler for VirtioBalloonDevice {
    fn activate(
        &mut self,
        activation: VirtioMmioDeviceActivation<'_>,
    ) -> Result<(), VirtioMmioDeviceActivationError> {
        self.activate_balloon(activation).map_err(Into::into)
    }

    fn reset(&mut self) {
        VirtioBalloonDevice::reset(self);
    }
}

#[derive(Debug)]
pub enum VirtioBalloonDeviceActivationError {
    AlreadyActive,
    QueueCountMismatch {
        expected: usize,
        actual: usize,
    },
    QueueIndexTooLarge {
        queue_index: usize,
    },
    QueueMetadata {
        queue_index: u32,
        source: VirtioMmioQueueRegisterError,
    },
    QueueBuild {
        queue_index: u32,
        kind: VirtioBalloonQueueKind,
        source: VirtioBalloonQueueBuildError,
    },
}

impl fmt::Display for VirtioBalloonDeviceActivationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyActive => f.write_str("virtio-balloon device is already active"),
            Self::QueueCountMismatch { expected, actual } => {
                write!(
                    f,
                    "virtio-balloon device requires {expected} queue(s), got {actual}"
                )
            }
            Self::QueueIndexTooLarge { queue_index } => {
                write!(
                    f,
                    "virtio-balloon queue index {queue_index} does not fit a virtio-mmio queue selector"
                )
            }
            Self::QueueMetadata {
                queue_index,
                source,
            } => {
                write!(
                    f,
                    "failed to read virtio-balloon queue {queue_index} activation metadata: {source}"
                )
            }
            Self::QueueBuild {
                queue_index,
                kind,
                source,
            } => {
                write!(
                    f,
                    "failed to activate virtio-balloon {kind} queue {queue_index}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonDeviceActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueMetadata { source, .. } => Some(source),
            Self::QueueBuild { source, .. } => Some(source),
            Self::AlreadyActive
            | Self::QueueCountMismatch { .. }
            | Self::QueueIndexTooLarge { .. } => None,
        }
    }
}

impl From<VirtioBalloonDeviceActivationError> for VirtioMmioDeviceActivationError {
    fn from(source: VirtioBalloonDeviceActivationError) -> Self {
        MmioHandlerError::new(source.to_string()).into()
    }
}

#[derive(Debug)]
pub struct VirtioBalloonDeviceNotificationDispatch {
    drained_notifications: Vec<usize>,
    inflate_notifications: usize,
    deflate_notifications: usize,
    inflate_queue_dispatch: Option<VirtioBalloonQueueDispatch>,
    deflate_queue_dispatch: Option<VirtioBalloonQueueDispatch>,
}

impl VirtioBalloonDeviceNotificationDispatch {
    const fn new(
        drained_notifications: Vec<usize>,
        inflate_notifications: usize,
        deflate_notifications: usize,
        inflate_queue_dispatch: Option<VirtioBalloonQueueDispatch>,
        deflate_queue_dispatch: Option<VirtioBalloonQueueDispatch>,
    ) -> Self {
        Self {
            drained_notifications,
            inflate_notifications,
            deflate_notifications,
            inflate_queue_dispatch,
            deflate_queue_dispatch,
        }
    }

    pub fn drained_notifications(&self) -> &[usize] {
        &self.drained_notifications
    }

    pub const fn inflate_notifications(&self) -> usize {
        self.inflate_notifications
    }

    pub const fn deflate_notifications(&self) -> usize {
        self.deflate_notifications
    }

    pub const fn inflate_queue_dispatch(&self) -> Option<&VirtioBalloonQueueDispatch> {
        self.inflate_queue_dispatch.as_ref()
    }

    pub const fn deflate_queue_dispatch(&self) -> Option<&VirtioBalloonQueueDispatch> {
        self.deflate_queue_dispatch.as_ref()
    }

    pub fn needs_queue_interrupt(&self) -> bool {
        self.inflate_queue_dispatch
            .as_ref()
            .is_some_and(VirtioBalloonQueueDispatch::needs_queue_interrupt)
            || self
                .deflate_queue_dispatch
                .as_ref()
                .is_some_and(VirtioBalloonQueueDispatch::needs_queue_interrupt)
    }
}

#[derive(Debug)]
pub enum VirtioBalloonDeviceNotificationError {
    Inactive {
        drained_notifications: Vec<usize>,
    },
    UnsupportedQueue {
        drained_notifications: Vec<usize>,
        queue_index: usize,
    },
    QueueDispatch {
        completed_dispatch: Box<VirtioBalloonDeviceNotificationDispatch>,
        source: VirtioBalloonQueueDispatchError,
    },
}

impl VirtioBalloonDeviceNotificationError {
    pub fn drained_notifications(&self) -> &[usize] {
        match self {
            Self::Inactive {
                drained_notifications,
            }
            | Self::UnsupportedQueue {
                drained_notifications,
                ..
            } => drained_notifications,
            Self::QueueDispatch {
                completed_dispatch, ..
            } => completed_dispatch.drained_notifications(),
        }
    }

    pub const fn completed_dispatch(&self) -> Option<&VirtioBalloonQueueDispatch> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source.completed_dispatch()),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }

    pub const fn completed_notification_dispatch(
        &self,
    ) -> Option<&VirtioBalloonDeviceNotificationDispatch> {
        match self {
            Self::QueueDispatch {
                completed_dispatch, ..
            } => Some(completed_dispatch),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

impl fmt::Display for VirtioBalloonDeviceNotificationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive { .. } => f.write_str(
                "virtio-balloon queue notification cannot be dispatched before activation",
            ),
            Self::UnsupportedQueue { queue_index, .. } => {
                write!(
                    f,
                    "virtio-balloon queue notification for unsupported queue {queue_index}"
                )
            }
            Self::QueueDispatch { source, .. } => {
                write!(
                    f,
                    "failed to dispatch virtio-balloon queue notification: {source}"
                )
            }
        }
    }
}

impl std::error::Error for VirtioBalloonDeviceNotificationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::QueueDispatch { source, .. } => Some(source),
            Self::Inactive { .. } | Self::UnsupportedQueue { .. } => None,
        }
    }
}

fn active_optional_queue_from_activation(
    config: Option<VirtioBalloonQueueConfig>,
    activation: VirtioMmioDeviceActivation<'_>,
) -> Result<Option<VirtioBalloonQueue>, VirtioBalloonDeviceActivationError> {
    config
        .map(|config| active_queue_from_activation(config, activation))
        .transpose()
}

fn active_queue_from_activation(
    config: VirtioBalloonQueueConfig,
    activation: VirtioMmioDeviceActivation<'_>,
) -> Result<VirtioBalloonQueue, VirtioBalloonDeviceActivationError> {
    let queue_index = u32::try_from(config.index()).map_err(|_| {
        VirtioBalloonDeviceActivationError::QueueIndexTooLarge {
            queue_index: config.index(),
        }
    })?;
    let queue = activation.queue(queue_index).map_err(|source| {
        VirtioBalloonDeviceActivationError::QueueMetadata {
            queue_index,
            source,
        }
    })?;
    VirtioBalloonQueue::from_mmio_queue_state(queue).map_err(|source| {
        VirtioBalloonDeviceActivationError::QueueBuild {
            queue_index,
            kind: config.kind(),
            source,
        }
    })
}

fn descriptor_chain_head(chain: &VirtqueueDescriptorChain) -> Option<u16> {
    chain
        .descriptors()
        .first()
        .copied()
        .map(VirtqueueDescriptor::index)
}

const fn is_inflate_or_deflate_queue(queue_index: usize) -> bool {
    queue_index == VIRTIO_BALLOON_INFLATE_QUEUE_INDEX
        || queue_index == VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedBalloonDevice {
    config_space: VirtioBalloonConfigSpace,
    available_features: u64,
    queue_layout: VirtioBalloonQueueLayout,
}

impl PreparedBalloonDevice {
    pub fn from_config(config: BalloonConfig) -> Result<Self, BalloonPageCountOverflow> {
        Ok(Self {
            config_space: VirtioBalloonConfigSpace::from_config(config)?,
            available_features: available_features(config),
            queue_layout: VirtioBalloonQueueLayout::from_config(config),
        })
    }

    pub const fn config_space(self) -> VirtioBalloonConfigSpace {
        self.config_space
    }

    pub const fn available_features(self) -> u64 {
        self.available_features
    }

    pub const fn queue_layout(self) -> VirtioBalloonQueueLayout {
        self.queue_layout
    }

    pub fn queue_sizes(self) -> VirtioBalloonQueueSizes {
        self.queue_layout.queue_sizes()
    }

    pub fn register_mmio(
        self,
        layout: BalloonMmioLayout,
    ) -> Result<BalloonMmioDevice, BalloonMmioRegistrationError> {
        BalloonMmioDevice::from_prepared(self, layout)
    }

    pub fn register_mmio_with_dispatcher(
        self,
        layout: BalloonMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<BalloonMmioDevice, BalloonMmioRegistrationError> {
        BalloonMmioDevice::from_prepared_with_dispatcher(self, layout, dispatcher)
    }
}

impl TryFrom<BalloonConfig> for PreparedBalloonDevice {
    type Error = BalloonPageCountOverflow;

    fn try_from(config: BalloonConfig) -> Result<Self, Self::Error> {
        Self::from_config(config)
    }
}

pub const fn available_features(config: BalloonConfig) -> u64 {
    let mut features = virtio_feature_bit(VIRTIO_FEATURE_VERSION_1);
    if config.deflate_on_oom() {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_DEFLATE_ON_OOM);
    }
    if config.stats_polling_interval_s() > 0 {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_STATS_VQ);
    }
    if config.free_page_hinting() {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_FREE_PAGE_HINTING);
    }
    if config.free_page_reporting() {
        features |= virtio_feature_bit(VIRTIO_BALLOON_F_FREE_PAGE_REPORTING);
    }

    features
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonMmioLayout {
    address: GuestAddress,
    region_id: MmioRegionId,
}

impl BalloonMmioLayout {
    pub const fn new(address: GuestAddress, region_id: MmioRegionId) -> Self {
        Self { address, region_id }
    }

    pub const fn address(self) -> GuestAddress {
        self.address
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region_id
    }

    fn region(self) -> Result<MmioRegion, BalloonMmioRegistrationError> {
        MmioRegion::new(self.region_id, self.address, VIRTIO_MMIO_DEVICE_WINDOW_SIZE).map_err(
            |source| BalloonMmioRegistrationError::InvalidRegion {
                region_id: self.region_id,
                address: self.address,
                source,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BalloonMmioDeviceRegistration {
    region: MmioRegion,
}

impl BalloonMmioDeviceRegistration {
    pub const fn region(self) -> MmioRegion {
        self.region
    }

    pub const fn region_id(self) -> MmioRegionId {
        self.region.id()
    }

    pub const fn address(self) -> GuestAddress {
        self.region.range().start()
    }
}

#[derive(Debug)]
pub struct BalloonMmioDevice {
    dispatcher: MmioDispatcher,
    registration: BalloonMmioDeviceRegistration,
}

impl BalloonMmioDevice {
    pub fn from_prepared(
        prepared: PreparedBalloonDevice,
        layout: BalloonMmioLayout,
    ) -> Result<Self, BalloonMmioRegistrationError> {
        Self::from_prepared_with_dispatcher(prepared, layout, MmioDispatcher::new())
    }

    pub fn from_prepared_with_dispatcher(
        prepared: PreparedBalloonDevice,
        layout: BalloonMmioLayout,
        dispatcher: MmioDispatcher,
    ) -> Result<Self, BalloonMmioRegistrationError> {
        let region = layout.region()?;
        let queue_sizes = prepared.queue_sizes();
        let handler = VirtioBalloonMmioHandler::with_device_config_and_activation(
            VIRTIO_BALLOON_DEVICE_ID,
            prepared.available_features(),
            queue_sizes.as_slice(),
            prepared.config_space(),
            VirtioBalloonDevice::new(prepared.queue_layout()),
        )
        .map_err(|source| BalloonMmioRegistrationError::BuildHandler {
            region_id: layout.region_id(),
            source,
        })?;
        let mut dispatcher = dispatcher;
        let inserted_region = dispatcher
            .insert_region(
                layout.region_id(),
                layout.address(),
                VIRTIO_MMIO_DEVICE_WINDOW_SIZE,
            )
            .map_err(|source| BalloonMmioRegistrationError::InsertRegion {
                region_id: layout.region_id(),
                address: layout.address(),
                source,
            })?;
        dispatcher
            .register_handler(layout.region_id(), handler)
            .map_err(|source| BalloonMmioRegistrationError::RegisterHandler {
                region_id: layout.region_id(),
                source,
            })?;
        debug_assert_eq!(inserted_region, region);

        Ok(Self {
            dispatcher,
            registration: BalloonMmioDeviceRegistration { region },
        })
    }

    pub fn dispatcher(&self) -> &MmioDispatcher {
        &self.dispatcher
    }

    pub fn dispatcher_mut(&mut self) -> &mut MmioDispatcher {
        &mut self.dispatcher
    }

    pub const fn registration(&self) -> BalloonMmioDeviceRegistration {
        self.registration
    }

    pub fn into_parts(self) -> (MmioDispatcher, BalloonMmioDeviceRegistration) {
        (self.dispatcher, self.registration)
    }
}

#[derive(Debug)]
pub enum BalloonMmioRegistrationError {
    InvalidRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: GuestMemoryError,
    },
    BuildHandler {
        region_id: MmioRegionId,
        source: VirtioMmioRegisterHandlerError,
    },
    InsertRegion {
        region_id: MmioRegionId,
        address: GuestAddress,
        source: MmioBusError,
    },
    RegisterHandler {
        region_id: MmioRegionId,
        source: MmioDispatchError,
    },
}

impl fmt::Display for BalloonMmioRegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "invalid balloon MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::BuildHandler { region_id, source } => {
                write!(
                    f,
                    "failed to build balloon MMIO handler for region id={region_id}: {source}"
                )
            }
            Self::InsertRegion {
                region_id,
                address,
                source,
            } => {
                write!(
                    f,
                    "failed to insert balloon MMIO region id={region_id} at {address}: {source}"
                )
            }
            Self::RegisterHandler { region_id, source } => {
                write!(
                    f,
                    "failed to register balloon MMIO handler for region id={region_id}: {source}"
                )
            }
        }
    }
}

impl std::error::Error for BalloonMmioRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRegion { source, .. } => Some(source),
            Self::BuildHandler { source, .. } => Some(source),
            Self::InsertRegion { source, .. } => Some(source),
            Self::RegisterHandler { source, .. } => Some(source),
        }
    }
}

fn balloon_config_access_bytes(
    bytes: &[u8; VIRTIO_BALLOON_CONFIG_SPACE_SIZE],
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

fn balloon_config_access_bytes_mut(
    bytes: &mut [u8; VIRTIO_BALLOON_CONFIG_SPACE_SIZE],
    access: VirtioMmioDeviceConfigAccess,
) -> Result<&mut [u8], VirtioMmioDeviceConfigError> {
    let offset = usize::try_from(access.offset()).map_err(|_| {
        VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        }
    })?;
    let Some(end) = offset.checked_add(access.len()) else {
        return Err(VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        });
    };

    bytes
        .get_mut(offset..end)
        .ok_or(VirtioMmioDeviceConfigError::UnsupportedWrite {
            offset: access.offset(),
            len: access.len(),
        })
}

fn balloon_config_bytes_error(source: MmioAccessBytesError) -> VirtioMmioDeviceConfigError {
    VirtioMmioDeviceConfigError::Handler {
        source: MmioHandlerError::new(format!(
            "virtio-balloon config access bytes failed: {source}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::interrupt::DeviceInterruptKind;
    use crate::memory::{
        GuestAddress, GuestMemory, GuestMemoryAccessError, GuestMemoryError, GuestMemoryLayout,
        GuestMemoryRange,
    };
    use crate::mmio::{MmioAccessBytes, MmioDispatchOutcome, MmioOperation, MmioRegionId};
    use crate::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
        VIRTIO_MMIO_DEVICE_CONFIG_OFFSET, VirtioMmioDeviceActivation, VirtioMmioDeviceRegisters,
        VirtioMmioQueueRegisters, VirtioMmioRegister, VirtioMmioRegisterHandlerError,
    };
    use crate::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
        VirtqueueAvailableRing, VirtqueueUsedRing, read_descriptor_chain,
    };

    const TEST_BALLOON_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_8000);
    const TEST_BALLOON_MMIO_REGION_ID: MmioRegionId = MmioRegionId::new(4000);
    const TEST_QUEUE_SIZE: u16 = 8;
    const TEST_MEMORY_SIZE: u64 = 0x20000;
    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x2000);
    const TEST_PFN_DATA: GuestAddress = GuestAddress::new(0x4000);
    const TEST_PFN_DATA_SPLIT: GuestAddress = GuestAddress::new(0x5000);
    const TEST_DESCRIPTOR_BASE: u64 = 0x1000;
    const TEST_DRIVER_BASE: u64 = 0x8000;
    const TEST_DEVICE_BASE: u64 = 0x10000;
    const TEST_QUEUE_STRIDE: u64 = 0x1000;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;

    fn balloon_config(
        amount_mib: u32,
        deflate_on_oom: bool,
        stats_polling_interval_s: u16,
        free_page_hinting: bool,
        free_page_reporting: bool,
    ) -> BalloonConfig {
        BalloonConfigInput::new(amount_mib, deflate_on_oom)
            .with_stats_polling_interval_s(stats_polling_interval_s)
            .with_free_page_hinting(free_page_hinting)
            .with_free_page_reporting(free_page_reporting)
            .into()
    }

    fn prepared(config: BalloonConfig) -> PreparedBalloonDevice {
        PreparedBalloonDevice::from_config(config).expect("balloon config should prepare")
    }

    fn pfn_payload_bytes(pfns: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for pfn in pfns {
            bytes.extend_from_slice(&pfn.to_le_bytes());
        }
        bytes
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

    fn pfn_descriptor_memory() -> GuestMemory {
        let layout = GuestMemoryLayout::new(vec![
            GuestMemoryRange::new(GuestAddress::new(0), TEST_MEMORY_SIZE)
                .expect("test memory range should be valid"),
        ])
        .expect("test memory layout should be valid");
        GuestMemory::allocate(&layout).expect("test guest memory should allocate")
    }

    fn write_guest_bytes(memory: &mut GuestMemory, address: GuestAddress, bytes: &[u8]) {
        memory
            .write_slice(bytes, address)
            .expect("guest bytes should write");
    }

    fn descriptor_table_for_queue(queue_index: usize) -> GuestAddress {
        queue_address(TEST_DESCRIPTOR_BASE, queue_index_u32(queue_index))
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

    fn write_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        write_descriptor_at(memory, TEST_DESCRIPTOR_TABLE, index, descriptor);
    }

    fn write_inflate_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        write_descriptor_at(
            memory,
            descriptor_table_for_queue(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
            index,
            descriptor,
        );
    }

    fn descriptor_chain(memory: &GuestMemory, head_index: u16) -> VirtqueueDescriptorChain {
        read_descriptor_chain(memory, TEST_DESCRIPTOR_TABLE, TEST_QUEUE_SIZE, head_index)
            .expect("descriptor chain should read")
    }

    fn descriptor_len(bytes: &[u8]) -> u32 {
        u32::try_from(bytes.len()).expect("test descriptor length should fit u32")
    }

    fn has_feature(features: u64, feature: u32) -> bool {
        features & virtio_feature_bit(feature) != 0
    }

    fn read_mmio_config(device: &mut BalloonMmioDevice, offset: u64, len: u64) -> MmioAccessBytes {
        let address = TEST_BALLOON_MMIO_BASE
            .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset)
            .expect("test MMIO address should not overflow");
        let access = device
            .dispatcher()
            .lookup(address, len)
            .expect("balloon config access should resolve");
        let outcome = device
            .dispatcher_mut()
            .dispatch(MmioOperation::read(access).expect("read operation should build"))
            .expect("balloon config read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => data,
            MmioDispatchOutcome::Write => panic!("read should return read data"),
        }
    }

    fn write_mmio_config(device: &mut BalloonMmioDevice, offset: u64, data: &[u8]) {
        let address = TEST_BALLOON_MMIO_BASE
            .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + offset)
            .expect("test MMIO address should not overflow");
        let access = device
            .dispatcher()
            .lookup(
                address,
                u64::try_from(data.len()).expect("test data length should fit u64"),
            )
            .expect("balloon config access should resolve");
        let data = MmioAccessBytes::new(data).expect("test config bytes should build");
        device
            .dispatcher_mut()
            .dispatch(MmioOperation::write(access, data).expect("write operation should build"))
            .expect("balloon config write should dispatch");
    }

    fn queue_index_u32(queue_index: usize) -> u32 {
        u32::try_from(queue_index).expect("test queue index should fit u32")
    }

    fn queue_address(base: u64, queue_index: u32) -> GuestAddress {
        GuestAddress::new(
            base.checked_add(u64::from(queue_index) * TEST_QUEUE_STRIDE)
                .expect("test queue address should not overflow"),
        )
    }

    fn address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value() & u64::from(u32::MAX))
            .expect("low address word should fit u32")
    }

    fn address_high(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value() >> u32::BITS).expect("high address word should fit u32")
    }

    fn deflate_queue() -> VirtioBalloonQueue {
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            deflate_available_ring(),
            TEST_QUEUE_SIZE,
        )
        .expect("deflate available ring should build");
        let used = VirtqueueUsedRing::new(deflate_used_ring(), TEST_QUEUE_SIZE)
            .expect("deflate used ring should build");
        VirtioBalloonQueue::new(available, used)
    }

    fn deflate_queue_with_used_ring(used_ring: GuestAddress) -> VirtioBalloonQueue {
        let available = VirtqueueAvailableRing::new(
            TEST_DESCRIPTOR_TABLE,
            deflate_available_ring(),
            TEST_QUEUE_SIZE,
        )
        .expect("deflate available ring should build");
        let used =
            VirtqueueUsedRing::new(used_ring, TEST_QUEUE_SIZE).expect("used ring should build");
        VirtioBalloonQueue::new(available, used)
    }

    fn inflate_queue() -> VirtioBalloonQueue {
        inflate_queue_with_used_ring(inflate_used_ring())
    }

    fn inflate_queue_with_used_ring(used_ring: GuestAddress) -> VirtioBalloonQueue {
        let available = VirtqueueAvailableRing::new(
            descriptor_table_for_queue(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
            inflate_available_ring(),
            TEST_QUEUE_SIZE,
        )
        .expect("inflate available ring should build");
        let used =
            VirtqueueUsedRing::new(used_ring, TEST_QUEUE_SIZE).expect("used ring should build");
        VirtioBalloonQueue::new(available, used)
    }

    fn inflate_available_ring() -> GuestAddress {
        queue_address(
            TEST_DRIVER_BASE,
            queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
        )
    }

    fn inflate_used_ring() -> GuestAddress {
        queue_address(
            TEST_DEVICE_BASE,
            queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
        )
    }

    fn deflate_available_ring() -> GuestAddress {
        queue_address(
            TEST_DRIVER_BASE,
            queue_index_u32(VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX),
        )
    }

    fn deflate_used_ring() -> GuestAddress {
        queue_address(
            TEST_DEVICE_BASE,
            queue_index_u32(VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX),
        )
    }

    fn available_ring_idx_address(available_ring: GuestAddress) -> GuestAddress {
        available_ring
            .checked_add(2)
            .expect("available index address should not overflow")
    }

    fn available_ring_entry_address(available_ring: GuestAddress, index: usize) -> GuestAddress {
        available_ring
            .checked_add(4 + u64::try_from(index).expect("available index should fit u64") * 2)
            .expect("available ring entry address should not overflow")
    }

    fn used_ring_idx_address(used_ring: GuestAddress) -> GuestAddress {
        used_ring
            .checked_add(2)
            .expect("used index address should not overflow")
    }

    fn used_ring_entry_address(used_ring: GuestAddress, index: usize) -> GuestAddress {
        used_ring
            .checked_add(4 + u64::try_from(index).expect("used index should fit u64") * 8)
            .expect("used ring entry address should not overflow")
    }

    fn write_available_heads(
        memory: &mut GuestMemory,
        available_ring: GuestAddress,
        heads: &[u16],
    ) {
        for (index, head) in heads.iter().copied().enumerate() {
            write_u16(
                memory,
                available_ring_entry_address(available_ring, index),
                head,
            );
        }
        write_u16(
            memory,
            available_ring_idx_address(available_ring),
            u16::try_from(heads.len()).expect("available head count should fit u16"),
        );
    }

    fn read_used_idx(memory: &GuestMemory, used_ring: GuestAddress) -> u16 {
        read_u16(memory, used_ring_idx_address(used_ring))
    }

    fn read_used_element(
        memory: &GuestMemory,
        used_ring: GuestAddress,
        index: usize,
    ) -> (u32, u32) {
        let address = used_ring_entry_address(used_ring, index);
        (
            read_u32(memory, address),
            read_u32(
                memory,
                address
                    .checked_add(4)
                    .expect("used ring length address should not overflow"),
            ),
        )
    }

    fn write_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 should write");
    }

    fn read_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("u16 should read");
        u16::from_le_bytes(bytes)
    }

    fn read_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("u32 should read");
        u32::from_le_bytes(bytes)
    }

    fn configure_queue_registers(queues: &mut VirtioMmioQueueRegisters, queue_index: u32) {
        queues
            .write_register(
                VirtioMmioRegister::QueueSel,
                queue_index,
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue select should write");
        queues
            .write_register(
                VirtioMmioRegister::QueueNum,
                u32::from(TEST_QUEUE_SIZE),
                QUEUE_CONFIG_STATUS,
            )
            .expect("queue size should write");

        let descriptor_table = queue_address(TEST_DESCRIPTOR_BASE, queue_index);
        let driver_ring = queue_address(TEST_DRIVER_BASE, queue_index);
        let device_ring = queue_address(TEST_DEVICE_BASE, queue_index);
        for (register, value) in [
            (
                VirtioMmioRegister::QueueDescLow,
                address_low(descriptor_table),
            ),
            (
                VirtioMmioRegister::QueueDescHigh,
                address_high(descriptor_table),
            ),
            (VirtioMmioRegister::QueueDriverLow, address_low(driver_ring)),
            (
                VirtioMmioRegister::QueueDriverHigh,
                address_high(driver_ring),
            ),
            (VirtioMmioRegister::QueueDeviceLow, address_low(device_ring)),
            (
                VirtioMmioRegister::QueueDeviceHigh,
                address_high(device_ring),
            ),
        ] {
            queues
                .write_register(register, value, QUEUE_CONFIG_STATUS)
                .expect("queue address should write");
        }
        queues
            .write_register(VirtioMmioRegister::QueueReady, 1, QUEUE_CONFIG_STATUS)
            .expect("queue ready should write");
    }

    fn configured_queue_registers(queue_count: usize) -> VirtioMmioQueueRegisters {
        let queue_sizes = vec![VIRTIO_BALLOON_QUEUE_SIZE; queue_count];
        let mut queues =
            VirtioMmioQueueRegisters::new(&queue_sizes).expect("queue table should build");
        for queue_index in 0..queue_count {
            configure_queue_registers(&mut queues, queue_index_u32(queue_index));
        }

        queues
    }

    fn activation_for_queues<'a>(
        device_registers: &'a VirtioMmioDeviceRegisters,
        queues: &'a VirtioMmioQueueRegisters,
    ) -> VirtioMmioDeviceActivation<'a> {
        VirtioMmioDeviceActivation::new(device_registers, queues)
    }

    fn configure_handler_queue(handler: &mut VirtioBalloonMmioHandler, queue_index: u32) {
        handler
            .write_register(VirtioMmioRegister::QueueSel, queue_index)
            .expect("queue select should write");
        handler
            .write_register(VirtioMmioRegister::QueueNum, u32::from(TEST_QUEUE_SIZE))
            .expect("queue size should write");

        let descriptor_table = queue_address(TEST_DESCRIPTOR_BASE, queue_index);
        let driver_ring = queue_address(TEST_DRIVER_BASE, queue_index);
        let device_ring = queue_address(TEST_DEVICE_BASE, queue_index);
        for (register, value) in [
            (
                VirtioMmioRegister::QueueDescLow,
                address_low(descriptor_table),
            ),
            (
                VirtioMmioRegister::QueueDescHigh,
                address_high(descriptor_table),
            ),
            (VirtioMmioRegister::QueueDriverLow, address_low(driver_ring)),
            (
                VirtioMmioRegister::QueueDriverHigh,
                address_high(driver_ring),
            ),
            (VirtioMmioRegister::QueueDeviceLow, address_low(device_ring)),
            (
                VirtioMmioRegister::QueueDeviceHigh,
                address_high(device_ring),
            ),
        ] {
            handler
                .write_register(register, value)
                .expect("queue address should write");
        }
        handler
            .write_register(VirtioMmioRegister::QueueReady, 1)
            .expect("queue ready should write");
    }

    fn set_handler_queue_config_status(handler: &mut VirtioBalloonMmioHandler) {
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("acknowledge status should write");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("driver status should write");
        handler
            .write_register(VirtioMmioRegister::Status, QUEUE_CONFIG_STATUS)
            .expect("features-ok status should write");
    }

    fn activate_handler(handler: &mut VirtioBalloonMmioHandler) {
        set_handler_queue_config_status(handler);
        for queue_index in 0..handler.queue_registers().queue_count() {
            configure_handler_queue(handler, queue_index_u32(queue_index));
        }
        handler
            .write_register(VirtioMmioRegister::Status, DRIVER_OK_STATUS)
            .expect("driver-ok status should write");
    }

    fn balloon_mmio_device(config: BalloonConfig) -> BalloonMmioDevice {
        prepared(config)
            .register_mmio(BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                TEST_BALLOON_MMIO_REGION_ID,
            ))
            .expect("balloon MMIO device should register")
    }

    #[test]
    fn default_prepared_device_has_version_feature_and_base_queues() {
        let device = prepared(balloon_config(64, false, 0, false, false));

        assert_eq!(
            device.available_features(),
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1)
        );
        assert_eq!(
            device.queue_layout().queue_count(),
            VIRTIO_BALLOON_MIN_QUEUE_COUNT
        );
        let queues: Vec<_> = device.queue_layout().iter().collect();
        assert_eq!(
            queues,
            vec![
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Inflate,
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Deflate,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
            ]
        );
    }

    #[test]
    fn prepared_device_features_follow_balloon_config() {
        let device = prepared(balloon_config(64, true, 1, true, true));
        let features = device.available_features();

        assert!(has_feature(features, VIRTIO_FEATURE_VERSION_1));
        assert!(has_feature(features, VIRTIO_BALLOON_F_DEFLATE_ON_OOM));
        assert!(has_feature(features, VIRTIO_BALLOON_F_STATS_VQ));
        assert!(has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_HINTING));
        assert!(has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_REPORTING));
    }

    #[test]
    fn prepared_device_omits_disabled_optional_features() {
        let device = prepared(balloon_config(64, true, 0, false, true));
        let features = device.available_features();

        assert!(has_feature(features, VIRTIO_FEATURE_VERSION_1));
        assert!(has_feature(features, VIRTIO_BALLOON_F_DEFLATE_ON_OOM));
        assert!(!has_feature(features, VIRTIO_BALLOON_F_STATS_VQ));
        assert!(!has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_HINTING));
        assert!(has_feature(features, VIRTIO_BALLOON_F_FREE_PAGE_REPORTING));
    }

    #[test]
    fn prepared_device_converts_target_mib_to_config_pages() {
        let device = prepared(balloon_config(64, false, 0, false, false));
        let config_space = device.config_space();

        assert_eq!(
            config_space.num_pages(),
            64 * VIRTIO_BALLOON_MIB_TO_4K_PAGES
        );
        assert_eq!(config_space.actual_pages(), 0);
        assert_eq!(
            config_space.free_page_hint_cmd_id(),
            VIRTIO_BALLOON_FREE_PAGE_HINT_STOP
        );
    }

    #[test]
    fn page_conversion_rejects_overflow() {
        let amount_mib = VIRTIO_BALLOON_MAX_AMOUNT_MIB + 1;
        let err = mib_to_4k_pages(amount_mib).expect_err("page conversion should overflow");

        assert_eq!(err.amount_mib(), amount_mib);
        assert_eq!(err.max_amount_mib(), VIRTIO_BALLOON_MAX_AMOUNT_MIB);
        assert_eq!(
            err.to_string(),
            "balloon amount_mib 16777216 exceeds maximum 16777215 MiB representable as 4 KiB pages"
        );
        assert_eq!(
            PreparedBalloonDevice::from_config(balloon_config(amount_mib, false, 0, false, false))
                .expect_err("prepared device should reject page overflow"),
            err
        );
    }

    #[test]
    fn page_conversion_accepts_maximum_amount() {
        let pages = mib_to_4k_pages(VIRTIO_BALLOON_MAX_AMOUNT_MIB)
            .expect("maximum balloon amount should convert");
        let device = prepared(balloon_config(
            VIRTIO_BALLOON_MAX_AMOUNT_MIB,
            false,
            0,
            false,
            false,
        ));

        assert_eq!(pages, u32::MAX - 255);
        assert_eq!(device.config_space().num_pages(), pages);
    }

    #[test]
    fn page_conversion_accepts_zero_amount() {
        let pages = mib_to_4k_pages(0).expect("zero balloon amount should convert");
        let device = prepared(balloon_config(0, false, 0, false, false));

        assert_eq!(pages, 0);
        assert_eq!(device.config_space().num_pages(), 0);
    }

    #[test]
    fn config_space_uses_firecracker_little_endian_layout() {
        let config_space = VirtioBalloonConfigSpace::new(0x0102_0304, 0x0506_0708, 0x090a_0b0c);

        assert_eq!(
            config_space.to_le_bytes(),
            [
                0x04, 0x03, 0x02, 0x01, 0x08, 0x07, 0x06, 0x05, 0x0c, 0x0b, 0x0a, 0x09,
            ]
        );
        assert_eq!(
            config_space.to_le_bytes().len(),
            VIRTIO_BALLOON_CONFIG_SPACE_SIZE
        );
    }

    #[test]
    fn pfn_payload_parser_accepts_little_endian_pfns() {
        let bytes = pfn_payload_bytes(&[0x0102_0304, 0x0506_0708, 0xffff_ffff]);

        let payload = VirtioBalloonPfnPayload::parse(&bytes).expect("PFN payload should parse");

        assert_eq!(payload.pfns(), &[0x0102_0304, 0x0506_0708, 0xffff_ffff]);
        assert_eq!(payload.len(), 3);
        assert!(!payload.is_empty());
        assert_eq!(
            payload.into_vec(),
            vec![0x0102_0304, 0x0506_0708, 0xffff_ffff]
        );
    }

    #[test]
    fn pfn_payload_parser_rejects_empty_payload() {
        let err =
            VirtioBalloonPfnPayload::parse(&[]).expect_err("empty PFN payload should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnPayloadParseError::EmptyPayload
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN payload cannot be empty"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_payload_parser_rejects_unaligned_payload_length() {
        let err = VirtioBalloonPfnPayload::parse(&[1, 2, 3, 4, 5])
            .expect_err("unaligned PFN payload should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnPayloadParseError::UnalignedLength { len: 5 }
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN payload length 5 is not a multiple of 4"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_payload_parser_accepts_maximum_pfn_count() {
        let bytes = vec![0; VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE];

        let payload =
            VirtioBalloonPfnPayload::parse(&bytes).expect("maximum PFN payload should parse");

        assert_eq!(payload.len(), VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR);
        assert!(
            payload.pfns().iter().all(|pfn| *pfn == 0),
            "zero-filled payload should parse as zero PFNs"
        );
    }

    #[test]
    fn pfn_payload_parser_rejects_one_over_maximum_pfn_count() {
        let bytes = vec![0; VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + VIRTIO_BALLOON_PFN_SIZE];

        let err = VirtioBalloonPfnPayload::parse(&bytes)
            .expect_err("oversized PFN payload should be rejected before parsing");

        assert!(matches!(
            err,
            VirtioBalloonPfnPayloadParseError::TooManyPfns {
                count,
                max: VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR
            } if count == VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR + 1
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN payload contains 257 PFNs, exceeding maximum 256"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_payload_parser_errors_return_no_partial_payload() {
        assert!(VirtioBalloonPfnPayload::parse(&[0, 1, 2]).is_err());
        let oversized_payload =
            vec![0; VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + VIRTIO_BALLOON_PFN_SIZE];
        assert!(VirtioBalloonPfnPayload::parse(&oversized_payload).is_err());
    }

    #[test]
    fn pfn_descriptor_payload_reads_single_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[0x0102_0304, 0x0506_0708, 0xffff_ffff]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);

        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("single PFN descriptor payload should read");

        assert_eq!(payload.bytes(), bytes.as_slice());
        assert_eq!(payload.len(), bytes.len());
        assert!(!payload.is_empty());
        assert_eq!(payload.clone().into_vec(), bytes);
        assert_eq!(
            payload
                .into_pfn_payload()
                .expect("descriptor payload should parse")
                .pfns(),
            &[0x0102_0304, 0x0506_0708, 0xffff_ffff]
        );
    }

    #[test]
    fn pfn_descriptor_payload_reads_split_descriptor_chain() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[1, 2, 3]);
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA,
            &bytes[..VIRTIO_BALLOON_PFN_SIZE],
        );
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA_SPLIT,
            &bytes[VIRTIO_BALLOON_PFN_SIZE..],
        );
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_PFN_DATA,
                u32::try_from(VIRTIO_BALLOON_PFN_SIZE).expect("PFN size should fit u32"),
                Some(1),
            ),
        );
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_PFN_DATA_SPLIT,
                descriptor_len(&bytes[VIRTIO_BALLOON_PFN_SIZE..]),
                None,
            ),
        );
        let chain = descriptor_chain(&memory, 0);

        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("split PFN descriptor payload should read");

        assert_eq!(payload.bytes(), bytes.as_slice());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_write_only_descriptor() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[1]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::writable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("write-only PFN descriptor should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorWriteOnly { index: 0 }
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN descriptor 0 is write-only"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_empty_descriptor() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, 0, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("empty PFN descriptor should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorEmpty { index: 0 }
        ));
        assert_eq!(err.to_string(), "virtio-balloon PFN descriptor 0 is empty");
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_unmapped_descriptor() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(GuestAddress::new(TEST_MEMORY_SIZE), 4, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("unmapped PFN descriptor should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorAccess {
                index: 0,
                address,
                len: 4,
                source: GuestMemoryAccessError::UnmappedRange { .. },
            } if address == GuestAddress::new(TEST_MEMORY_SIZE)
        ));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_range_overflow() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(GuestAddress::new(u64::MAX), 4, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("overflowing PFN descriptor range should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::DescriptorRange {
                index: 0,
                address,
                len: 4,
                source: GuestMemoryError::AddressOverflow { .. },
            } if address == GuestAddress::new(u64::MAX)
        ));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn pfn_descriptor_payload_reports_unaligned_total_length() {
        let mut memory = pfn_descriptor_memory();
        let bytes = [1, 2, 3, 4, 5];
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);
        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("unaligned descriptor payload bytes should still read");

        let err = payload
            .parse_pfn_payload()
            .expect_err("unaligned PFN payload should fail parsing");

        assert!(matches!(
            err,
            VirtioBalloonPfnPayloadParseError::UnalignedLength { len: 5 }
        ));
    }

    #[test]
    fn pfn_descriptor_payload_accepts_exact_maximum_payload() {
        let mut memory = pfn_descriptor_memory();
        let bytes = vec![0; VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE];
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);

        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("maximum PFN descriptor payload should read");
        let parsed = payload
            .parse_pfn_payload()
            .expect("maximum PFN descriptor payload should parse");

        assert_eq!(payload.len(), VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE);
        assert_eq!(parsed.len(), VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR);
    }

    #[test]
    fn pfn_descriptor_payload_rejects_one_over_maximum_payload() {
        let mut memory = pfn_descriptor_memory();
        let len = u32::try_from(VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + 1)
            .expect("test payload length should fit u32");
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, len, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("oversized PFN descriptor payload should be rejected");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthTooLarge {
                len,
                max: VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE,
            } if len == VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + 1
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN descriptor payload length 1025 exceeds maximum 1024"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_descriptor_payload_rejects_oversized_payload_before_guest_range_access() {
        let mut memory = pfn_descriptor_memory();
        let len = u32::try_from(VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + 1)
            .expect("test payload length should fit u32");
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(GuestAddress::new(TEST_MEMORY_SIZE), len, None),
        );
        let chain = descriptor_chain(&memory, 0);

        let err = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect_err("oversized PFN descriptor payload should be rejected before access");

        assert!(matches!(
            err,
            VirtioBalloonPfnDescriptorPayloadReadError::PayloadLengthTooLarge {
                len,
                max: VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE,
            } if len == VIRTIO_BALLOON_MAX_PFN_PAYLOAD_SIZE + 1
        ));
    }

    #[test]
    fn pfn_descriptor_payload_converts_into_ranges() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[3, 1, 2, 2]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        let chain = descriptor_chain(&memory, 0);
        let payload = VirtioBalloonPfnDescriptorPayload::read(&memory, &chain)
            .expect("PFN descriptor payload should read");

        let ranges = payload
            .parse_pfn_payload()
            .expect("PFN descriptor payload should parse")
            .into_page_ranges()
            .expect("PFN descriptor payload should compact");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 1,
                page_count: 3
            }]
        );
    }

    #[test]
    fn pfn_ranges_accept_empty_input() {
        let ranges =
            VirtioBalloonPfnRanges::from_pfns(Vec::new()).expect("empty PFN input should compact");

        assert!(ranges.is_empty());
        assert_eq!(ranges.len(), 0);
        assert_eq!(ranges.ranges(), &[]);
        assert_eq!(ranges.into_vec(), Vec::new());
    }

    #[test]
    fn pfn_ranges_compact_contiguous_pfns() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![0, 1, 2, 3])
            .expect("contiguous PFNs should compact");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 0,
                page_count: 4
            }]
        );
        assert_eq!(ranges.ranges()[0].start_pfn(), 0);
        assert_eq!(ranges.ranges()[0].page_count(), 4);
    }

    #[test]
    fn pfn_ranges_sort_unsorted_pfns() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![5, 2, 4, 3, 9])
            .expect("unsorted PFNs should compact");

        assert_eq!(
            ranges.ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 2,
                    page_count: 4
                },
                VirtioBalloonPfnRange {
                    start_pfn: 9,
                    page_count: 1
                }
            ]
        );
    }

    #[test]
    fn pfn_ranges_deduplicate_pfns() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![7, 7, 8, 8, 10, 9])
            .expect("duplicate PFNs should compact");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 7,
                page_count: 4
            }]
        );
    }

    #[test]
    fn pfn_ranges_keep_separated_ranges() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![0, 1, 3, 4, 6])
            .expect("separated PFNs should compact");

        assert_eq!(
            ranges.ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 0,
                    page_count: 2
                },
                VirtioBalloonPfnRange {
                    start_pfn: 3,
                    page_count: 2
                },
                VirtioBalloonPfnRange {
                    start_pfn: 6,
                    page_count: 1
                }
            ]
        );
    }

    #[test]
    fn pfn_ranges_handle_maximum_pfn_without_overflow() {
        let ranges = VirtioBalloonPfnRanges::from_pfns(vec![u32::MAX, u32::MAX - 1, u32::MAX, 0])
            .expect("maximum PFN should compact without overflow");

        assert_eq!(
            ranges.ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 0,
                    page_count: 1
                },
                VirtioBalloonPfnRange {
                    start_pfn: u32::MAX - 1,
                    page_count: 2
                }
            ]
        );
    }

    #[test]
    fn pfn_ranges_accept_maximum_pfn_count() {
        let max_page_count = u32::try_from(VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR)
            .expect("maximum PFN count should fit u32");
        let pfns = (0..max_page_count).collect();

        let ranges =
            VirtioBalloonPfnRanges::from_pfns(pfns).expect("maximum PFN count should compact");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 0,
                page_count: max_page_count
            }]
        );
    }

    #[test]
    fn pfn_ranges_reject_one_over_maximum_pfn_count() {
        let err =
            VirtioBalloonPfnRanges::from_pfns(vec![0; VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR + 1])
                .expect_err("oversized PFN range input should be rejected before sorting");

        assert!(matches!(
            err,
            VirtioBalloonPfnRangeCompactError::TooManyPfns {
                count,
                max: VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR
            } if count == VIRTIO_BALLOON_MAX_PFNS_PER_DESCRIPTOR + 1
        ));
        assert_eq!(
            err.to_string(),
            "virtio-balloon PFN compaction input contains 257 PFNs, exceeding maximum 256"
        );
        assert!(std::error::Error::source(&err).is_none());
    }

    #[test]
    fn pfn_payload_converts_into_ranges() {
        let bytes = pfn_payload_bytes(&[3, 1, 2, 2]);
        let payload = VirtioBalloonPfnPayload::parse(&bytes).expect("PFN payload should parse");

        let ranges = payload
            .into_page_ranges()
            .expect("parsed PFNs should compact into ranges");

        assert_eq!(
            ranges.ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 1,
                page_count: 3
            }]
        );
    }

    #[test]
    fn inflate_queue_dispatch_empty_available_ring_is_noop() {
        let mut memory = pfn_descriptor_memory();
        let mut queue = inflate_queue();

        let dispatch = queue
            .dispatch_inflate(&mut memory)
            .expect("empty inflate queue should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert!(dispatch.inflated_page_ranges().is_empty());
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 0);
    }

    #[test]
    fn inflate_queue_dispatch_publishes_zero_length_used_element_and_records_ranges() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[3, 1, 2, 2]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let mut queue = inflate_queue();

        let dispatch = queue
            .dispatch_inflate(&mut memory)
            .expect("inflate descriptor should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            dispatch.inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 1,
                page_count: 3,
            }]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
    }

    #[test]
    fn inflate_queue_dispatch_reads_split_descriptor_payload() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[5, 6, 8]);
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA,
            &bytes[..VIRTIO_BALLOON_PFN_SIZE],
        );
        write_guest_bytes(
            &mut memory,
            TEST_PFN_DATA_SPLIT,
            &bytes[VIRTIO_BALLOON_PFN_SIZE..],
        );
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(
                TEST_PFN_DATA,
                u32::try_from(VIRTIO_BALLOON_PFN_SIZE).expect("PFN size should fit u32"),
                Some(1),
            ),
        );
        write_inflate_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(
                TEST_PFN_DATA_SPLIT,
                descriptor_len(&bytes[VIRTIO_BALLOON_PFN_SIZE..]),
                None,
            ),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let mut queue = inflate_queue();

        let dispatch = queue
            .dispatch_inflate(&mut memory)
            .expect("split inflate descriptor should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert_eq!(
            dispatch.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 5,
                    page_count: 2,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 8,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
    }

    #[test]
    fn inflate_queue_dispatch_aggregates_multiple_descriptors() {
        let mut memory = pfn_descriptor_memory();
        let first = pfn_payload_bytes(&[10]);
        let second = pfn_payload_bytes(&[20, 21]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &first);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &second);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&first), None),
        );
        write_inflate_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, descriptor_len(&second), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0, 1]);
        let mut queue = inflate_queue();

        let dispatch = queue
            .dispatch_inflate(&mut memory)
            .expect("inflate descriptors should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 2);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(
            dispatch.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 10,
                    page_count: 1,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 20,
                    page_count: 2,
                }
            ]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 2);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 1), (1, 0));
    }

    #[test]
    fn inflate_queue_dispatch_available_ring_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[13, 14]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0, TEST_QUEUE_SIZE]);
        let mut queue = inflate_queue();

        let error = queue
            .dispatch_inflate(&mut memory)
            .expect_err("invalid second inflate queue head should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::AvailableRing { .. }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert!(error.completed_dispatch().needs_queue_interrupt());
        assert_eq!(
            error.completed_dispatch().inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 13,
                page_count: 2,
            }]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn inflate_queue_dispatch_parse_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let first = pfn_payload_bytes(&[7]);
        let malformed = [1, 2, 3, 4, 5];
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &first);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &malformed);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&first), None),
        );
        write_inflate_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, descriptor_len(&malformed), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0, 1]);
        let mut queue = inflate_queue();

        let error = queue
            .dispatch_inflate(&mut memory)
            .expect_err("malformed second inflate descriptor should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::PfnPayloadParse {
                descriptor_head: 1,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert!(error.completed_dispatch().needs_queue_interrupt());
        assert_eq!(
            error.completed_dispatch().inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 7,
                page_count: 1,
            }]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn inflate_queue_dispatch_read_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let first = pfn_payload_bytes(&[9]);
        let second = pfn_payload_bytes(&[11]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &first);
        write_guest_bytes(&mut memory, TEST_PFN_DATA_SPLIT, &second);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&first), None),
        );
        write_inflate_descriptor(
            &mut memory,
            1,
            TestDescriptor::writable(TEST_PFN_DATA_SPLIT, descriptor_len(&second), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0, 1]);
        let mut queue = inflate_queue();

        let error = queue
            .dispatch_inflate(&mut memory)
            .expect_err("write-only second inflate descriptor should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::PfnDescriptorRead {
                descriptor_head: 1,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert_eq!(
            error.completed_dispatch().inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 9,
                page_count: 1,
            }]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn inflate_queue_dispatch_used_ring_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[12]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let mut queue = inflate_queue_with_used_ring(GuestAddress::new(TEST_MEMORY_SIZE));

        let error = queue
            .dispatch_inflate(&mut memory)
            .expect_err("unmapped inflate used ring should fail publication");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::UsedRing {
                descriptor_head: 0,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 0);
        assert!(!error.completed_dispatch().needs_queue_interrupt());
        assert!(error.completed_dispatch().inflated_page_ranges().is_empty());
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn deflate_queue_dispatch_empty_available_ring_is_noop() {
        let mut memory = pfn_descriptor_memory();
        let mut queue = deflate_queue();

        let dispatch = queue
            .dispatch_deflate(&mut memory)
            .expect("empty deflate queue should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 0);
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 0);
    }

    #[test]
    fn deflate_queue_dispatch_publishes_zero_length_used_element() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_PFN_SIZE as u32, None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let mut queue = deflate_queue();

        let dispatch = queue
            .dispatch_deflate(&mut memory)
            .expect("deflate descriptor should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 1);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
    }

    #[test]
    fn deflate_queue_dispatch_publishes_multiple_zero_length_used_elements() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_PFN_SIZE as u32, None),
        );
        write_descriptor(
            &mut memory,
            1,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, VIRTIO_BALLOON_PFN_SIZE as u32, None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0, 1]);
        let mut queue = deflate_queue();

        let dispatch = queue
            .dispatch_deflate(&mut memory)
            .expect("deflate descriptors should dispatch");

        assert_eq!(dispatch.completed_descriptors(), 2);
        assert!(dispatch.needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 2);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 1), (1, 0));
    }

    #[test]
    fn deflate_queue_dispatch_available_ring_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_PFN_SIZE as u32, None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0, TEST_QUEUE_SIZE]);
        let mut queue = deflate_queue();

        let error = queue
            .dispatch_deflate(&mut memory)
            .expect_err("invalid second available head should fail");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::AvailableRing { .. }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 1);
        assert!(error.completed_dispatch().needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn deflate_queue_dispatch_used_ring_error_preserves_completed_dispatch() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_PFN_SIZE as u32, None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let mut queue = deflate_queue_with_used_ring(GuestAddress::new(TEST_MEMORY_SIZE));

        let error = queue
            .dispatch_deflate(&mut memory)
            .expect_err("unmapped used ring should fail publication");

        assert!(matches!(
            error,
            VirtioBalloonQueueDispatchError::UsedRing {
                descriptor_head: 0,
                ..
            }
        ));
        assert_eq!(error.completed_dispatch().completed_descriptors(), 0);
        assert!(!error.completed_dispatch().needs_queue_interrupt());
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn optional_queue_metadata_is_deterministic_and_bounded() {
        let device = prepared(balloon_config(64, false, 1, true, true));
        let queues: Vec<_> = device.queue_layout().iter().collect();

        assert_eq!(queues.len(), VIRTIO_BALLOON_MAX_QUEUE_COUNT);
        assert_eq!(
            queues,
            vec![
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Inflate,
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Deflate,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::Statistics,
                    VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::FreePageHinting,
                    VIRTIO_BALLOON_STATS_QUEUE_INDEX + 1,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
                VirtioBalloonQueueConfig::new(
                    VirtioBalloonQueueKind::FreePageReporting,
                    VIRTIO_BALLOON_STATS_QUEUE_INDEX + 2,
                    VIRTIO_BALLOON_QUEUE_SIZE,
                ),
            ]
        );
    }

    #[test]
    fn optional_queues_compact_when_statistics_queue_is_disabled() {
        let device = prepared(balloon_config(64, false, 0, true, true));
        let queues: Vec<_> = device.queue_layout().iter().collect();

        assert_eq!(queues.len(), VIRTIO_BALLOON_MIN_QUEUE_COUNT + 2);
        assert_eq!(queues[2].kind(), VirtioBalloonQueueKind::FreePageHinting);
        assert_eq!(queues[2].index(), VIRTIO_BALLOON_STATS_QUEUE_INDEX);
        assert_eq!(queues[3].kind(), VirtioBalloonQueueKind::FreePageReporting);
        assert_eq!(queues[3].index(), VIRTIO_BALLOON_STATS_QUEUE_INDEX + 1);
    }

    #[test]
    fn reporting_queue_compacts_when_hinting_and_statistics_are_disabled() {
        let device = prepared(balloon_config(64, false, 0, false, true));
        let queues: Vec<_> = device.queue_layout().iter().collect();

        assert_eq!(queues.len(), VIRTIO_BALLOON_MIN_QUEUE_COUNT + 1);
        assert_eq!(queues[2].kind(), VirtioBalloonQueueKind::FreePageReporting);
        assert_eq!(queues[2].index(), VIRTIO_BALLOON_STATS_QUEUE_INDEX);
    }

    #[test]
    fn sparse_optional_queue_combinations_keep_expected_indexes() {
        let stats_only = prepared(balloon_config(64, false, 1, false, false)).queue_layout();
        assert_eq!(stats_only.queue_count(), VIRTIO_BALLOON_MIN_QUEUE_COUNT + 1);
        assert_eq!(
            stats_only.statistics(),
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::Statistics,
                VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        );
        assert_eq!(stats_only.free_page_hinting(), None);
        assert_eq!(stats_only.free_page_reporting(), None);

        let hinting_only = prepared(balloon_config(64, false, 0, true, false)).queue_layout();
        assert_eq!(
            hinting_only.queue_count(),
            VIRTIO_BALLOON_MIN_QUEUE_COUNT + 1
        );
        assert_eq!(hinting_only.statistics(), None);
        assert_eq!(
            hinting_only.free_page_hinting(),
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageHinting,
                VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        );
        assert_eq!(hinting_only.free_page_reporting(), None);

        let stats_reporting = prepared(balloon_config(64, false, 1, false, true)).queue_layout();
        assert_eq!(
            stats_reporting.queue_count(),
            VIRTIO_BALLOON_MIN_QUEUE_COUNT + 2
        );
        assert_eq!(
            stats_reporting.statistics(),
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::Statistics,
                VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        );
        assert_eq!(stats_reporting.free_page_hinting(), None);
        assert_eq!(
            stats_reporting.free_page_reporting(),
            Some(VirtioBalloonQueueConfig::new(
                VirtioBalloonQueueKind::FreePageReporting,
                VIRTIO_BALLOON_STATS_QUEUE_INDEX + 1,
                VIRTIO_BALLOON_QUEUE_SIZE,
            ))
        );
    }

    #[test]
    fn queue_sizes_follow_compacted_queue_layout() {
        let base = prepared(balloon_config(64, false, 0, false, false)).queue_sizes();
        assert_eq!(base.as_slice(), &[VIRTIO_BALLOON_QUEUE_SIZE; 2]);

        let all_enabled = prepared(balloon_config(64, false, 1, true, true)).queue_sizes();
        assert_eq!(all_enabled.len(), VIRTIO_BALLOON_MAX_QUEUE_COUNT);
        assert_eq!(all_enabled.as_slice(), &[VIRTIO_BALLOON_QUEUE_SIZE; 5]);

        let reporting_only = prepared(balloon_config(64, false, 0, false, true)).queue_sizes();
        assert_eq!(
            reporting_only.as_slice(),
            &[VIRTIO_BALLOON_QUEUE_SIZE; VIRTIO_BALLOON_MIN_QUEUE_COUNT + 1]
        );
    }

    #[test]
    fn balloon_device_activation_builds_base_queues_from_mmio_state() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);

        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("balloon device should activate");

        let active = device
            .active_queues()
            .expect("balloon device should expose active queues");
        assert_eq!(active.queue_count(), VIRTIO_BALLOON_MIN_QUEUE_COUNT);
        assert_eq!(
            active.inflate().available_ring().descriptor_table(),
            queue_address(
                TEST_DESCRIPTOR_BASE,
                queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX)
            )
        );
        assert_eq!(
            active.deflate().used_ring().used_ring(),
            queue_address(
                TEST_DEVICE_BASE,
                queue_index_u32(VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX)
            )
        );
        assert_eq!(
            active.inflate().available_ring().queue_size(),
            TEST_QUEUE_SIZE
        );
        assert!(active.statistics().is_none());
        assert!(active.free_page_hinting().is_none());
        assert!(active.free_page_reporting().is_none());
    }

    #[test]
    fn balloon_device_activation_builds_optional_queues_from_mmio_state() {
        let layout = prepared(balloon_config(64, false, 1, true, true)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);

        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("balloon device should activate optional queues");

        let active = device
            .active_queues()
            .expect("balloon device should expose active queues");
        assert_eq!(active.queue_count(), VIRTIO_BALLOON_MAX_QUEUE_COUNT);
        assert!(active.statistics().is_some());
        assert!(active.free_page_hinting().is_some());
        assert!(active.free_page_reporting().is_some());
    }

    #[test]
    fn balloon_device_activation_rejects_queue_count_mismatch() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(1);
        let mut device = VirtioBalloonDevice::new(layout);

        let error = device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect_err("queue count mismatch should fail activation");

        assert!(matches!(
            error,
            VirtioBalloonDeviceActivationError::QueueCountMismatch {
                expected: VIRTIO_BALLOON_MIN_QUEUE_COUNT,
                actual: 1
            }
        ));
        assert_eq!(
            error.to_string(),
            "virtio-balloon device requires 2 queue(s), got 1"
        );
        assert!(std::error::Error::source(&error).is_none());
        assert!(!device.is_activated());
    }

    #[test]
    fn balloon_device_activation_rejects_not_ready_queue() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queue_sizes = vec![VIRTIO_BALLOON_QUEUE_SIZE; VIRTIO_BALLOON_MIN_QUEUE_COUNT];
        let mut queues =
            VirtioMmioQueueRegisters::new(&queue_sizes).expect("queue table should build");
        configure_queue_registers(
            &mut queues,
            queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
        );
        let mut device = VirtioBalloonDevice::new(layout);

        let error = device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect_err("not-ready deflate queue should fail activation");

        assert!(matches!(
            error,
            VirtioBalloonDeviceActivationError::QueueBuild {
                queue_index: 1,
                kind: VirtioBalloonQueueKind::Deflate,
                source: VirtioBalloonQueueBuildError::QueueNotReady
            }
        ));
        assert_eq!(
            error.to_string(),
            "failed to activate virtio-balloon deflate queue 1: virtio-balloon queue is not ready"
        );
        assert!(std::error::Error::source(&error).is_some());
        assert!(!device.is_activated());
    }

    #[test]
    fn balloon_device_activation_rejects_duplicate_activation() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let activation = activation_for_queues(&device_registers, &queues);
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation)
            .expect("first activation should succeed");

        let error = device
            .activate_balloon(activation)
            .expect_err("second activation should fail");

        assert!(matches!(
            error,
            VirtioBalloonDeviceActivationError::AlreadyActive
        ));
        assert_eq!(error.to_string(), "virtio-balloon device is already active");
        assert!(device.is_activated());
    }

    #[test]
    fn balloon_device_reset_clears_active_queues() {
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        device.reset();

        assert!(!device.is_activated());
        assert!(device.active_queues().is_none());
    }

    #[test]
    fn balloon_notification_dispatch_accepts_empty_batch_before_activation() {
        let mut memory = pfn_descriptor_memory();
        let mut device = VirtioBalloonDevice::new(
            prepared(balloon_config(64, false, 0, false, false)).queue_layout(),
        );

        let dispatch = device
            .dispatch_drained_queue_notifications(&mut memory, Vec::new())
            .expect("empty notification batch should dispatch");

        assert!(dispatch.drained_notifications().is_empty());
        assert_eq!(dispatch.inflate_notifications(), 0);
        assert_eq!(dispatch.deflate_notifications(), 0);
        assert!(dispatch.inflate_queue_dispatch().is_none());
        assert!(dispatch.deflate_queue_dispatch().is_none());
        assert!(!dispatch.needs_queue_interrupt());
    }

    #[test]
    fn balloon_notification_dispatch_rejects_supported_queue_before_activation() {
        let mut memory = pfn_descriptor_memory();
        let mut device = VirtioBalloonDevice::new(
            prepared(balloon_config(64, false, 0, false, false)).queue_layout(),
        );

        let error = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![VIRTIO_BALLOON_INFLATE_QUEUE_INDEX],
            )
            .expect_err("inactive device should reject supported queue notification");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::Inactive { .. }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_BALLOON_INFLATE_QUEUE_INDEX]
        );
        assert_eq!(
            error.to_string(),
            "virtio-balloon queue notification cannot be dispatched before activation"
        );
    }

    #[test]
    fn balloon_notification_dispatch_preserves_completed_deflate_queue_dispatch() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_PFN_SIZE as u32, None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0, TEST_QUEUE_SIZE]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let error = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX],
            )
            .expect_err("invalid deflate queue head should fail dispatch");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::QueueDispatch { .. }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX]
        );
        let completed = error
            .completed_dispatch()
            .expect("queue dispatch error should expose completed work");
        assert_eq!(completed.completed_descriptors(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn balloon_notification_dispatch_preserves_completed_inflate_when_deflate_fails() {
        let mut memory = pfn_descriptor_memory();
        let inflate_bytes = pfn_payload_bytes(&[30, 31]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &inflate_bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&inflate_bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        write_available_heads(&mut memory, deflate_available_ring(), &[TEST_QUEUE_SIZE]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let error = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                ],
            )
            .expect_err("invalid deflate queue head should fail after inflate dispatch");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::QueueDispatch { .. }
        ));
        let completed = error
            .completed_notification_dispatch()
            .expect("queue dispatch error should expose completed notification state");
        let inflate_dispatch = completed
            .inflate_queue_dispatch()
            .expect("inflate queue dispatch should be preserved");
        assert_eq!(inflate_dispatch.completed_descriptors(), 1);
        assert_eq!(
            inflate_dispatch.inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 30,
                page_count: 2,
            }]
        );
        assert!(completed.needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 0);
    }

    #[test]
    fn balloon_notification_dispatch_coalesces_duplicate_deflate_notifications() {
        let mut memory = pfn_descriptor_memory();
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, VIRTIO_BALLOON_PFN_SIZE as u32, None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let dispatch = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
                ],
            )
            .expect("duplicate deflate notifications should dispatch once");

        assert_eq!(dispatch.deflate_notifications(), 2);
        let deflate_dispatch = dispatch
            .deflate_queue_dispatch()
            .expect("deflate queue should dispatch");
        assert_eq!(deflate_dispatch.completed_descriptors(), 1);
        assert!(deflate_dispatch.needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
    }

    #[test]
    fn balloon_notification_dispatch_coalesces_duplicate_inflate_notifications() {
        let mut memory = pfn_descriptor_memory();
        let bytes = pfn_payload_bytes(&[40, 42]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        let layout = prepared(balloon_config(64, false, 0, false, false)).queue_layout();
        let device_registers = VirtioMmioDeviceRegisters::new(
            VIRTIO_BALLOON_DEVICE_ID,
            virtio_feature_bit(VIRTIO_FEATURE_VERSION_1),
        );
        let queues = configured_queue_registers(layout.queue_count());
        let mut device = VirtioBalloonDevice::new(layout);
        device
            .activate_balloon(activation_for_queues(&device_registers, &queues))
            .expect("activation should succeed");

        let dispatch = device
            .dispatch_drained_queue_notifications(
                &mut memory,
                vec![
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                    VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                ],
            )
            .expect("duplicate inflate notifications should dispatch once");

        assert_eq!(dispatch.inflate_notifications(), 2);
        let inflate_dispatch = dispatch
            .inflate_queue_dispatch()
            .expect("inflate queue should dispatch");
        assert_eq!(inflate_dispatch.completed_descriptors(), 1);
        assert!(inflate_dispatch.needs_queue_interrupt());
        assert_eq!(
            inflate_dispatch.inflated_page_ranges(),
            &[
                VirtioBalloonPfnRange {
                    start_pfn: 40,
                    page_count: 1,
                },
                VirtioBalloonPfnRange {
                    start_pfn: 42,
                    page_count: 1,
                }
            ]
        );
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
    }

    #[test]
    fn balloon_mmio_handler_activates_device_with_configured_queues() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");

        activate_handler(handler);

        assert!(handler.is_device_activated());
        assert!(handler.activation_handler().is_activated());
        assert_eq!(
            handler
                .activation_handler()
                .active_queues()
                .expect("active queues should be present")
                .queue_count(),
            VIRTIO_BALLOON_MIN_QUEUE_COUNT
        );
    }

    #[test]
    fn balloon_mmio_handler_dispatches_inflate_and_deflate_notifications() {
        let mut memory = pfn_descriptor_memory();
        let inflate_bytes = pfn_payload_bytes(&[50, 51]);
        write_guest_bytes(&mut memory, TEST_PFN_DATA, &inflate_bytes);
        write_inflate_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA, descriptor_len(&inflate_bytes), None),
        );
        write_available_heads(&mut memory, inflate_available_ring(), &[0]);
        write_descriptor(
            &mut memory,
            0,
            TestDescriptor::readable(TEST_PFN_DATA_SPLIT, VIRTIO_BALLOON_PFN_SIZE as u32, None),
        );
        write_available_heads(&mut memory, deflate_available_ring(), &[0]);
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);

        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
            )
            .expect("inflate queue notification should write");
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX),
            )
            .expect("deflate queue notification should write");

        let dispatch = handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect("inflate and deflate notifications should dispatch");

        assert_eq!(
            dispatch.drained_notifications(),
            &[
                VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
                VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX
            ]
        );
        assert_eq!(dispatch.inflate_notifications(), 1);
        assert_eq!(dispatch.deflate_notifications(), 1);
        let inflate_dispatch = dispatch
            .inflate_queue_dispatch()
            .expect("inflate queue should dispatch");
        assert_eq!(inflate_dispatch.completed_descriptors(), 1);
        assert_eq!(
            inflate_dispatch.inflated_page_ranges(),
            &[VirtioBalloonPfnRange {
                start_pfn: 50,
                page_count: 2,
            }]
        );
        let deflate_dispatch = dispatch
            .deflate_queue_dispatch()
            .expect("deflate queue should dispatch");
        assert_eq!(deflate_dispatch.completed_descriptors(), 1);
        assert!(deflate_dispatch.needs_queue_interrupt());
        assert_eq!(read_used_idx(&memory, inflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, inflate_used_ring(), 0), (0, 0));
        assert_eq!(read_used_idx(&memory, deflate_used_ring()), 1);
        assert_eq!(read_used_element(&memory, deflate_used_ring(), 0), (0, 0));
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn balloon_mmio_handler_rejects_unsupported_optional_queue_notification() {
        let mut memory = pfn_descriptor_memory();
        let mut device = balloon_mmio_device(balloon_config(64, false, 1, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);

        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_STATS_QUEUE_INDEX),
            )
            .expect("statistics queue notification should write");

        let error = handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect_err("statistics queue notification should fail closed");

        assert!(matches!(
            error,
            VirtioBalloonDeviceNotificationError::UnsupportedQueue {
                queue_index: VIRTIO_BALLOON_STATS_QUEUE_INDEX,
                ..
            }
        ));
        assert_eq!(
            error.drained_notifications(),
            &[VIRTIO_BALLOON_STATS_QUEUE_INDEX]
        );
        assert_eq!(
            error.to_string(),
            "virtio-balloon queue notification for unsupported queue 2"
        );
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn balloon_mmio_handler_status_reset_clears_active_queues_and_notifications() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        activate_handler(handler);
        handler
            .write_register(
                VirtioMmioRegister::QueueNotify,
                queue_index_u32(VIRTIO_BALLOON_INFLATE_QUEUE_INDEX),
            )
            .expect("inflate queue notification should write");

        handler
            .write_register(VirtioMmioRegister::Status, 0)
            .expect("status reset should write");

        assert!(!handler.is_device_activated());
        assert!(!handler.activation_handler().is_activated());
        assert!(handler.pending_queue_notifications().is_empty());
    }

    #[test]
    fn mmio_registration_exposes_identity_features_and_queues() {
        let mut device = balloon_mmio_device(balloon_config(64, true, 1, true, true));
        let registration = device.registration();

        assert_eq!(registration.region_id(), TEST_BALLOON_MMIO_REGION_ID);
        assert_eq!(registration.address(), TEST_BALLOON_MMIO_BASE);

        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        assert_eq!(
            handler.device_registers().device_id(),
            VIRTIO_BALLOON_DEVICE_ID
        );
        assert!(has_feature(
            handler.device_registers().device_features(),
            VIRTIO_FEATURE_VERSION_1
        ));
        assert!(has_feature(
            handler.device_registers().device_features(),
            VIRTIO_BALLOON_F_DEFLATE_ON_OOM
        ));
        assert_eq!(
            handler.queue_registers().queue_count(),
            VIRTIO_BALLOON_MAX_QUEUE_COUNT
        );
        assert_eq!(
            handler
                .queue_registers()
                .queue(0)
                .expect("inflate queue should exist")
                .max_size(),
            VIRTIO_BALLOON_QUEUE_SIZE
        );
    }

    #[test]
    fn mmio_config_space_reads_firecracker_layout() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));

        assert_eq!(
            read_mmio_config(&mut device, 0, 4).as_slice(),
            &(64 * VIRTIO_BALLOON_MIB_TO_4K_PAGES).to_le_bytes()
        );
        assert_eq!(
            read_mmio_config(&mut device, 4, 4).as_slice(),
            &[0, 0, 0, 0]
        );
        assert_eq!(
            read_mmio_config(&mut device, 8, 4).as_slice(),
            &[0, 0, 0, 0]
        );
        let mut config_space = [0; VIRTIO_BALLOON_CONFIG_SPACE_SIZE];
        config_space[0..4].copy_from_slice(read_mmio_config(&mut device, 0, 4).as_slice());
        config_space[4..8].copy_from_slice(read_mmio_config(&mut device, 4, 4).as_slice());
        config_space[8..12].copy_from_slice(read_mmio_config(&mut device, 8, 4).as_slice());
        assert_eq!(
            config_space,
            VirtioBalloonConfigSpace::from_config(balloon_config(64, false, 0, false, false))
                .expect("balloon config space should build")
                .to_le_bytes()
        );
    }

    #[test]
    fn mmio_config_space_keeps_guest_writes_in_local_state() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");

        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("acknowledge status should write");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("driver status should write");

        write_mmio_config(&mut device, 4, &0x0102_0304_u32.to_le_bytes());
        write_mmio_config(&mut device, 8, &0x0506_0708_u32.to_le_bytes());

        assert_eq!(
            read_mmio_config(&mut device, 4, 4).as_slice(),
            &0x0102_0304_u32.to_le_bytes()
        );
        assert_eq!(
            read_mmio_config(&mut device, 8, 4).as_slice(),
            &0x0506_0708_u32.to_le_bytes()
        );
    }

    #[test]
    fn mmio_config_space_rejects_out_of_bounds_accesses() {
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let read_access = device
            .dispatcher()
            .lookup(
                TEST_BALLOON_MMIO_BASE
                    .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 12)
                    .expect("test MMIO address should not overflow"),
                1,
            )
            .expect("access should resolve inside MMIO window");
        let read = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered")
            .read_access(read_access);
        assert_eq!(
            read,
            Err(VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigRead { offset: 12, len: 1 })
        );

        let write_access = device
            .dispatcher()
            .lookup(
                TEST_BALLOON_MMIO_BASE
                    .checked_add(VIRTIO_MMIO_DEVICE_CONFIG_OFFSET + 11)
                    .expect("test MMIO address should not overflow"),
                2,
            )
            .expect("access should resolve inside MMIO window");
        let write_data = MmioAccessBytes::new(&[1, 2]).expect("test bytes should build");
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");
        handler
            .write_register(VirtioMmioRegister::Status, VIRTIO_DEVICE_STATUS_ACKNOWLEDGE)
            .expect("acknowledge status should write");
        handler
            .write_register(
                VirtioMmioRegister::Status,
                VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
            )
            .expect("driver status should write");
        let write = handler.write_access(write_access, write_data);
        assert_eq!(
            write,
            Err(
                VirtioMmioRegisterHandlerError::UnsupportedDeviceConfigWrite { offset: 11, len: 2 }
            )
        );
    }

    #[test]
    fn mmio_queue_notifications_are_recorded_until_balloon_dispatch() {
        let mut memory = pfn_descriptor_memory();
        let mut device = balloon_mmio_device(balloon_config(64, false, 0, false, false));
        let handler = device
            .dispatcher_mut()
            .handler_mut::<VirtioBalloonMmioHandler>(TEST_BALLOON_MMIO_REGION_ID)
            .expect("balloon handler should be registered");

        activate_handler(handler);
        handler
            .write_register(VirtioMmioRegister::QueueNotify, 0)
            .expect("queue notification should be accepted");

        assert_eq!(handler.pending_queue_notifications(), vec![0]);
        let dispatch = handler
            .dispatch_balloon_queue_notifications(&mut memory)
            .expect("queue notification should drain");
        assert_eq!(dispatch.drained_notifications(), &[0]);
        assert!(!dispatch.needs_queue_interrupt());
        assert_eq!(
            handler
                .read_register(VirtioMmioRegister::InterruptStatus)
                .expect("interrupt status should read"),
            0
        );
        assert!(handler.pending_queue_notifications().is_empty());
    }
}
