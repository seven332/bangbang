use std::collections::TryReserveError;
use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;
use std::sync::Arc;

use bangbang_runtime::BackendError;
use bangbang_runtime::memory::{GuestMemory, GuestMemoryRange, GuestMemoryRegion};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HvfMemoryPermissions {
    bits: crate::ffi::HvMemoryFlags,
}

impl HvfMemoryPermissions {
    pub const READ: Self = Self {
        bits: crate::ffi::HV_MEMORY_READ,
    };
    pub const WRITE: Self = Self {
        bits: crate::ffi::HV_MEMORY_WRITE,
    };
    pub const EXECUTE: Self = Self {
        bits: crate::ffi::HV_MEMORY_EXEC,
    };
    pub const GUEST_RAM: Self = Self {
        bits: crate::ffi::HV_MEMORY_READ | crate::ffi::HV_MEMORY_WRITE | crate::ffi::HV_MEMORY_EXEC,
    };

    pub const fn new(read: bool, write: bool, execute: bool) -> Self {
        let mut bits = 0;

        if read {
            bits |= crate::ffi::HV_MEMORY_READ;
        }
        if write {
            bits |= crate::ffi::HV_MEMORY_WRITE;
        }
        if execute {
            bits |= crate::ffi::HV_MEMORY_EXEC;
        }

        Self { bits }
    }

    pub(crate) const fn bits(self) -> crate::ffi::HvMemoryFlags {
        self.bits
    }

    const fn is_empty(self) -> bool {
        self.bits == 0
    }
}

impl Default for HvfMemoryPermissions {
    fn default() -> Self {
        Self::GUEST_RAM
    }
}

#[derive(Debug)]
pub enum HvfGuestMemoryMappingError {
    Backend(BackendError),
    InvalidState(&'static str),
    EmptyGuestMemory,
    EmptyPermissions,
    InvalidHostPageSize,
    SizeTooLarge {
        range: GuestMemoryRange,
    },
    UnalignedGuestRange {
        range: GuestMemoryRange,
        alignment: u64,
    },
    UnalignedHostAddress {
        range: GuestMemoryRange,
        alignment: usize,
    },
    NullHostAddress {
        range: GuestMemoryRange,
    },
    UnalignedHostSize {
        range: GuestMemoryRange,
        host_size: usize,
        alignment: usize,
    },
    HostSizeMismatch {
        range: GuestMemoryRange,
        host_size: usize,
        expected_size: usize,
    },
    HostMapping {
        label: String,
        range: GuestMemoryRange,
        source: Box<HvfGuestMemoryMappingError>,
    },
    MappingMetadataAllocationFailed {
        source: TryReserveError,
    },
    MapFailed {
        range: GuestMemoryRange,
        source: BackendError,
        cleanup_failures: Vec<HvfGuestMemoryUnmapFailure>,
    },
    UnmapFailed {
        failures: Vec<HvfGuestMemoryUnmapFailure>,
    },
}

impl fmt::Display for HvfGuestMemoryMappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => write!(f, "{source}"),
            Self::InvalidState(message) => {
                write!(f, "invalid guest memory mapping state: {message}")
            }
            Self::EmptyGuestMemory => f.write_str("guest memory must contain at least one region"),
            Self::EmptyPermissions => f.write_str("guest memory permissions must not be empty"),
            Self::InvalidHostPageSize => f.write_str("host page size is unavailable or invalid"),
            Self::SizeTooLarge { range } => {
                write!(
                    f,
                    "guest memory range {range} is too large to map on this host"
                )
            }
            Self::UnalignedGuestRange { range, alignment } => {
                write!(
                    f,
                    "guest memory range {range} is not aligned to {alignment} bytes"
                )
            }
            Self::UnalignedHostAddress { range, alignment } => {
                write!(
                    f,
                    "host address for guest memory range {range} is not aligned to {alignment} bytes"
                )
            }
            Self::NullHostAddress { range } => {
                write!(f, "host address for guest memory range {range} is null")
            }
            Self::UnalignedHostSize {
                range,
                host_size,
                alignment,
            } => {
                write!(
                    f,
                    "host mapping for guest memory range {range} has size {host_size}, which is not aligned to {alignment} bytes"
                )
            }
            Self::HostSizeMismatch {
                range,
                host_size,
                expected_size,
            } => {
                write!(
                    f,
                    "host mapping for guest memory range {range} has size {host_size}, expected {expected_size}"
                )
            }
            Self::HostMapping {
                label,
                range,
                source,
            } => {
                write!(
                    f,
                    "failed to map host-backed guest memory range {range} for {label}: {source}"
                )
            }
            Self::MappingMetadataAllocationFailed { source } => {
                write!(
                    f,
                    "failed to reserve guest memory mapping metadata: {source}"
                )
            }
            Self::MapFailed {
                range,
                source,
                cleanup_failures,
            } => {
                if cleanup_failures.is_empty() {
                    write!(f, "failed to map guest memory range {range}: {source}")
                } else {
                    write!(
                        f,
                        "failed to map guest memory range {range}: {source}; also failed to unmap {} previously mapped region(s)",
                        cleanup_failures.len()
                    )
                }
            }
            Self::UnmapFailed { failures } => {
                write!(
                    f,
                    "failed to unmap {} guest memory region(s)",
                    failures.len()
                )
            }
        }
    }
}

impl std::error::Error for HvfGuestMemoryMappingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) | Self::MapFailed { source, .. } => Some(source),
            Self::HostMapping { source, .. } => Some(source),
            Self::MappingMetadataAllocationFailed { source } => Some(source),
            Self::UnmapFailed { failures } => failures
                .first()
                .map(|failure| &failure.source as &(dyn std::error::Error + 'static)),
            Self::InvalidState(_)
            | Self::EmptyGuestMemory
            | Self::EmptyPermissions
            | Self::InvalidHostPageSize
            | Self::SizeTooLarge { .. }
            | Self::UnalignedGuestRange { .. }
            | Self::UnalignedHostAddress { .. }
            | Self::NullHostAddress { .. }
            | Self::UnalignedHostSize { .. }
            | Self::HostSizeMismatch { .. } => None,
        }
    }
}

impl From<BackendError> for HvfGuestMemoryMappingError {
    fn from(source: BackendError) -> Self {
        Self::Backend(source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfGuestMemoryUnmapFailure {
    range: GuestMemoryRange,
    source: BackendError,
}

impl HvfGuestMemoryUnmapFailure {
    pub const fn range(&self) -> GuestMemoryRange {
        self.range
    }

    pub const fn source(&self) -> &BackendError {
        &self.source
    }
}

#[derive(Debug)]
pub(crate) struct HvfGuestMemoryMapping {
    memory: Option<GuestMemory>,
    host_memory: Vec<HvfHostMemoryMapping>,
    mapped_regions: Vec<HvfMappedGuestMemoryRegion>,
    mapper: Arc<dyn HvfMemoryMapper>,
}

impl HvfGuestMemoryMapping {
    #[cfg(test)]
    pub(crate) fn map_with_mapper(
        memory: GuestMemory,
        permissions: HvfMemoryPermissions,
        mapper: Arc<dyn HvfMemoryMapper>,
    ) -> Result<Self, Box<FailedGuestMemoryMapping>> {
        Self::map_with_mapper_and_host_mappings(memory, permissions, Vec::new(), mapper)
    }

    pub(crate) fn map_with_mapper_and_host_mappings(
        memory: GuestMemory,
        permissions: HvfMemoryPermissions,
        host_memory: Vec<HvfHostMemoryMapping>,
        mapper: Arc<dyn HvfMemoryMapper>,
    ) -> Result<Self, Box<FailedGuestMemoryMapping>> {
        let mut mapping = Self {
            memory: Some(memory),
            host_memory,
            mapped_regions: Vec::new(),
            mapper,
        };

        match mapping.map_all(permissions) {
            Ok(()) => Ok(mapping),
            Err(error) => Err(Box::new(FailedGuestMemoryMapping { mapping, error })),
        }
    }

    pub(crate) fn unmap_all(&mut self) -> Result<(), HvfGuestMemoryMappingError> {
        let failures = self.unmap_mapped_regions();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(HvfGuestMemoryMappingError::UnmapFailed { failures })
        }
    }

    pub(crate) fn has_mapped_regions(&self) -> bool {
        !self.mapped_regions.is_empty()
    }

    pub(crate) fn memory(&self) -> Result<&GuestMemory, HvfGuestMemoryMappingError> {
        self.memory
            .as_ref()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                "guest memory owner is missing",
            ))
    }

    pub(crate) fn memory_mut(&mut self) -> Result<&mut GuestMemory, HvfGuestMemoryMappingError> {
        self.memory
            .as_mut()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                "guest memory owner is missing",
            ))
    }

    // HVF destroys guest mappings with the VM. Use this only after `hv_vm_destroy`
    // succeeds following an earlier unmap failure.
    pub(crate) fn release_after_vm_destroy(mut self) {
        self.mapped_regions.clear();
    }

    fn map_all(
        &mut self,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let memory = self
            .memory
            .as_ref()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                "guest memory owner is missing",
            ))?;
        let page_size = host_page_size()?;
        let requests = validated_map_requests(memory, permissions, page_size)?;
        let host_requests = validated_host_map_requests(&self.host_memory, page_size)?;
        self.mapped_regions
            .try_reserve_exact(requests.len() + host_requests.len())
            .map_err(
                |source| HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source },
            )?;

        for request in requests {
            self.map_validated_request(request, permissions)?;
        }

        for request in host_requests {
            self.map_validated_request(request.request, request.permissions)
                .map_err(|source| {
                    HvfGuestMemoryMappingError::host_mapping(
                        &request.label,
                        request.request.range,
                        source,
                    )
                })?;
        }

        Ok(())
    }

    fn map_validated_request(
        &mut self,
        request: HvfMemoryMapRequest,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let mapped_region = request.mapped_region();
        if let Err(source) = self.mapper.map_region(request, permissions) {
            let cleanup_failures = self.unmap_mapped_regions();
            return Err(HvfGuestMemoryMappingError::MapFailed {
                range: request.range,
                source,
                cleanup_failures,
            });
        }

        self.mapped_regions.push(mapped_region);
        Ok(())
    }

    fn unmap_mapped_regions(&mut self) -> Vec<HvfGuestMemoryUnmapFailure> {
        let mut failures = Vec::new();
        let mut remaining_regions = Vec::new();

        while let Some(mapped_region) = self.mapped_regions.pop() {
            if let Err(source) = self.mapper.unmap_region(mapped_region) {
                failures.push(HvfGuestMemoryUnmapFailure {
                    range: mapped_region.range,
                    source,
                });
                remaining_regions.push(mapped_region);
            }
        }

        while let Some(mapped_region) = remaining_regions.pop() {
            self.mapped_regions.push(mapped_region);
        }

        failures
    }
}

impl Drop for HvfGuestMemoryMapping {
    fn drop(&mut self) {
        if self.unmap_all().is_err() {
            if let Some(memory) = self.memory.take() {
                std::mem::forget(memory);
            }

            let host_memory = std::mem::take(&mut self.host_memory);
            std::mem::forget(host_memory);
        }
    }
}

impl HvfGuestMemoryMappingError {
    fn host_mapping(
        label: &str,
        range: GuestMemoryRange,
        source: HvfGuestMemoryMappingError,
    ) -> Self {
        Self::HostMapping {
            label: label.to_string(),
            range,
            source: Box::new(source),
        }
    }
}

#[derive(Debug)]
pub(crate) struct FailedGuestMemoryMapping {
    pub(crate) mapping: HvfGuestMemoryMapping,
    pub(crate) error: HvfGuestMemoryMappingError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvfMemoryMapRequest {
    range: GuestMemoryRange,
    host_address: usize,
    guest_address: u64,
    size: usize,
}

#[derive(Debug)]
pub(crate) struct HvfHostMemoryMapping {
    label: String,
    memory: GuestMemory,
    permissions: HvfMemoryPermissions,
}

impl HvfHostMemoryMapping {
    pub(crate) fn new(
        label: impl Into<String>,
        memory: GuestMemory,
        permissions: HvfMemoryPermissions,
    ) -> Self {
        Self {
            label: label.into(),
            memory,
            permissions,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HvfValidatedHostMemoryMapRequest {
    label: String,
    request: HvfMemoryMapRequest,
    permissions: HvfMemoryPermissions,
}

impl HvfMemoryMapRequest {
    #[cfg(test)]
    pub(crate) const fn range(self) -> GuestMemoryRange {
        self.range
    }

    const fn mapped_region(self) -> HvfMappedGuestMemoryRegion {
        HvfMappedGuestMemoryRegion {
            range: self.range,
            guest_address: self.guest_address,
            size: self.size,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvfMappedGuestMemoryRegion {
    range: GuestMemoryRange,
    guest_address: u64,
    size: usize,
}

pub(crate) trait HvfMemoryMapper: fmt::Debug + Send + Sync {
    fn map_region(
        &self,
        request: HvfMemoryMapRequest,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), BackendError>;

    fn unmap_region(&self, mapped_region: HvfMappedGuestMemoryRegion) -> Result<(), BackendError>;
}

#[derive(Debug, Default)]
pub(crate) struct RealHvfMemoryMapper;

impl HvfMemoryMapper for RealHvfMemoryMapper {
    fn map_region(
        &self,
        request: HvfMemoryMapRequest,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), BackendError> {
        let host_address = NonNull::<c_void>::new(request.host_address as *mut c_void).ok_or(
            BackendError::InvalidState("validated guest memory host address is null"),
        )?;

        crate::ffi::map_memory(
            host_address,
            request.guest_address,
            request.size,
            permissions.bits(),
        )
    }

    fn unmap_region(&self, mapped_region: HvfMappedGuestMemoryRegion) -> Result<(), BackendError> {
        crate::ffi::unmap_memory(mapped_region.guest_address, mapped_region.size)
    }
}

fn validated_map_requests(
    memory: &GuestMemory,
    permissions: HvfMemoryPermissions,
    page_size: u64,
) -> Result<Vec<HvfMemoryMapRequest>, HvfGuestMemoryMappingError> {
    if permissions.is_empty() {
        return Err(HvfGuestMemoryMappingError::EmptyPermissions);
    }

    if memory.regions().is_empty() {
        return Err(HvfGuestMemoryMappingError::EmptyGuestMemory);
    }

    let mut requests = Vec::new();
    requests
        .try_reserve_exact(memory.regions().len())
        .map_err(|source| HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source })?;

    for region in memory.regions() {
        requests.push(validated_region_map_request(region, page_size)?);
    }

    Ok(requests)
}

fn validated_host_map_requests(
    host_mappings: &[HvfHostMemoryMapping],
    page_size: u64,
) -> Result<Vec<HvfValidatedHostMemoryMapRequest>, HvfGuestMemoryMappingError> {
    let mut requests = Vec::new();
    let request_count = host_mappings
        .iter()
        .map(|mapping| mapping.memory.regions().len())
        .sum();
    requests
        .try_reserve_exact(request_count)
        .map_err(|source| HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source })?;

    for mapping in host_mappings {
        if mapping.memory.regions().is_empty() {
            return Err(HvfGuestMemoryMappingError::EmptyGuestMemory);
        }

        for region in mapping.memory.regions() {
            requests.push(validate_host_map_request(
                &mapping.label,
                mapping.permissions,
                region,
                page_size,
            )?);
        }
    }

    Ok(requests)
}

fn validate_host_map_request(
    label: &str,
    permissions: HvfMemoryPermissions,
    region: &GuestMemoryRegion,
    page_size: u64,
) -> Result<HvfValidatedHostMemoryMapRequest, HvfGuestMemoryMappingError> {
    let range = region.range();

    if permissions.is_empty() {
        return Err(HvfGuestMemoryMappingError::host_mapping(
            label,
            range,
            HvfGuestMemoryMappingError::EmptyPermissions,
        ));
    }

    let request = validate_map_request(
        range,
        region.host_address().as_ptr() as usize,
        region.host_size(),
        page_size,
    )
    .map_err(|source| HvfGuestMemoryMappingError::host_mapping(label, range, source))?;

    Ok(HvfValidatedHostMemoryMapRequest {
        label: label.to_string(),
        request,
        permissions,
    })
}

fn validated_region_map_request(
    region: &GuestMemoryRegion,
    page_size: u64,
) -> Result<HvfMemoryMapRequest, HvfGuestMemoryMappingError> {
    validate_map_request(
        region.range(),
        region.host_address().as_ptr() as usize,
        region.host_size(),
        page_size,
    )
}

fn validate_map_request(
    range: GuestMemoryRange,
    host_address: usize,
    host_size: usize,
    page_size: u64,
) -> Result<HvfMemoryMapRequest, HvfGuestMemoryMappingError> {
    validate_host_page_size(page_size)?;
    let alignment =
        usize::try_from(page_size).map_err(|_| HvfGuestMemoryMappingError::InvalidHostPageSize)?;

    if range.validate_alignment(page_size).is_err() {
        return Err(HvfGuestMemoryMappingError::UnalignedGuestRange {
            range,
            alignment: page_size,
        });
    }

    let size = usize::try_from(range.size())
        .map_err(|_| HvfGuestMemoryMappingError::SizeTooLarge { range })?;

    if host_address == 0 {
        return Err(HvfGuestMemoryMappingError::NullHostAddress { range });
    }

    if !host_size.is_multiple_of(alignment) {
        return Err(HvfGuestMemoryMappingError::UnalignedHostSize {
            range,
            host_size,
            alignment,
        });
    }

    if host_size != size {
        return Err(HvfGuestMemoryMappingError::HostSizeMismatch {
            range,
            host_size,
            expected_size: size,
        });
    }

    if !host_address.is_multiple_of(alignment) {
        return Err(HvfGuestMemoryMappingError::UnalignedHostAddress { range, alignment });
    }

    Ok(HvfMemoryMapRequest {
        range,
        host_address,
        guest_address: range.start().raw_value(),
        size,
    })
}

pub(crate) fn host_page_size() -> Result<u64, HvfGuestMemoryMappingError> {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no pointer arguments and does not
    // require process-local invariants from Rust.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page_size =
        u64::try_from(page_size).map_err(|_| HvfGuestMemoryMappingError::InvalidHostPageSize)?;

    validate_host_page_size(page_size)?;
    Ok(page_size)
}

fn validate_host_page_size(page_size: u64) -> Result<(), HvfGuestMemoryMappingError> {
    if page_size == 0 || !page_size.is_power_of_two() {
        Err(HvfGuestMemoryMappingError::InvalidHostPageSize)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };

    use super::{
        HvfGuestMemoryMapping, HvfGuestMemoryMappingError, HvfHostMemoryMapping,
        HvfMappedGuestMemoryRegion, HvfMemoryMapRequest, HvfMemoryMapper, HvfMemoryPermissions,
        host_page_size, validate_map_request,
    };
    use crate::memory::FailedGuestMemoryMapping;

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size)
            .expect("guest memory range should be valid for test")
    }

    fn memory_for_ranges(ranges: Vec<GuestMemoryRange>) -> GuestMemory {
        let layout =
            GuestMemoryLayout::new(ranges).expect("guest memory layout should be valid for test");

        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed")
    }

    fn page_size() -> u64 {
        host_page_size().expect("host page size should be available for tests")
    }

    #[test]
    fn permission_bits_match_hvf_flags() {
        assert_eq!(
            HvfMemoryPermissions::READ.bits(),
            crate::ffi::HV_MEMORY_READ
        );
        assert_eq!(
            HvfMemoryPermissions::WRITE.bits(),
            crate::ffi::HV_MEMORY_WRITE
        );
        assert_eq!(
            HvfMemoryPermissions::EXECUTE.bits(),
            crate::ffi::HV_MEMORY_EXEC
        );
        assert_eq!(
            HvfMemoryPermissions::new(true, true, true),
            HvfMemoryPermissions::GUEST_RAM
        );
        assert_eq!(
            HvfMemoryPermissions::default(),
            HvfMemoryPermissions::GUEST_RAM
        );
    }

    #[test]
    fn validate_map_request_rejects_unaligned_guest_range() {
        let page_size = page_size();
        let guest_range = range(page_size, page_size - 1);

        let err = validate_map_request(
            guest_range,
            usize::try_from(page_size).expect("page size should fit usize"),
            usize::try_from(page_size - 1).expect("range size should fit usize"),
            page_size,
        )
        .expect_err("unaligned guest range should be rejected");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::UnalignedGuestRange { range, alignment }
                if range == guest_range && alignment == page_size
        ));
    }

    #[test]
    fn validate_map_request_rejects_unaligned_host_address() {
        let page_size = page_size();
        let alignment = usize::try_from(page_size).expect("page size should fit usize");
        let guest_range = range(0, page_size);

        let err = validate_map_request(guest_range, alignment / 2, alignment, page_size)
            .expect_err("unaligned host address should be rejected");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::UnalignedHostAddress { range, alignment: error_alignment }
                if range == guest_range && error_alignment == alignment
        ));
    }

    #[test]
    fn validate_map_request_rejects_null_host_address() {
        let page_size = page_size();
        let alignment = usize::try_from(page_size).expect("page size should fit usize");
        let guest_range = range(0, page_size);

        let err = validate_map_request(guest_range, 0, alignment, page_size)
            .expect_err("null host address should be rejected");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::NullHostAddress { range } if range == guest_range
        ));
    }

    #[test]
    fn validate_map_request_rejects_unaligned_host_size() {
        let page_size = page_size();
        let alignment = usize::try_from(page_size).expect("page size should fit usize");
        let guest_range = range(0, page_size);
        let host_size = alignment + 1;

        let err = validate_map_request(guest_range, alignment, host_size, page_size)
            .expect_err("unaligned host size should be rejected");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::UnalignedHostSize {
                range,
                host_size: error_host_size,
                alignment: error_alignment,
            } if range == guest_range
                && error_host_size == host_size
                && error_alignment == alignment
        ));
    }

    #[test]
    fn validate_map_request_rejects_host_size_mismatch() {
        let page_size = page_size();
        let alignment = usize::try_from(page_size).expect("page size should fit usize");
        let guest_range = range(0, page_size);

        let err = validate_map_request(guest_range, alignment, alignment * 2, page_size)
            .expect_err("host size mismatch should be rejected");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::HostSizeMismatch {
                range,
                host_size,
                expected_size,
            } if range == guest_range && host_size == alignment * 2 && expected_size == alignment
        ));
    }

    #[test]
    fn mapping_rejects_empty_permissions_before_map_calls() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size)]);
        let mapper = Arc::new(RecordingMapper::default());

        let failure = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::new(false, false, false),
            mapper.clone(),
        )
        .expect_err("empty permissions should be rejected");

        assert!(matches!(
            failure.error,
            HvfGuestMemoryMappingError::EmptyPermissions
        ));
        assert_eq!(mapper.map_count(), 0);
        assert_eq!(mapper.unmap_count(), 0);
    }

    #[test]
    fn mapping_maps_regions_and_drop_unmaps_in_reverse_order() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size), range(page_size, page_size)]);
        let mapper = Arc::new(RecordingMapper::default());

        let mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("guest memory mapping should succeed");

        assert_eq!(mapping.mapped_regions.len(), 2);
        let maps = mapper.maps();
        let mut mapped_ranges = maps.iter().map(|(request, _)| request.range);
        assert_eq!(mapped_ranges.next(), Some(range(0, page_size)));
        assert_eq!(mapped_ranges.next(), Some(range(page_size, page_size)));
        assert_eq!(mapped_ranges.next(), None);

        drop(mapping);

        let unmaps = mapper.unmaps();
        let mut unmapped_ranges = unmaps.iter().map(|mapped_region| mapped_region.range);
        assert_eq!(unmapped_ranges.next(), Some(range(page_size, page_size)));
        assert_eq!(unmapped_ranges.next(), Some(range(0, page_size)));
        assert_eq!(unmapped_ranges.next(), None);
    }

    #[test]
    fn mapping_maps_host_mappings_after_guest_memory_with_own_permissions() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size)]);
        let readonly_pmem_range = range(page_size * 8, page_size);
        let writable_pmem_range = range(page_size * 9, page_size);
        let readonly_host = memory_for_ranges(vec![readonly_pmem_range]);
        let writable_host = memory_for_ranges(vec![writable_pmem_range]);
        let writable_pmem_permissions = HvfMemoryPermissions::new(true, true, false);
        let host_mappings = vec![
            HvfHostMemoryMapping::new(
                "pmem device `readonly`",
                readonly_host,
                HvfMemoryPermissions::READ,
            ),
            HvfHostMemoryMapping::new(
                "pmem device `writable`",
                writable_host,
                writable_pmem_permissions,
            ),
        ];
        let mapper = Arc::new(RecordingMapper::default());

        let mapping = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            mapper.clone(),
        )
        .expect("guest and host-backed memory mapping should succeed");

        assert_eq!(mapping.mapped_regions.len(), 3);
        let maps = mapper.maps();
        let mut mapped = maps
            .iter()
            .map(|(request, permissions)| (request.range, request.guest_address, *permissions));
        assert_eq!(
            mapped.next(),
            Some((range(0, page_size), 0, HvfMemoryPermissions::GUEST_RAM))
        );
        assert_eq!(
            mapped.next(),
            Some((
                readonly_pmem_range,
                readonly_pmem_range.start().raw_value(),
                HvfMemoryPermissions::READ
            ))
        );
        assert_eq!(
            mapped.next(),
            Some((
                writable_pmem_range,
                writable_pmem_range.start().raw_value(),
                writable_pmem_permissions
            ))
        );
        assert_eq!(mapped.next(), None);

        drop(mapping);

        let unmaps = mapper.unmaps();
        let mut unmapped_ranges = unmaps.iter().map(|mapped_region| mapped_region.range);
        assert_eq!(unmapped_ranges.next(), Some(writable_pmem_range));
        assert_eq!(unmapped_ranges.next(), Some(readonly_pmem_range));
        assert_eq!(unmapped_ranges.next(), Some(range(0, page_size)));
        assert_eq!(unmapped_ranges.next(), None);
    }

    #[test]
    fn host_mapping_validation_error_identifies_label_and_range_without_path() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size)]);
        let pmem_range = range(page_size * 8, page_size);
        let host_memory = memory_for_ranges(vec![pmem_range]);
        let host_mappings = vec![HvfHostMemoryMapping::new(
            "pmem device `pmem0`",
            host_memory,
            HvfMemoryPermissions::new(false, false, false),
        )];
        let mapper = Arc::new(RecordingMapper::default());

        let failure = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            mapper.clone(),
        )
        .expect_err("empty host mapping permissions should be rejected");

        assert!(matches!(
            &failure.error,
            HvfGuestMemoryMappingError::HostMapping {
                label,
                range,
                source,
            } if label == "pmem device `pmem0`"
                && *range == pmem_range
                && matches!(
                    source.as_ref(),
                    HvfGuestMemoryMappingError::EmptyPermissions
                )
        ));
        let message = failure.error.to_string();
        assert!(message.contains("pmem device `pmem0`"));
        assert!(message.contains(&pmem_range.to_string()));
        assert!(!message.contains('/'));
        assert_eq!(mapper.map_count(), 0);
        assert_eq!(mapper.unmap_count(), 0);
    }

    #[test]
    fn partial_map_failure_unmaps_previously_mapped_regions() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size), range(page_size, page_size)]);
        let failed_range = range(page_size, page_size);
        let mapper = Arc::new(RecordingMapper::new(Some(2), false));

        let failure = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect_err("second map should fail");

        assert!(matches!(
            failure.error,
            HvfGuestMemoryMappingError::MapFailed {
                range,
                cleanup_failures,
                ..
            } if range == failed_range && cleanup_failures.is_empty()
        ));
        assert!(!failure.mapping.has_mapped_regions());
        assert_eq!(mapper.map_count(), 2);

        let unmaps = mapper.unmaps();
        let mut unmapped_ranges = unmaps.iter().map(|mapped_region| mapped_region.range);
        assert_eq!(unmapped_ranges.next(), Some(range(0, page_size)));
        assert_eq!(unmapped_ranges.next(), None);
    }

    #[test]
    fn partial_map_failure_retains_regions_when_cleanup_fails() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size), range(page_size, page_size)]);
        let failed_range = range(page_size, page_size);
        let mapper = Arc::new(RecordingMapper::new(Some(2), true));

        let failure = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect_err("second map should fail");

        assert!(matches!(
            failure.error,
            HvfGuestMemoryMappingError::MapFailed {
                range,
                cleanup_failures,
                ..
            } if range == failed_range && cleanup_failures.len() == 1
        ));
        assert!(failure.mapping.has_mapped_regions());
        assert_eq!(mapper.map_count(), 2);
        assert_eq!(mapper.unmap_count(), 1);
    }

    #[test]
    fn partial_host_mapping_failure_unmaps_guest_and_previous_host_regions() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size)]);
        let first_pmem_range = range(page_size * 8, page_size);
        let failed_pmem_range = range(page_size * 9, page_size);
        let first_host = memory_for_ranges(vec![first_pmem_range]);
        let second_host = memory_for_ranges(vec![failed_pmem_range]);
        let host_mappings = vec![
            HvfHostMemoryMapping::new(
                "pmem device `pmem0`",
                first_host,
                HvfMemoryPermissions::READ,
            ),
            HvfHostMemoryMapping::new(
                "pmem device `pmem1`",
                second_host,
                HvfMemoryPermissions::READ,
            ),
        ];
        let mapper = Arc::new(RecordingMapper::new(Some(3), false));

        let failure = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            mapper.clone(),
        )
        .expect_err("second host-backed mapping should fail");

        assert!(matches!(
            &failure.error,
            HvfGuestMemoryMappingError::HostMapping {
                label,
                range,
                source,
            } if label == "pmem device `pmem1`"
                && *range == failed_pmem_range
                && matches!(
                    source.as_ref(),
                    HvfGuestMemoryMappingError::MapFailed {
                        range,
                        cleanup_failures,
                        ..
                    } if *range == failed_pmem_range && cleanup_failures.is_empty()
                )
        ));
        assert!(!failure.mapping.has_mapped_regions());
        assert_eq!(mapper.map_count(), 3);

        let unmaps = mapper.unmaps();
        let mut unmapped_ranges = unmaps.iter().map(|mapped_region| mapped_region.range);
        assert_eq!(unmapped_ranges.next(), Some(first_pmem_range));
        assert_eq!(unmapped_ranges.next(), Some(range(0, page_size)));
        assert_eq!(unmapped_ranges.next(), None);
    }

    #[test]
    fn explicit_unmap_keeps_failed_regions_for_retry() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size)]);
        let mapper = Arc::new(RecordingMapper::new(None, true));
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("guest memory mapping should succeed");

        let err = mapping
            .unmap_all()
            .expect_err("first unmap should report failure");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::UnmapFailed { failures } if failures.len() == 1
        ));
        assert!(mapping.has_mapped_regions());

        mapper.set_fail_unmap(false);
        mapping
            .unmap_all()
            .expect("second unmap should clean up retained region");

        assert!(!mapping.has_mapped_regions());
        assert_eq!(mapper.unmap_count(), 2);
    }

    #[test]
    fn explicit_unmap_keeps_failed_host_regions_for_retry() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size)]);
        let host_range = range(page_size * 8, page_size);
        let host_memory = memory_for_ranges(vec![host_range]);
        let mapper = Arc::new(RecordingMapper::new(None, true));
        let host_mappings = vec![HvfHostMemoryMapping::new(
            "pmem device `pmem0`",
            host_memory,
            HvfMemoryPermissions::READ,
        )];
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            mapper.clone(),
        )
        .expect("guest and host-backed memory mapping should succeed");

        let err = mapping
            .unmap_all()
            .expect_err("first unmap should report failures");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::UnmapFailed { failures } if failures.len() == 2
        ));
        assert!(mapping.has_mapped_regions());

        mapper.set_fail_unmap(false);
        mapping
            .unmap_all()
            .expect("second unmap should clean up retained regions");

        assert!(!mapping.has_mapped_regions());
        assert_eq!(mapper.unmap_count(), 4);
    }

    #[test]
    fn release_after_vm_destroy_does_not_unmap_again() {
        let page_size = page_size();
        let memory = memory_for_ranges(vec![range(0, page_size)]);
        let mapper = Arc::new(RecordingMapper::default());
        let mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("guest memory mapping should succeed");

        mapping.release_after_vm_destroy();

        assert_eq!(mapper.unmap_count(), 0);
    }

    #[derive(Debug)]
    struct RecordingMapper {
        state: Mutex<RecordingMapperState>,
    }

    impl Default for RecordingMapper {
        fn default() -> Self {
            Self::new(None, false)
        }
    }

    impl RecordingMapper {
        fn new(fail_map_on: Option<usize>, fail_unmap: bool) -> Self {
            Self {
                state: Mutex::new(RecordingMapperState {
                    maps: Vec::new(),
                    unmaps: Vec::new(),
                    fail_map_on,
                    fail_unmap,
                }),
            }
        }

        fn map_count(&self) -> usize {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .maps
                .len()
        }

        fn unmap_count(&self) -> usize {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .unmaps
                .len()
        }

        fn maps(&self) -> Vec<(HvfMemoryMapRequest, HvfMemoryPermissions)> {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .maps
                .clone()
        }

        fn unmaps(&self) -> Vec<HvfMappedGuestMemoryRegion> {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .unmaps
                .clone()
        }

        fn set_fail_unmap(&self, fail_unmap: bool) {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .fail_unmap = fail_unmap;
        }
    }

    impl HvfMemoryMapper for RecordingMapper {
        fn map_region(
            &self,
            request: HvfMemoryMapRequest,
            permissions: HvfMemoryPermissions,
        ) -> Result<(), bangbang_runtime::BackendError> {
            let mut state = self
                .state
                .lock()
                .expect("state lock should not be poisoned");
            state.maps.push((request, permissions));

            if state.fail_map_on == Some(state.maps.len()) {
                return Err(bangbang_runtime::BackendError::Hypervisor(
                    "injected map failure".to_string(),
                ));
            }

            Ok(())
        }

        fn unmap_region(
            &self,
            mapped_region: HvfMappedGuestMemoryRegion,
        ) -> Result<(), bangbang_runtime::BackendError> {
            let mut state = self
                .state
                .lock()
                .expect("state lock should not be poisoned");
            state.unmaps.push(mapped_region);

            if state.fail_unmap {
                return Err(bangbang_runtime::BackendError::Hypervisor(
                    "injected unmap failure".to_string(),
                ));
            }

            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingMapperState {
        maps: Vec<(HvfMemoryMapRequest, HvfMemoryPermissions)>,
        unmaps: Vec<HvfMappedGuestMemoryRegion>,
        fail_map_on: Option<usize>,
        fail_unmap: bool,
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn mapping_owner_is_send_and_sync() {
        assert_send_sync::<HvfGuestMemoryMapping>();
    }

    #[test]
    fn failed_mapping_keeps_owner_for_cleanup_review() {
        assert_send_sync::<FailedGuestMemoryMapping>();
    }
}
