use std::collections::TryReserveError;
use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use bangbang_runtime::BackendError;
use bangbang_runtime::memory::{
    GuestMemory, GuestMemoryAllocationError, GuestMemoryRange, GuestMemoryRegion,
    GuestMemoryRegionRemovalError,
};
use bangbang_runtime::memory_hotplug::{
    VirtioMemAppliedMutation, VirtioMemMutation, VirtioMemMutationError, VirtioMemMutationExecutor,
    VirtioMemMutationKind, VirtioMemMutationRollbackError,
};
use bangbang_runtime::pmem::PmemBackingMapping;

use crate::dirty::{
    HvfDirtyWriteEpochResetError, HvfDirtyWriteMappingMutationError, HvfDirtyWriteTracker,
    HvfDirtyWriteTrackerStartError, HvfDirtyWriteTrackerStopError,
};

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

    pub(crate) const fn contains(self, permissions: Self) -> bool {
        self.bits & permissions.bits == permissions.bits
    }

    pub(crate) const fn without(self, permissions: Self) -> Self {
        Self {
            bits: self.bits & !permissions.bits,
        }
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
    DynamicRegionAllocationFailed {
        range: GuestMemoryRange,
        source: GuestMemoryAllocationError,
    },
    DynamicRegionMapFailed {
        range: GuestMemoryRange,
        source: BackendError,
        owner_cleanup: Option<GuestMemoryRegionRemovalError>,
    },
    DynamicRegionOverlapsMapped {
        existing: GuestMemoryRange,
        requested: GuestMemoryRange,
    },
    DynamicRegionMissing {
        range: GuestMemoryRange,
    },
    DynamicRegionOwnerMissing {
        range: GuestMemoryRange,
    },
    DynamicRegionRemovalFailed {
        range: GuestMemoryRange,
        source: GuestMemoryRegionRemovalError,
    },
    HostMapping {
        label: String,
        range: GuestMemoryRange,
        source: Box<HvfGuestMemoryMappingError>,
    },
    FlushFailed {
        failures: Vec<HvfGuestMemoryFlushFailure>,
    },
    PmemMappingMissing {
        range: GuestMemoryRange,
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
    DirtyWriteTrackingStop {
        source: HvfDirtyWriteTrackerStopError,
    },
    DirtyWriteMappingMutation {
        source: HvfDirtyWriteMappingMutationError,
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
            Self::DynamicRegionAllocationFailed { range, source } => {
                write!(
                    f,
                    "failed to allocate dynamic guest memory region {range}: {source}"
                )
            }
            Self::DynamicRegionMapFailed {
                range,
                source,
                owner_cleanup,
            } => {
                if let Some(owner_cleanup) = owner_cleanup {
                    write!(
                        f,
                        "failed to map dynamic guest memory region {range}: {source}; also failed to remove the inserted owner: {owner_cleanup}"
                    )
                } else {
                    write!(
                        f,
                        "failed to map dynamic guest memory region {range}: {source}"
                    )
                }
            }
            Self::DynamicRegionOverlapsMapped {
                existing,
                requested,
            } => {
                write!(
                    f,
                    "dynamic guest memory region {requested} overlaps mapped range {existing}"
                )
            }
            Self::DynamicRegionMissing { range } => {
                write!(f, "dynamic guest memory region {range} is not mapped")
            }
            Self::DynamicRegionOwnerMissing { range } => {
                write!(
                    f,
                    "dynamic guest memory region {range} is missing from the guest memory owner"
                )
            }
            Self::DynamicRegionRemovalFailed { range, source } => {
                write!(
                    f,
                    "failed to remove dynamic guest memory region {range} from owner: {source}"
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
            Self::FlushFailed { failures } => match failures.as_slice() {
                [failure] => match failure.range {
                    Some(range) => write!(
                        f,
                        "failed to flush host-backed guest memory range {range} for {}: {}",
                        failure.label, failure.source
                    ),
                    None => write!(
                        f,
                        "failed to flush host-backed guest memory for {}: {}",
                        failure.label, failure.source
                    ),
                },
                [first, ..] => match first.range {
                    Some(range) => write!(
                        f,
                        "failed to flush {} host-backed guest memory mapping(s); first failure at range {range} for {}: {}",
                        failures.len(),
                        first.label,
                        first.source
                    ),
                    None => write!(
                        f,
                        "failed to flush {} host-backed guest memory mapping(s); first failure for {}: {}",
                        failures.len(),
                        first.label,
                        first.source
                    ),
                },
                [] => f.write_str("failed to flush host-backed guest memory mapping(s)"),
            },
            Self::PmemMappingMissing { range } => {
                write!(
                    f,
                    "HVF pmem mapping for guest memory range {range} is missing"
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
            Self::DirtyWriteTrackingStop { source } => {
                write!(
                    f,
                    "failed to stop guest memory dirty-write tracking: {source}"
                )
            }
            Self::DirtyWriteMappingMutation { source } => {
                write!(f, "failed to update tracked guest memory mapping: {source}")
            }
        }
    }
}

impl std::error::Error for HvfGuestMemoryMappingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source)
            | Self::MapFailed { source, .. }
            | Self::DynamicRegionMapFailed { source, .. } => Some(source),
            Self::DynamicRegionAllocationFailed { source, .. } => Some(source),
            Self::DynamicRegionRemovalFailed { source, .. } => Some(source),
            Self::HostMapping { source, .. } => Some(source),
            Self::FlushFailed { failures } => failures
                .first()
                .map(|failure| &failure.source as &(dyn std::error::Error + 'static)),
            Self::MappingMetadataAllocationFailed { source } => Some(source),
            Self::UnmapFailed { failures } => failures
                .first()
                .map(|failure| &failure.source as &(dyn std::error::Error + 'static)),
            Self::DirtyWriteTrackingStop { source } => Some(source),
            Self::DirtyWriteMappingMutation { source } => Some(source),
            Self::InvalidState(_)
            | Self::EmptyGuestMemory
            | Self::EmptyPermissions
            | Self::InvalidHostPageSize
            | Self::SizeTooLarge { .. }
            | Self::UnalignedGuestRange { .. }
            | Self::UnalignedHostAddress { .. }
            | Self::NullHostAddress { .. }
            | Self::UnalignedHostSize { .. }
            | Self::HostSizeMismatch { .. }
            | Self::DynamicRegionOverlapsMapped { .. }
            | Self::DynamicRegionMissing { .. }
            | Self::DynamicRegionOwnerMissing { .. }
            | Self::PmemMappingMissing { .. } => None,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvfGuestMemoryFlushFailure {
    label: String,
    range: Option<GuestMemoryRange>,
    source: BackendError,
}

impl HvfGuestMemoryFlushFailure {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub const fn range(&self) -> Option<GuestMemoryRange> {
        self.range
    }

    pub const fn source(&self) -> &BackendError {
        &self.source
    }
}

#[derive(Debug)]
pub(crate) struct HvfGuestMemoryMapping {
    memory: Option<GuestMemory>,
    state: HvfGuestMemoryMappingState,
}

#[derive(Debug)]
struct HvfGuestMemoryMappingState {
    host_memory: Vec<HvfHostMemoryMapping>,
    host_memory_should_flush: bool,
    host_memory_flushed: bool,
    mapped_regions: Vec<HvfMappedGuestMemoryRegion>,
    dynamic_regions: Vec<GuestMemoryRange>,
    mapper: Arc<dyn HvfMemoryMapper>,
    dirty_write_tracker: Option<Arc<HvfDirtyWriteTracker>>,
    protection_poisoned: bool,
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
            state: HvfGuestMemoryMappingState {
                host_memory,
                host_memory_should_flush: false,
                host_memory_flushed: false,
                mapped_regions: Vec::new(),
                dynamic_regions: Vec::new(),
                mapper,
                dirty_write_tracker: None,
                protection_poisoned: false,
            },
        };

        match mapping.map_all(permissions) {
            Ok(()) => Ok(mapping),
            Err(error) => Err(Box::new(FailedGuestMemoryMapping { mapping, error })),
        }
    }

    pub(crate) fn validate_guest_memory(
        memory: &GuestMemory,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let page_size = host_page_size()?;
        let _requests = validated_map_requests(memory, permissions, page_size)?;

        Ok(())
    }

    pub(crate) fn unmap_all(&mut self) -> Result<(), HvfGuestMemoryMappingError> {
        if !self.state.protection_poisoned {
            self.state
                .stop_dirty_write_tracking()
                .map_err(|source| HvfGuestMemoryMappingError::DirtyWriteTrackingStop { source })?;
        }
        let failures = self.state.unmap_mapped_regions();
        if !failures.is_empty() {
            return Err(HvfGuestMemoryMappingError::UnmapFailed { failures });
        }

        self.state.dirty_write_tracker = None;
        self.state.protection_poisoned = false;
        self.state.flush_host_memory()
    }

    pub(crate) fn has_mapped_regions(&self) -> bool {
        self.state.has_mapped_regions()
    }

    #[cfg(test)]
    fn has_dynamic_regions(&self) -> bool {
        self.state.has_dynamic_regions()
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
        self.state.release_after_vm_destroy();
    }

    pub(crate) fn map_dynamic_region(
        &mut self,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let (memory, state) = self.memory_and_state_mut()?;
        state.map_dynamic_region(memory, range, permissions)
    }

    pub(crate) fn unmap_dynamic_region(
        &mut self,
        range: GuestMemoryRange,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let (memory, state) = self.memory_and_state_mut()?;
        state.unmap_dynamic_region(memory, range)
    }

    pub(crate) fn map_runtime_pmem_mapping(
        &mut self,
        mapping: HvfHostMemoryMapping,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.state.map_runtime_pmem_mapping(mapping)
    }

    pub(crate) fn take_runtime_pmem_mapping(
        &mut self,
        range: GuestMemoryRange,
        flush: bool,
    ) -> Result<HvfHostMemoryMapping, HvfGuestMemoryMappingError> {
        self.state.take_runtime_pmem_mapping(range, flush)
    }

    pub(crate) fn memory_and_virtio_mem_executor_mut(
        &mut self,
        permissions: HvfMemoryPermissions,
    ) -> Result<(&mut GuestMemory, HvfVirtioMemMutationExecutor<'_>), HvfGuestMemoryMappingError>
    {
        let (memory, state) = self.memory_and_state_mut()?;
        Ok((
            memory,
            HvfVirtioMemMutationExecutor::new(state, permissions),
        ))
    }

    pub(crate) fn memory_and_pmem_flush_executor_mut(
        &mut self,
    ) -> Result<(&mut GuestMemory, HvfPmemFlushExecutor<'_>), HvfGuestMemoryMappingError> {
        let (memory, state) = self.memory_and_state_mut()?;
        Ok((memory, HvfPmemFlushExecutor::new(state)))
    }

    pub(crate) fn start_dirty_write_tracking(
        &mut self,
    ) -> Result<Arc<HvfDirtyWriteTracker>, HvfDirtyWriteTrackerStartError> {
        let memory = self
            .memory
            .as_mut()
            .ok_or(HvfDirtyWriteTrackerStartError::InvalidState(
                "guest memory owner is missing",
            ))?;
        let dirty_tracker = memory
            .enable_dirty_tracking()
            .map_err(|_| HvfDirtyWriteTrackerStartError::DirtyBitmap)?;
        let mut owned_mapped_regions = Vec::new();
        owned_mapped_regions
            .try_reserve_exact(memory.regions().len())
            .map_err(|_| {
                HvfDirtyWriteTrackerStartError::AllocationFailed("owned mapping metadata")
            })?;
        for region in memory.regions() {
            let range = region.range();
            let mapped = self
                .state
                .mapped_regions
                .iter()
                .find(|mapped| mapped.range == range)
                .copied()
                .ok_or(HvfDirtyWriteTrackerStartError::InvalidState(
                    "owned guest RAM is not fully mapped",
                ))?;
            owned_mapped_regions.push(mapped);
        }
        self.state
            .start_dirty_write_tracking(&owned_mapped_regions, dirty_tracker)
    }

    pub(crate) fn stop_dirty_write_tracking(
        &mut self,
    ) -> Result<(), HvfDirtyWriteTrackerStopError> {
        self.state.stop_dirty_write_tracking()
    }

    pub(crate) fn reset_dirty_epoch_quiesced(
        &mut self,
    ) -> Result<Option<u64>, HvfDirtyWriteEpochResetError> {
        self.state.reset_dirty_epoch_quiesced()
    }

    pub(crate) fn active_dirty_write_tracker(
        &self,
    ) -> Result<Option<Arc<HvfDirtyWriteTracker>>, BackendError> {
        self.state.active_dirty_write_tracker()
    }

    fn memory_and_state_mut(
        &mut self,
    ) -> Result<(&mut GuestMemory, &mut HvfGuestMemoryMappingState), HvfGuestMemoryMappingError>
    {
        let memory = self
            .memory
            .as_mut()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                "guest memory owner is missing",
            ))?;
        Ok((memory, &mut self.state))
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
        self.state.map_all(memory, permissions)
    }
}

impl HvfGuestMemoryMappingState {
    fn has_mapped_regions(&self) -> bool {
        !self.mapped_regions.is_empty()
    }

    #[cfg(test)]
    fn has_dynamic_regions(&self) -> bool {
        !self.dynamic_regions.is_empty()
    }

    fn flush_host_memory_now(&self) -> Result<(), HvfGuestMemoryMappingError> {
        let mut failures = Vec::new();
        for mapping in &self.host_memory {
            if let Err(source) = mapping.flush() {
                failures.push(source);
            }
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(HvfGuestMemoryMappingError::FlushFailed { failures })
        }
    }

    fn flush_pmem_mapping(
        &self,
        range: GuestMemoryRange,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let mapping = self
            .host_memory
            .iter()
            .find(|mapping| mapping.pmem_range() == Some(range))
            .ok_or(HvfGuestMemoryMappingError::PmemMappingMissing { range })?;

        mapping
            .flush()
            .map_err(|failure| HvfGuestMemoryMappingError::FlushFailed {
                failures: vec![failure],
            })
    }

    fn start_dirty_write_tracking(
        &mut self,
        owned_mapped_regions: &[HvfMappedGuestMemoryRegion],
        dirty_tracker: Arc<bangbang_runtime::memory_dirty::GuestMemoryDirtyTracker>,
    ) -> Result<Arc<HvfDirtyWriteTracker>, HvfDirtyWriteTrackerStartError> {
        if self.protection_poisoned {
            return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "guest memory protection rollback requires VM teardown",
            ));
        }
        if let Some(tracker) = self.dirty_write_tracker.as_ref()
            && tracker.is_active().map_err(|_| {
                HvfDirtyWriteTrackerStartError::InvalidState(
                    "dirty-write tracker state is unavailable",
                )
            })?
        {
            return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "dirty-write tracking is already active",
            ));
        }

        let page_size = host_page_size()
            .map_err(|_| HvfDirtyWriteTrackerStartError::InvalidHostPageSize { page_size: 0 })?;
        match HvfDirtyWriteTracker::start_with_dirty_tracker(
            owned_mapped_regions,
            Arc::clone(&self.mapper),
            page_size,
            dirty_tracker,
        ) {
            Ok(tracker) => {
                self.dirty_write_tracker = Some(Arc::clone(&tracker));
                Ok(tracker)
            }
            Err(source) => {
                if source.requires_vm_teardown() {
                    self.protection_poisoned = true;
                }
                Err(source)
            }
        }
    }

    fn stop_dirty_write_tracking(&mut self) -> Result<(), HvfDirtyWriteTrackerStopError> {
        let Some(tracker) = self.dirty_write_tracker.as_ref() else {
            return Ok(());
        };
        tracker.stop()?;
        self.dirty_write_tracker = None;
        Ok(())
    }

    fn reset_dirty_epoch_quiesced(&mut self) -> Result<Option<u64>, HvfDirtyWriteEpochResetError> {
        if self.protection_poisoned {
            return Err(HvfDirtyWriteEpochResetError::InvalidState(
                "guest memory protection rollback requires VM teardown",
            ));
        }
        let Some(tracker) = self.dirty_write_tracker.as_ref() else {
            return Ok(None);
        };
        match tracker.reset_epoch_quiesced() {
            Ok(epoch) => Ok(Some(epoch)),
            Err(source) => {
                if source.requires_vm_teardown() {
                    self.protection_poisoned = true;
                }
                Err(source)
            }
        }
    }

    fn active_dirty_write_tracker(
        &self,
    ) -> Result<Option<Arc<HvfDirtyWriteTracker>>, BackendError> {
        if self.protection_poisoned {
            return Err(BackendError::InvalidState(
                "guest memory protection rollback requires VM teardown",
            ));
        }
        let Some(tracker) = self.dirty_write_tracker.as_ref() else {
            return Ok(None);
        };
        tracker
            .is_active()
            .map_err(|_| BackendError::InvalidState("dirty-write tracker state is unavailable"))?
            .then(|| Arc::clone(tracker))
            .map_or(Ok(None), |tracker| Ok(Some(tracker)))
    }

    fn validate_dirty_write_mutation_state(&self) -> Result<(), HvfGuestMemoryMappingError> {
        if self.protection_poisoned {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                "guest memory protection rollback requires VM teardown",
            ));
        }
        Ok(())
    }

    fn release_after_vm_destroy(&mut self) {
        self.mapped_regions.clear();
        self.dynamic_regions.clear();
        self.dirty_write_tracker = None;
        self.protection_poisoned = false;
        self.host_memory_should_flush = false;
    }

    fn map_dynamic_region(
        &mut self,
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.validate_dirty_write_mutation_state()?;
        if permissions.is_empty() {
            return Err(HvfGuestMemoryMappingError::EmptyPermissions);
        }

        let page_size = host_page_size()?;
        let insert_index = self.validate_dynamic_map_range(range)?;
        self.mapped_regions.try_reserve_exact(1).map_err(|source| {
            HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source }
        })?;
        self.dynamic_regions
            .try_reserve_exact(1)
            .map_err(
                |source| HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source },
            )?;

        let tracker = self.dirty_write_tracker.as_ref().map(Arc::clone);
        let mut mutation = tracker
            .as_ref()
            .map(|tracker| tracker.begin_mapping_mutation())
            .transpose()
            .map_err(|source| HvfGuestMemoryMappingError::DirtyWriteMappingMutation { source })?;
        let prepared_tracking = mutation
            .as_mut()
            .map(|mutation| mutation.prepare_add(range, permissions))
            .transpose()
            .map_err(|source| HvfGuestMemoryMappingError::DirtyWriteMappingMutation { source })?
            .flatten();

        memory.insert_region(range).map_err(|source| {
            HvfGuestMemoryMappingError::DynamicRegionAllocationFailed { range, source }
        })?;

        let request = match dynamic_region_map_request(memory, range, page_size) {
            Ok(request) => request,
            Err(source) => {
                Self::remove_dynamic_owner_after_failed_map(memory, range)?;
                return Err(source);
            }
        };
        let mapped_region = request.mapped_region(permissions);
        let effective_permissions = if prepared_tracking.is_some() {
            permissions.without(HvfMemoryPermissions::WRITE)
        } else {
            permissions
        };

        if let Err(source) = self.mapper.map_region(request, effective_permissions) {
            let _ = memory.discard_range(range);
            let owner_cleanup = memory.remove_region(range).err();
            return Err(HvfGuestMemoryMappingError::DynamicRegionMapFailed {
                range,
                source,
                owner_cleanup,
            });
        }

        if let (Some(mutation), Some(prepared_tracking)) = (mutation.as_mut(), prepared_tracking) {
            mutation.commit_add(prepared_tracking);
        }

        self.mapped_regions.insert(insert_index, mapped_region);
        self.dynamic_regions.push(range);
        Ok(())
    }

    fn map_existing_dynamic_region(
        &mut self,
        memory: &GuestMemory,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.validate_dirty_write_mutation_state()?;
        if permissions.is_empty() {
            return Err(HvfGuestMemoryMappingError::EmptyPermissions);
        }
        if !guest_memory_contains_region(memory, range) {
            return Err(HvfGuestMemoryMappingError::DynamicRegionOwnerMissing { range });
        }

        let page_size = host_page_size()?;
        let insert_index = self.validate_dynamic_map_range(range)?;
        self.mapped_regions.try_reserve_exact(1).map_err(|source| {
            HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source }
        })?;
        self.dynamic_regions
            .try_reserve_exact(1)
            .map_err(
                |source| HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source },
            )?;

        let tracker = self.dirty_write_tracker.as_ref().map(Arc::clone);
        let mut mutation = tracker
            .as_ref()
            .map(|tracker| tracker.begin_mapping_mutation())
            .transpose()
            .map_err(|source| HvfGuestMemoryMappingError::DirtyWriteMappingMutation { source })?;
        let prepared_tracking = mutation
            .as_mut()
            .map(|mutation| mutation.prepare_add(range, permissions))
            .transpose()
            .map_err(|source| HvfGuestMemoryMappingError::DirtyWriteMappingMutation { source })?
            .flatten();

        let request = dynamic_region_map_request(memory, range, page_size)?;
        let mapped_region = request.mapped_region(permissions);
        let effective_permissions = if prepared_tracking.is_some() {
            permissions.without(HvfMemoryPermissions::WRITE)
        } else {
            permissions
        };
        self.mapper
            .map_region(request, effective_permissions)
            .map_err(
                |source| HvfGuestMemoryMappingError::DynamicRegionMapFailed {
                    range,
                    source,
                    owner_cleanup: None,
                },
            )?;

        if let (Some(mutation), Some(prepared_tracking)) = (mutation.as_mut(), prepared_tracking) {
            mutation.commit_add(prepared_tracking);
        }
        self.mapped_regions.insert(insert_index, mapped_region);
        self.dynamic_regions.push(range);
        Ok(())
    }

    fn unmap_dynamic_region(
        &mut self,
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.unmap_dynamic_region_preserving_owner(memory, range)?;
        let _ = memory.discard_range(range);
        memory.remove_region(range).map_err(|source| {
            HvfGuestMemoryMappingError::DynamicRegionRemovalFailed { range, source }
        })
    }

    fn unmap_dynamic_region_preserving_owner(
        &mut self,
        memory: &GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.validate_dirty_write_mutation_state()?;
        let dynamic_index = self
            .dynamic_regions
            .iter()
            .position(|dynamic_range| *dynamic_range == range)
            .ok_or(HvfGuestMemoryMappingError::DynamicRegionMissing { range })?;
        let mapped_index = self
            .mapped_regions
            .iter()
            .position(|mapped_region| mapped_region.range == range)
            .ok_or(HvfGuestMemoryMappingError::DynamicRegionMissing { range })?;
        if !guest_memory_contains_region(memory, range) {
            return Err(HvfGuestMemoryMappingError::DynamicRegionOwnerMissing { range });
        }

        let mapped_region = self
            .mapped_regions
            .get(mapped_index)
            .copied()
            .ok_or(HvfGuestMemoryMappingError::DynamicRegionMissing { range })?;
        let tracker = self.dirty_write_tracker.as_ref().map(Arc::clone);
        let mut mutation = tracker
            .as_ref()
            .map(|tracker| tracker.begin_mapping_mutation())
            .transpose()
            .map_err(|source| HvfGuestMemoryMappingError::DirtyWriteMappingMutation { source })?;
        let tracked_region_index = mutation
            .as_ref()
            .map(|mutation| mutation.prepare_remove(range, mapped_region.permissions))
            .transpose()
            .map_err(|source| HvfGuestMemoryMappingError::DirtyWriteMappingMutation { source })?
            .flatten();
        if let Err(source) = self.mapper.unmap_region(mapped_region) {
            return Err(HvfGuestMemoryMappingError::UnmapFailed {
                failures: vec![HvfGuestMemoryUnmapFailure { range, source }],
            });
        }

        if let Some(mutation) = mutation.as_mut() {
            mutation.commit_remove(tracked_region_index);
        }

        self.mapped_regions.remove(mapped_index);
        self.dynamic_regions.remove(dynamic_index);
        Ok(())
    }

    fn validate_dynamic_map_range(
        &self,
        range: GuestMemoryRange,
    ) -> Result<usize, HvfGuestMemoryMappingError> {
        for (index, mapped_region) in self.mapped_regions.iter().enumerate() {
            let existing = mapped_region.range;
            if existing.overlaps(range) {
                return Err(HvfGuestMemoryMappingError::DynamicRegionOverlapsMapped {
                    existing,
                    requested: range,
                });
            }

            if range.start() < existing.start() {
                return Ok(index);
            }
        }

        Ok(self.mapped_regions.len())
    }

    fn map_runtime_pmem_mapping(
        &mut self,
        mapping: HvfHostMemoryMapping,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.validate_dirty_write_mutation_state()?;
        let range = mapping
            .pmem_range()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                "runtime pmem mapping has no guest range",
            ))?;
        let page_size = host_page_size()?;
        let mut requests = validated_host_map_requests(std::slice::from_ref(&mapping), page_size)?;
        if requests.len() != 1 {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                "runtime pmem mapping must contain exactly one region",
            ));
        }
        let request = requests
            .pop()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                "runtime pmem map request is missing",
            ))?;
        debug_assert_eq!(request.request.range, range);
        let insert_index = self.validate_dynamic_map_range(range)?;
        self.mapped_regions.try_reserve_exact(1).map_err(|source| {
            HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source }
        })?;
        self.host_memory.try_reserve_exact(1).map_err(|source| {
            HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source }
        })?;

        let mapped_region = request.request.mapped_region(request.permissions);
        self.mapper
            .map_region(request.request, request.permissions)
            .map_err(|source| {
                HvfGuestMemoryMappingError::host_mapping(
                    &request.label,
                    range,
                    HvfGuestMemoryMappingError::MapFailed {
                        range,
                        source,
                        cleanup_failures: Vec::new(),
                    },
                )
            })?;

        self.mapped_regions.insert(insert_index, mapped_region);
        self.host_memory.push(mapping);
        self.host_memory_should_flush = true;
        self.host_memory_flushed = false;
        Ok(())
    }

    fn take_runtime_pmem_mapping(
        &mut self,
        range: GuestMemoryRange,
        flush: bool,
    ) -> Result<HvfHostMemoryMapping, HvfGuestMemoryMappingError> {
        self.validate_dirty_write_mutation_state()?;
        let host_index = self
            .host_memory
            .iter()
            .position(|mapping| mapping.pmem_range() == Some(range))
            .ok_or(HvfGuestMemoryMappingError::PmemMappingMissing { range })?;
        let mapped_index = self
            .mapped_regions
            .iter()
            .position(|mapped| mapped.range == range)
            .ok_or(HvfGuestMemoryMappingError::PmemMappingMissing { range })?;
        if flush {
            self.host_memory
                .get(host_index)
                .ok_or(HvfGuestMemoryMappingError::PmemMappingMissing { range })?
                .flush()
                .map_err(|failure| HvfGuestMemoryMappingError::FlushFailed {
                    failures: vec![failure],
                })?;
        }
        let mapped = self
            .mapped_regions
            .get(mapped_index)
            .copied()
            .ok_or(HvfGuestMemoryMappingError::PmemMappingMissing { range })?;
        self.mapper.unmap_region(mapped).map_err(|source| {
            HvfGuestMemoryMappingError::UnmapFailed {
                failures: vec![HvfGuestMemoryUnmapFailure { range, source }],
            }
        })?;
        self.mapped_regions.remove(mapped_index);
        Ok(self.host_memory.remove(host_index))
    }

    fn remove_dynamic_owner_after_failed_map(
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let _ = memory.discard_range(range);
        memory.remove_region(range).map_err(|source| {
            HvfGuestMemoryMappingError::DynamicRegionRemovalFailed { range, source }
        })
    }

    fn map_all(
        &mut self,
        memory: &GuestMemory,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let page_size = host_page_size()?;
        let requests = validated_map_requests(memory, permissions, page_size)?;
        let host_requests = validated_host_map_requests(&self.host_memory, page_size)?;
        let request_count = checked_map_request_count(requests.len(), host_requests.len())?;
        self.mapped_regions
            .try_reserve_exact(request_count)
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

        self.host_memory_should_flush = true;
        Ok(())
    }

    fn map_validated_request(
        &mut self,
        request: HvfMemoryMapRequest,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let mapped_region = request.mapped_region(permissions);
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

    fn flush_host_memory(&mut self) -> Result<(), HvfGuestMemoryMappingError> {
        if !self.host_memory_should_flush || self.host_memory_flushed {
            return Ok(());
        }

        self.flush_host_memory_now()?;
        self.host_memory_flushed = true;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct HvfPmemFlushExecutor<'a> {
    state: &'a HvfGuestMemoryMappingState,
}

impl<'a> HvfPmemFlushExecutor<'a> {
    fn new(state: &'a HvfGuestMemoryMappingState) -> Self {
        Self { state }
    }

    pub(crate) fn flush(
        &mut self,
        range: GuestMemoryRange,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.state.flush_pmem_mapping(range)
    }
}

#[derive(Debug)]
pub(crate) struct HvfVirtioMemMutationExecutor<'a> {
    state: &'a mut HvfGuestMemoryMappingState,
    permissions: HvfMemoryPermissions,
}

impl<'a> HvfVirtioMemMutationExecutor<'a> {
    fn new(state: &'a mut HvfGuestMemoryMappingState, permissions: HvfMemoryPermissions) -> Self {
        Self { state, permissions }
    }

    fn map_dynamic_region(
        &mut self,
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), VirtioMemMutationError> {
        self.state
            .map_dynamic_region(memory, range, self.permissions)
            .map_err(hvf_mutation_error)
    }

    fn unmap_dynamic_region(
        &mut self,
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), VirtioMemMutationError> {
        self.state
            .unmap_dynamic_region_preserving_owner(memory, range)
            .map_err(hvf_mutation_error)
    }

    fn rollback_mapped_region(
        &mut self,
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), VirtioMemMutationRollbackError> {
        self.state
            .unmap_dynamic_region(memory, range)
            .map_err(hvf_mutation_rollback_error)
    }

    fn rollback_unmapped_region(
        &mut self,
        memory: &mut GuestMemory,
        range: GuestMemoryRange,
    ) -> Result<(), VirtioMemMutationRollbackError> {
        self.state
            .map_existing_dynamic_region(memory, range, self.permissions)
            .map_err(hvf_mutation_rollback_error)
    }

    fn apply_plug(
        &mut self,
        memory: &mut GuestMemory,
        ranges: &[GuestMemoryRange],
    ) -> Result<(), VirtioMemMutationError> {
        for (index, range) in ranges.iter().copied().enumerate() {
            if let Err(source) = self.map_dynamic_region(memory, range) {
                let Some(applied_ranges) = ranges.get(..index) else {
                    return Err(VirtioMemMutationError::new(format!(
                        "{source}; completed plug prefix {index} exceeds {} ranges",
                        ranges.len()
                    )));
                };
                if let Err(rollback_source) = self.rollback_plug(memory, applied_ranges) {
                    return Err(VirtioMemMutationError::new(format!(
                        "{source}; also failed to roll back partially applied plug: {rollback_source}"
                    )));
                }

                return Err(source);
            }
        }

        Ok(())
    }

    fn rollback_plug(
        &mut self,
        memory: &mut GuestMemory,
        ranges: &[GuestMemoryRange],
    ) -> Result<(), VirtioMemMutationRollbackError> {
        let mut first_error = None;
        for range in ranges.iter().rev().copied() {
            if let Err(source) = self.rollback_mapped_region(memory, range)
                && first_error.is_none()
            {
                first_error = Some(source);
            }
        }

        match first_error {
            Some(source) => Err(source),
            None => Ok(()),
        }
    }

    fn apply_unplug(
        &mut self,
        memory: &mut GuestMemory,
        ranges: &[GuestMemoryRange],
        operation: &str,
    ) -> Result<(), VirtioMemMutationError> {
        for (index, range) in ranges.iter().copied().enumerate() {
            if let Err(source) = self.unmap_dynamic_region(memory, range) {
                let Some(applied_ranges) = ranges.get(..index) else {
                    return Err(VirtioMemMutationError::new(format!(
                        "{source}; completed {operation} prefix {index} exceeds {} ranges",
                        ranges.len()
                    )));
                };
                if let Err(rollback_source) = self.rollback_unplug(memory, applied_ranges) {
                    return Err(VirtioMemMutationError::new(format!(
                        "{source}; also failed to roll back partially applied {operation}: {rollback_source}"
                    )));
                }

                return Err(source);
            }
        }

        Ok(())
    }

    fn rollback_unplug(
        &mut self,
        memory: &mut GuestMemory,
        ranges: &[GuestMemoryRange],
    ) -> Result<(), VirtioMemMutationRollbackError> {
        let mut first_error = None;
        for range in ranges.iter().rev().copied() {
            if let Err(source) = self.rollback_unmapped_region(memory, range)
                && first_error.is_none()
            {
                first_error = Some(source);
            }
        }

        match first_error {
            Some(source) => Err(source),
            None => Ok(()),
        }
    }
}

impl VirtioMemMutationExecutor for HvfVirtioMemMutationExecutor<'_> {
    fn apply(
        &mut self,
        memory: &mut GuestMemory,
        mutation: VirtioMemMutation,
    ) -> Result<VirtioMemAppliedMutation, VirtioMemMutationError> {
        match mutation.kind() {
            VirtioMemMutationKind::Plug(ranges) => self.apply_plug(memory, ranges)?,
            VirtioMemMutationKind::Unplug(ranges) => self.apply_unplug(memory, ranges, "unplug")?,
            VirtioMemMutationKind::UnplugAll(ranges) => {
                self.apply_unplug(memory, ranges, "unplug-all")?
            }
        }

        Ok(VirtioMemAppliedMutation::new(mutation))
    }

    fn rollback(
        &mut self,
        memory: &mut GuestMemory,
        applied: VirtioMemAppliedMutation,
    ) -> Result<(), VirtioMemMutationRollbackError> {
        match applied.mutation().kind() {
            VirtioMemMutationKind::Plug(ranges) => self.rollback_plug(memory, ranges),
            VirtioMemMutationKind::Unplug(ranges) | VirtioMemMutationKind::UnplugAll(ranges) => {
                self.rollback_unplug(memory, ranges)
            }
        }
    }

    fn commit(&mut self, memory: &mut GuestMemory, applied: &VirtioMemAppliedMutation) {
        let ranges = match applied.mutation().kind() {
            VirtioMemMutationKind::Plug(_) => return,
            VirtioMemMutationKind::Unplug(ranges) | VirtioMemMutationKind::UnplugAll(ranges) => {
                ranges
            }
        };
        for range in ranges.iter().copied() {
            let _ = memory.discard_range(range);
            let removal = memory.remove_region(range);
            debug_assert!(
                removal.is_ok(),
                "committed virtio-mem unplug must retain its owner view until finalization"
            );
        }
    }
}

fn hvf_mutation_error(source: HvfGuestMemoryMappingError) -> VirtioMemMutationError {
    VirtioMemMutationError::new(source.to_string())
}

fn hvf_mutation_rollback_error(
    source: HvfGuestMemoryMappingError,
) -> VirtioMemMutationRollbackError {
    VirtioMemMutationRollbackError::new(source.to_string())
}

impl Drop for HvfGuestMemoryMapping {
    fn drop(&mut self) {
        if self.unmap_all().is_err() && self.has_mapped_regions() {
            if let Some(memory) = self.memory.take() {
                std::mem::forget(memory);
            }

            let host_memory = std::mem::take(&mut self.state.host_memory);
            std::mem::forget(host_memory);
        }
    }
}

impl HvfGuestMemoryMappingError {
    pub(crate) fn host_mapping(
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
    owner: HvfHostMemoryOwner,
    permissions: HvfMemoryPermissions,
}

impl HvfHostMemoryMapping {
    #[cfg(test)]
    pub(crate) fn new(
        label: impl Into<String>,
        memory: GuestMemory,
        permissions: HvfMemoryPermissions,
    ) -> Self {
        Self {
            label: label.into(),
            owner: HvfHostMemoryOwner::GuestMemory(memory),
            permissions,
        }
    }

    pub(crate) fn new_pmem(
        label: impl Into<String>,
        range: GuestMemoryRange,
        mapping: PmemBackingMapping,
        permissions: HvfMemoryPermissions,
    ) -> Self {
        Self {
            label: label.into(),
            owner: HvfHostMemoryOwner::Pmem { range, mapping },
            permissions,
        }
    }

    #[cfg(test)]
    fn new_test_pmem(
        label: impl Into<String>,
        memory: GuestMemory,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
        flush_count: Arc<AtomicUsize>,
        flush_fails: bool,
    ) -> Self {
        Self {
            label: label.into(),
            owner: HvfHostMemoryOwner::TestPmem {
                memory,
                range,
                flush_count,
                flush_fails,
            },
            permissions,
        }
    }

    fn flush(&self) -> Result<(), HvfGuestMemoryFlushFailure> {
        match &self.owner {
            #[cfg(test)]
            HvfHostMemoryOwner::GuestMemory(_) => Ok(()),
            #[cfg(test)]
            HvfHostMemoryOwner::TestPmem {
                flush_count,
                flush_fails,
                ..
            } => {
                flush_count.fetch_add(1, Ordering::Relaxed);
                if *flush_fails {
                    Err(self.flush_failure(BackendError::Hypervisor(
                        "injected pmem mapping flush failure".to_string(),
                    )))
                } else {
                    Ok(())
                }
            }
            HvfHostMemoryOwner::Pmem { mapping, .. } => mapping
                .flush()
                .map_err(|source| self.flush_failure(BackendError::Hypervisor(source.to_string()))),
        }
    }

    fn pmem_range(&self) -> Option<GuestMemoryRange> {
        match &self.owner {
            #[cfg(test)]
            HvfHostMemoryOwner::GuestMemory(_) => None,
            #[cfg(test)]
            HvfHostMemoryOwner::TestPmem { range, .. } => Some(*range),
            HvfHostMemoryOwner::Pmem { range, .. } => Some(*range),
        }
    }

    fn flush_failure(&self, source: BackendError) -> HvfGuestMemoryFlushFailure {
        HvfGuestMemoryFlushFailure {
            label: self.label.clone(),
            range: self.pmem_range(),
            source,
        }
    }
}

#[derive(Debug)]
enum HvfHostMemoryOwner {
    #[cfg(test)]
    GuestMemory(GuestMemory),
    #[cfg(test)]
    TestPmem {
        memory: GuestMemory,
        range: GuestMemoryRange,
        flush_count: Arc<AtomicUsize>,
        flush_fails: bool,
    },
    Pmem {
        range: GuestMemoryRange,
        mapping: PmemBackingMapping,
    },
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

    #[cfg(test)]
    pub(crate) const fn host_address(self) -> usize {
        self.host_address
    }

    #[cfg(test)]
    pub(crate) const fn size(self) -> usize {
        self.size
    }

    const fn mapped_region(self, permissions: HvfMemoryPermissions) -> HvfMappedGuestMemoryRegion {
        HvfMappedGuestMemoryRegion {
            range: self.range,
            guest_address: self.guest_address,
            size: self.size,
            permissions,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvfMappedGuestMemoryRegion {
    pub(crate) range: GuestMemoryRange,
    pub(crate) guest_address: u64,
    pub(crate) size: usize,
    pub(crate) permissions: HvfMemoryPermissions,
}

pub(crate) trait HvfMemoryMapper: fmt::Debug + Send + Sync {
    fn map_region(
        &self,
        request: HvfMemoryMapRequest,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), BackendError>;

    fn unmap_region(&self, mapped_region: HvfMappedGuestMemoryRegion) -> Result<(), BackendError>;

    fn protect_region(
        &self,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), BackendError>;
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

    fn protect_region(
        &self,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), BackendError> {
        let size = usize::try_from(range.size()).map_err(|_| {
            BackendError::InvalidState("validated guest memory protection size exceeds this host")
        })?;
        crate::ffi::protect_memory(range.start().raw_value(), size, permissions.bits())
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
    let request_count = host_map_request_count(host_mappings)?;
    requests
        .try_reserve_exact(request_count)
        .map_err(|source| HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source })?;

    for host_mapping in host_mappings {
        match &host_mapping.owner {
            #[cfg(test)]
            HvfHostMemoryOwner::GuestMemory(memory) => {
                if memory.regions().is_empty() {
                    return Err(HvfGuestMemoryMappingError::EmptyGuestMemory);
                }
                for region in memory.regions() {
                    requests.push(validate_host_map_request(
                        &host_mapping.label,
                        host_mapping.permissions,
                        region,
                        page_size,
                    )?);
                }
            }
            #[cfg(test)]
            HvfHostMemoryOwner::TestPmem { memory, .. } => {
                if memory.regions().is_empty() {
                    return Err(HvfGuestMemoryMappingError::EmptyGuestMemory);
                }
                for region in memory.regions() {
                    requests.push(validate_host_map_request(
                        &host_mapping.label,
                        host_mapping.permissions,
                        region,
                        page_size,
                    )?);
                }
            }
            HvfHostMemoryOwner::Pmem { range, mapping } => {
                requests.push(validate_host_map_request_parts(
                    &host_mapping.label,
                    host_mapping.permissions,
                    *range,
                    mapping.host_address().as_ptr() as usize,
                    mapping.host_size(),
                    page_size,
                )?);
            }
        }
    }

    Ok(requests)
}

fn host_map_request_count(
    host_mappings: &[HvfHostMemoryMapping],
) -> Result<usize, HvfGuestMemoryMappingError> {
    host_mappings.iter().try_fold(0, |count, mapping| {
        let mapping_count = match &mapping.owner {
            #[cfg(test)]
            HvfHostMemoryOwner::GuestMemory(memory) => memory.regions().len(),
            #[cfg(test)]
            HvfHostMemoryOwner::TestPmem { memory, .. } => memory.regions().len(),
            HvfHostMemoryOwner::Pmem { .. } => 1,
        };
        checked_map_request_count(count, mapping_count)
    })
}

fn checked_map_request_count(
    first: usize,
    second: usize,
) -> Result<usize, HvfGuestMemoryMappingError> {
    first.checked_add(second).ok_or_else(|| {
        BackendError::Hypervisor("too many HVF guest memory map requests".to_string()).into()
    })
}

#[cfg(test)]
fn validate_host_map_request(
    label: &str,
    permissions: HvfMemoryPermissions,
    region: &GuestMemoryRegion,
    page_size: u64,
) -> Result<HvfValidatedHostMemoryMapRequest, HvfGuestMemoryMappingError> {
    validate_host_map_request_parts(
        label,
        permissions,
        region.range(),
        region.host_address().as_ptr() as usize,
        region.host_size(),
        page_size,
    )
}

fn validate_host_map_request_parts(
    label: &str,
    permissions: HvfMemoryPermissions,
    range: GuestMemoryRange,
    host_address: usize,
    host_size: usize,
    page_size: u64,
) -> Result<HvfValidatedHostMemoryMapRequest, HvfGuestMemoryMappingError> {
    if permissions.is_empty() {
        return Err(HvfGuestMemoryMappingError::host_mapping(
            label,
            range,
            HvfGuestMemoryMappingError::EmptyPermissions,
        ));
    }

    let request = validate_map_request(range, host_address, host_size, page_size)
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

fn dynamic_region_map_request(
    memory: &GuestMemory,
    range: GuestMemoryRange,
    page_size: u64,
) -> Result<HvfMemoryMapRequest, HvfGuestMemoryMappingError> {
    let region = memory
        .regions()
        .iter()
        .find(|region| region.range() == range)
        .ok_or(HvfGuestMemoryMappingError::DynamicRegionOwnerMissing { range })?;

    validated_region_map_request(region, page_size)
}

fn guest_memory_contains_region(memory: &GuestMemory, range: GuestMemoryRange) -> bool {
    memory
        .regions()
        .iter()
        .any(|region| region.range() == range)
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryBacking, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::memory_hotplug::{
        VirtioMemMutation, VirtioMemMutationExecutor, VirtioMemMutationKind,
    };

    use crate::dirty::HvfDirtyWriteTrackerStartError;
    use crate::memory::FailedGuestMemoryMapping;

    use super::{
        HvfGuestMemoryMapping, HvfGuestMemoryMappingError, HvfHostMemoryMapping,
        HvfMappedGuestMemoryRegion, HvfMemoryMapRequest, HvfMemoryMapper, HvfMemoryPermissions,
        host_page_size, validate_map_request,
    };

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size)
            .expect("guest memory range should be valid for test")
    }

    fn memory_for_ranges(ranges: Vec<GuestMemoryRange>) -> GuestMemory {
        let layout =
            GuestMemoryLayout::new(ranges).expect("guest memory layout should be valid for test");

        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed")
    }

    fn memory_ranges(memory: &GuestMemory) -> Vec<GuestMemoryRange> {
        memory
            .regions()
            .iter()
            .map(|region| region.range())
            .collect()
    }

    fn page_size() -> u64 {
        host_page_size().expect("host page size should be available for tests")
    }

    fn writable_pmem_permissions() -> HvfMemoryPermissions {
        HvfMemoryPermissions::new(true, true, false)
    }

    fn counted_pmem_mapping(
        label: &str,
        memory: GuestMemory,
        range: GuestMemoryRange,
        flush_count: Arc<AtomicUsize>,
        flush_fails: bool,
    ) -> HvfHostMemoryMapping {
        HvfHostMemoryMapping::new_test_pmem(
            label,
            memory,
            range,
            writable_pmem_permissions(),
            flush_count,
            flush_fails,
        )
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
    fn checked_map_request_count_rejects_overflow() {
        let err = super::checked_map_request_count(usize::MAX, 1)
            .expect_err("overflowing request count should be rejected");

        assert_eq!(
            err.to_string(),
            "hypervisor error: too many HVF guest memory map requests"
        );
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

        assert_eq!(mapping.state.mapped_regions.len(), 2);
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

        assert_eq!(mapping.state.mapped_regions.len(), 3);
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
    fn dynamic_map_adds_owned_memory_and_hvf_mapping() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let dynamic_range = range(page_size, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");

        mapping
            .map_dynamic_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
            .expect("dynamic range should map");
        mapping
            .memory_mut()
            .expect("guest memory owner should exist")
            .write_slice(&[0xab], dynamic_range.start())
            .expect("dynamic range should be writable");

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, dynamic_range]
        );
        assert_eq!(mapping.state.mapped_regions.len(), 2);
        assert_eq!(mapping.state.dynamic_regions, vec![dynamic_range]);
        let maps = mapper.maps();
        let mut mapped = maps.iter().map(|(request, _)| request.range);
        assert_eq!(mapped.next(), Some(base_range));
        assert_eq!(mapped.next(), Some(dynamic_range));
        assert_eq!(mapped.next(), None);
    }

    #[test]
    fn dynamic_map_before_existing_range_preserves_owner_and_mapping_order() {
        let page_size = page_size();
        let dynamic_range = range(0, page_size);
        let base_range = range(page_size * 2, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping =
            HvfGuestMemoryMapping::map_with_mapper(memory, HvfMemoryPermissions::GUEST_RAM, mapper)
                .expect("initial guest memory should map");

        mapping
            .map_dynamic_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
            .expect("dynamic range before existing range should map");

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![dynamic_range, base_range]
        );
        assert_eq!(
            mapping
                .state
                .mapped_regions
                .iter()
                .map(|mapped_region| mapped_region.range)
                .collect::<Vec<_>>(),
            vec![dynamic_range, base_range]
        );
    }

    #[test]
    fn dynamic_map_rejects_empty_permissions_without_owner_mutation() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let dynamic_range = range(page_size, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");

        assert!(matches!(
            mapping.map_dynamic_region(
                dynamic_range,
                HvfMemoryPermissions::new(false, false, false)
            ),
            Err(HvfGuestMemoryMappingError::EmptyPermissions)
        ));
        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range]
        );
        assert!(!mapping.has_dynamic_regions());
        assert_eq!(mapper.map_count(), 1);
    }

    #[test]
    fn dynamic_map_rejects_overlap_without_owner_mutation() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let host_range = range(page_size * 8, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let host_memory = memory_for_ranges(vec![host_range]);
        let host_mappings = vec![HvfHostMemoryMapping::new(
            "pmem device `pmem0`",
            host_memory,
            HvfMemoryPermissions::READ,
        )];
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            mapper.clone(),
        )
        .expect("initial guest and host memory should map");

        let err = mapping
            .map_dynamic_region(base_range, HvfMemoryPermissions::GUEST_RAM)
            .expect_err("duplicate dynamic range should fail");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::DynamicRegionOverlapsMapped {
                existing,
                requested,
            } if existing == base_range && requested == base_range
        ));
        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range]
        );
        assert_eq!(mapper.map_count(), 2);

        let err = mapping
            .map_dynamic_region(host_range, HvfMemoryPermissions::GUEST_RAM)
            .expect_err("overlap with host-backed mapping should fail");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::DynamicRegionOverlapsMapped {
                existing,
                requested,
            } if existing == host_range && requested == host_range
        ));
        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range]
        );
        assert_eq!(mapper.map_count(), 2);
        assert!(!mapping.has_dynamic_regions());
    }

    #[test]
    fn dynamic_map_failure_rolls_back_owner() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let dynamic_range = range(page_size, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::new(Some(2), false));
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");

        let err = mapping
            .map_dynamic_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
            .expect_err("injected dynamic map failure should fail");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::DynamicRegionMapFailed {
                range,
                owner_cleanup: None,
                ..
            } if range == dynamic_range
        ));
        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range]
        );
        assert_eq!(mapping.state.mapped_regions.len(), 1);
        assert!(!mapping.has_dynamic_regions());
        assert_eq!(mapper.map_count(), 2);
        assert_eq!(mapper.unmap_count(), 0);
    }

    #[test]
    fn dynamic_unmap_removes_hvf_mapping_before_owner() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let dynamic_range = range(page_size, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");
        mapping
            .map_dynamic_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
            .expect("dynamic range should map");

        mapping
            .unmap_dynamic_region(dynamic_range)
            .expect("dynamic range should unmap and remove owner");

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range]
        );
        assert_eq!(mapping.state.mapped_regions.len(), 1);
        assert!(!mapping.has_dynamic_regions());
        let unmaps = mapper.unmaps();
        assert_eq!(
            unmaps.first().map(|mapped_region| mapped_region.range),
            Some(dynamic_range)
        );
    }

    #[test]
    fn dynamic_unmap_rejects_missing_or_static_range() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let missing_range = range(page_size, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");

        assert!(matches!(
            mapping.unmap_dynamic_region(base_range),
            Err(HvfGuestMemoryMappingError::DynamicRegionMissing { range })
                if range == base_range
        ));
        assert!(matches!(
            mapping.unmap_dynamic_region(missing_range),
            Err(HvfGuestMemoryMappingError::DynamicRegionMissing { range })
                if range == missing_range
        ));
        assert_eq!(mapper.unmap_count(), 0);
        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range]
        );
    }

    #[test]
    fn dynamic_unmap_failure_keeps_state_for_retry() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let dynamic_range = range(page_size, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");
        mapping
            .map_dynamic_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
            .expect("dynamic range should map");

        mapper.set_fail_unmap(true);
        let err = mapping
            .unmap_dynamic_region(dynamic_range)
            .expect_err("failed dynamic unmap should be retained");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::UnmapFailed { failures }
                if failures.len() == 1 && failures[0].range() == dynamic_range
        ));
        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, dynamic_range]
        );
        assert_eq!(mapping.state.dynamic_regions, vec![dynamic_range]);

        mapper.set_fail_unmap(false);
        mapping
            .unmap_dynamic_region(dynamic_range)
            .expect("dynamic unmap retry should succeed");

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range]
        );
        assert!(!mapping.has_dynamic_regions());
        assert_eq!(mapper.unmap_count(), 2);
    }

    #[test]
    fn virtio_mem_executor_plug_maps_block_ranges_and_rollback_unmaps_in_reverse() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let first_dynamic = range(page_size, page_size);
        let second_dynamic = range(page_size * 2, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");

        let applied = {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::Plug(vec![
                        first_dynamic,
                        second_dynamic,
                    ])),
                )
                .expect("plug mutation should map dynamic memory")
        };

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic, second_dynamic]
        );
        assert_eq!(
            mapping.state.dynamic_regions,
            vec![first_dynamic, second_dynamic]
        );
        assert_eq!(mapper.map_count(), 3);

        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .rollback(memory, applied)
                .expect("plug rollback should unmap dynamic memory");
        }

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range]
        );
        assert!(!mapping.has_dynamic_regions());
        assert_eq!(
            mapper
                .unmaps()
                .iter()
                .map(|mapped| mapped.range)
                .collect::<Vec<_>>(),
            vec![second_dynamic, first_dynamic]
        );
    }

    #[test]
    fn virtio_mem_executor_partially_unplugs_multi_block_plug_and_rollback_remaps() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let first_dynamic = range(page_size, page_size);
        let second_dynamic = range(page_size * 2, page_size);
        let layout =
            GuestMemoryLayout::new(vec![base_range]).expect("shared base layout should validate");
        let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
            .expect("shared base memory should allocate");
        memory
            .reserve_shared_region(range(page_size, page_size * 2))
            .expect("dynamic memory aperture should reserve");
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");
        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::Plug(vec![
                        first_dynamic,
                        second_dynamic,
                    ])),
                )
                .expect("multi-block plug should create exact block owners");
        }
        mapping
            .memory_mut()
            .expect("guest memory owner should exist")
            .write_slice(&[0x5a], second_dynamic.start())
            .expect("online reservation view should be writable");
        let second_host_address = mapping
            .memory()
            .expect("guest memory owner should exist")
            .regions()
            .iter()
            .find(|region| region.range() == second_dynamic)
            .expect("second dynamic owner should exist")
            .host_address();

        let applied = {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::Unplug(vec![second_dynamic])),
                )
                .expect("partial unplug should stage one block owner for removal")
        };

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic, second_dynamic]
        );
        assert_eq!(mapping.state.dynamic_regions, vec![first_dynamic]);
        let mut retained_byte = [0];
        mapping
            .memory()
            .expect("guest memory owner should exist")
            .read_slice(&mut retained_byte, second_dynamic.start())
            .expect("staged unplug must retain bytes for rollback");
        assert_eq!(retained_byte, [0x5a]);

        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .rollback(memory, applied)
                .expect("unplug rollback should remap dynamic memory");
        }

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic, second_dynamic]
        );
        assert_eq!(
            mapping.state.dynamic_regions,
            vec![first_dynamic, second_dynamic]
        );
        assert_eq!(
            mapping
                .memory()
                .expect("guest memory owner should exist")
                .regions()
                .iter()
                .find(|region| region.range() == second_dynamic)
                .expect("rolled-back owner should exist")
                .host_address(),
            second_host_address
        );
        assert_eq!(mapper.map_count(), 4);
        assert_eq!(mapper.unmap_count(), 1);

        let committed = {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::Unplug(vec![second_dynamic])),
                )
                .expect("retried unplug should stage the exact owner")
        };
        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor.commit(memory, &committed);
        }
        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic]
        );
        assert_eq!(mapping.state.dynamic_regions, vec![first_dynamic]);
        let mut removed_byte = [0xff];
        assert!(
            mapping
                .memory()
                .expect("guest memory owner should exist")
                .read_slice(&mut removed_byte, second_dynamic.start())
                .is_err(),
            "committed unplug must remove the active reservation view"
        );
        assert_eq!(removed_byte, [0xff]);
        assert_eq!(mapper.map_count(), 4);
        assert_eq!(mapper.unmap_count(), 2);
    }

    #[test]
    fn virtio_mem_executor_combines_adjacent_sequential_plugs_for_unplug() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let first_dynamic = range(page_size, page_size);
        let second_dynamic = range(page_size * 2, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");

        for dynamic_range in [first_dynamic, second_dynamic] {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::Plug(vec![dynamic_range])),
                )
                .expect("sequential block plug should map");
        }

        let applied = {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::Unplug(vec![
                        first_dynamic,
                        second_dynamic,
                    ])),
                )
                .expect("combined unplug should stage adjacent exact owners")
        };

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic, second_dynamic]
        );
        assert!(!mapping.has_dynamic_regions());
        assert_eq!(mapper.unmap_count(), 2);

        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .rollback(memory, applied)
                .expect("combined unplug rollback should restore both owners");
        }

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic, second_dynamic]
        );
        assert_eq!(
            mapping.state.dynamic_regions,
            vec![second_dynamic, first_dynamic]
        );
        assert_eq!(mapper.map_count(), 5);
    }

    #[test]
    fn virtio_mem_executor_unplug_all_removes_snapshot_and_rollback_remaps() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let first_dynamic = range(page_size, page_size);
        let second_dynamic = range(page_size * 2, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");
        mapping
            .map_dynamic_region(first_dynamic, HvfMemoryPermissions::GUEST_RAM)
            .expect("first dynamic range should map");
        mapping
            .map_dynamic_region(second_dynamic, HvfMemoryPermissions::GUEST_RAM)
            .expect("second dynamic range should map");

        let applied = {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::UnplugAll(vec![
                        first_dynamic,
                        second_dynamic,
                    ])),
                )
                .expect("unplug-all should stage dynamic memory removal")
        };

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic, second_dynamic]
        );
        assert!(!mapping.has_dynamic_regions());

        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .rollback(memory, applied)
                .expect("unplug-all rollback should remap dynamic memory");
        }

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic, second_dynamic]
        );
        assert_eq!(mapper.map_count(), 5);
        assert_eq!(mapper.unmap_count(), 2);
    }

    #[test]
    fn virtio_mem_executor_unplug_all_rolls_back_partial_apply_failure() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let dynamic_range = range(page_size, page_size);
        let missing_range = range(page_size * 2, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");
        mapping
            .map_dynamic_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
            .expect("dynamic range should map");

        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            let err = executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::UnplugAll(vec![
                        dynamic_range,
                        missing_range,
                    ])),
                )
                .expect_err("missing range should fail unplug-all");
            assert!(
                err.to_string().contains("not mapped"),
                "unexpected error: {err}"
            );
        }

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, dynamic_range]
        );
        assert_eq!(mapping.state.dynamic_regions, vec![dynamic_range]);
        assert_eq!(mapper.map_count(), 3);
        assert_eq!(mapper.unmap_count(), 1);
    }

    #[test]
    fn virtio_mem_executor_plug_rolls_back_partial_apply_failure() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let first_dynamic = range(page_size, page_size);
        let second_dynamic = range(page_size * 2, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::new(Some(3), false));
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");

        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            let error = executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::Plug(vec![
                        first_dynamic,
                        second_dynamic,
                    ])),
                )
                .expect_err("second block map failure should roll back the first block");
            assert!(
                error.to_string().contains("injected map failure"),
                "unexpected error: {error}"
            );
        }

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range]
        );
        assert!(!mapping.has_dynamic_regions());
        assert_eq!(mapper.map_count(), 3);
        assert_eq!(mapper.unmap_count(), 1);
    }

    #[test]
    fn virtio_mem_executor_plug_surfaces_partial_apply_rollback_failure() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let first_dynamic = range(page_size, page_size);
        let second_dynamic = range(page_size * 2, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::new(Some(3), true));
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");

        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            let error = executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::Plug(vec![
                        first_dynamic,
                        second_dynamic,
                    ])),
                )
                .expect_err("partial plug rollback failure should surface");
            let message = error.to_string();
            assert!(
                message.contains("injected map failure"),
                "unexpected error: {error}"
            );
            assert!(
                message.contains("also failed to roll back partially applied plug"),
                "unexpected error: {error}"
            );
            assert!(
                message.contains("failed to unmap 1 guest memory region(s)"),
                "unexpected error: {error}"
            );
        }

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic]
        );
        assert_eq!(mapping.state.dynamic_regions, vec![first_dynamic]);
        assert_eq!(mapper.map_count(), 3);
        assert_eq!(mapper.unmap_count(), 1);

        mapper.set_fail_unmap(false);
        mapping
            .unmap_dynamic_region(first_dynamic)
            .expect("retained block should remain cleanly removable");
    }

    #[test]
    fn virtio_mem_executor_reports_vector_rollback_failure_and_attempts_every_block() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let first_dynamic = range(page_size, page_size);
        let second_dynamic = range(page_size * 2, page_size);
        let memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("initial guest memory should map");
        let applied = {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            executor
                .apply(
                    memory,
                    VirtioMemMutation::new(VirtioMemMutationKind::Plug(vec![
                        first_dynamic,
                        second_dynamic,
                    ])),
                )
                .expect("plug mutation should map dynamic memory")
        };

        mapper.set_fail_unmap(true);
        {
            let (memory, mut executor) = mapping
                .memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .expect("virtio-mem executor should borrow mapped memory");
            let err = executor
                .rollback(memory, applied)
                .expect_err("injected rollback unmap failure should surface");
            assert!(
                err.to_string().contains("failed to unmap"),
                "unexpected error: {err}"
            );
        }

        assert_eq!(
            memory_ranges(mapping.memory().expect("guest memory owner should exist")),
            vec![base_range, first_dynamic, second_dynamic]
        );
        assert_eq!(
            mapping.state.dynamic_regions,
            vec![first_dynamic, second_dynamic]
        );
        assert_eq!(mapper.unmap_count(), 2);

        mapper.set_fail_unmap(false);
        mapping
            .unmap_dynamic_region(second_dynamic)
            .expect("second retained block should clean up");
        mapping
            .unmap_dynamic_region(first_dynamic)
            .expect("first retained block should clean up");
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
    #[test]
    fn explicit_unmap_flushes_pmem_mapping_once() {
        let page_size = page_size();
        let guest_memory = memory_for_ranges(vec![range(0, page_size)]);
        let pmem_range = range(page_size * 8, page_size);
        let flush_count = Arc::new(AtomicUsize::new(0));
        let host_mappings = vec![counted_pmem_mapping(
            "pmem0",
            memory_for_ranges(vec![pmem_range]),
            pmem_range,
            Arc::clone(&flush_count),
            false,
        )];
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            guest_memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            Arc::new(RecordingMapper::default()),
        )
        .expect("guest and pmem memory should map");

        mapping
            .unmap_all()
            .expect("unmap should synchronize the pmem mapping");

        assert_eq!(flush_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn targeted_pmem_flush_selects_only_requested_mapping() {
        let page_size = page_size();
        let guest_memory = memory_for_ranges(vec![range(0, page_size)]);
        let first_range = range(page_size * 8, page_size);
        let second_range = range(page_size * 9, page_size);
        let first_count = Arc::new(AtomicUsize::new(0));
        let second_count = Arc::new(AtomicUsize::new(0));
        let host_mappings = vec![
            counted_pmem_mapping(
                "pmem0",
                memory_for_ranges(vec![first_range]),
                first_range,
                Arc::clone(&first_count),
                false,
            ),
            counted_pmem_mapping(
                "pmem1",
                memory_for_ranges(vec![second_range]),
                second_range,
                Arc::clone(&second_count),
                false,
            ),
        ];
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            guest_memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            Arc::new(RecordingMapper::default()),
        )
        .expect("guest and pmem memory should map");

        let (_, mut executor) = mapping
            .memory_and_pmem_flush_executor_mut()
            .expect("pmem flush executor should borrow mapped memory");
        executor
            .flush(second_range)
            .expect("targeted pmem mapping should synchronize");

        assert_eq!(first_count.load(Ordering::Relaxed), 0);
        assert_eq!(second_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn targeted_pmem_flush_failure_is_typed_and_does_not_flush_peer() {
        let page_size = page_size();
        let guest_memory = memory_for_ranges(vec![range(0, page_size)]);
        let selected_range = range(page_size * 8, page_size);
        let peer_range = range(page_size * 9, page_size);
        let selected_count = Arc::new(AtomicUsize::new(0));
        let peer_count = Arc::new(AtomicUsize::new(0));
        let host_mappings = vec![
            counted_pmem_mapping(
                "pmem0",
                memory_for_ranges(vec![selected_range]),
                selected_range,
                Arc::clone(&selected_count),
                true,
            ),
            counted_pmem_mapping(
                "pmem1",
                memory_for_ranges(vec![peer_range]),
                peer_range,
                Arc::clone(&peer_count),
                false,
            ),
        ];
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            guest_memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            Arc::new(RecordingMapper::default()),
        )
        .expect("guest and pmem memory should map");

        let (_, mut executor) = mapping
            .memory_and_pmem_flush_executor_mut()
            .expect("pmem flush executor should borrow mapped memory");
        let err = executor
            .flush(selected_range)
            .expect_err("injected pmem mapping flush should fail");
        let message = err.to_string();

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::FlushFailed { failures }
                if failures.len() == 1
                    && failures.first().is_some_and(|failure| {
                        failure.label() == "pmem0"
                            && failure.range() == Some(selected_range)
                    })
        ));
        assert!(message.contains("pmem0"));
        assert!(message.contains(&selected_range.to_string()));
        assert!(!message.contains("pmem1"));
        assert_eq!(selected_count.load(Ordering::Relaxed), 1);
        assert_eq!(peer_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn targeted_pmem_flush_rejects_missing_range_without_flushing() {
        let page_size = page_size();
        let guest_memory = memory_for_ranges(vec![range(0, page_size)]);
        let pmem_range = range(page_size * 8, page_size);
        let missing_range = range(page_size * 9, page_size);
        let flush_count = Arc::new(AtomicUsize::new(0));
        let host_mappings = vec![counted_pmem_mapping(
            "pmem0",
            memory_for_ranges(vec![pmem_range]),
            pmem_range,
            Arc::clone(&flush_count),
            false,
        )];
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            guest_memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            Arc::new(RecordingMapper::default()),
        )
        .expect("guest and pmem memory should map");

        let (_, mut executor) = mapping
            .memory_and_pmem_flush_executor_mut()
            .expect("pmem flush executor should borrow mapped memory");
        let err = executor
            .flush(missing_range)
            .expect_err("unknown pmem range should fail closed");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::PmemMappingMissing { range }
                if range == missing_range
        ));
        assert_eq!(flush_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn runtime_pmem_take_restore_preserves_owner_and_flush_boundary() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let pmem_range = range(page_size * 8, page_size);
        let mapper = Arc::new(RecordingMapper::default());
        let flush_count = Arc::new(AtomicUsize::new(0));
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory_for_ranges(vec![base_range]),
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("base guest memory should map");
        let host = counted_pmem_mapping(
            "runtime",
            memory_for_ranges(vec![pmem_range]),
            pmem_range,
            Arc::clone(&flush_count),
            false,
        );

        mapping
            .map_runtime_pmem_mapping(host)
            .expect("runtime pmem mapping should map");
        let retained = mapping
            .take_runtime_pmem_mapping(pmem_range, true)
            .expect("runtime pmem mapping should flush and unmap");
        assert_eq!(flush_count.load(Ordering::Relaxed), 1);
        assert_eq!(mapper.unmap_count(), 1);
        assert!(matches!(
            mapping.take_runtime_pmem_mapping(pmem_range, false),
            Err(HvfGuestMemoryMappingError::PmemMappingMissing { range })
                if range == pmem_range
        ));

        mapping
            .map_runtime_pmem_mapping(retained)
            .expect("the exact retained mapping should remap");
        drop(
            mapping
                .take_runtime_pmem_mapping(pmem_range, false)
                .expect("rollback should take the exact owner without flushing"),
        );
        assert_eq!(flush_count.load(Ordering::Relaxed), 1);
        mapping.unmap_all().expect("base memory should unmap");
    }

    #[test]
    fn runtime_pmem_failures_publish_nothing_and_retain_live_owner() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let pmem_range = range(page_size * 8, page_size);
        let mapper = Arc::new(RecordingMapper::new(Some(2), false));
        let flush_count = Arc::new(AtomicUsize::new(0));
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            memory_for_ranges(vec![base_range]),
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("base guest memory should map");

        assert!(matches!(
            mapping.map_runtime_pmem_mapping(counted_pmem_mapping(
                "runtime",
                memory_for_ranges(vec![pmem_range]),
                pmem_range,
                Arc::clone(&flush_count),
                false,
            )),
            Err(HvfGuestMemoryMappingError::HostMapping { range, .. })
                if range == pmem_range
        ));
        assert_eq!(mapping.state.mapped_regions.len(), 1);
        assert!(mapping.state.host_memory.is_empty());

        mapper.set_fail_map_call(None);
        mapping
            .map_runtime_pmem_mapping(counted_pmem_mapping(
                "runtime",
                memory_for_ranges(vec![pmem_range]),
                pmem_range,
                Arc::clone(&flush_count),
                false,
            ))
            .expect("runtime pmem mapping should map after retry");
        mapper.set_fail_unmap(true);
        assert!(matches!(
            mapping.take_runtime_pmem_mapping(pmem_range, false),
            Err(HvfGuestMemoryMappingError::UnmapFailed { .. })
        ));
        assert_eq!(mapping.state.host_memory.len(), 1);
        assert_eq!(flush_count.load(Ordering::Relaxed), 0);

        mapper.set_fail_unmap(false);
        drop(
            mapping
                .take_runtime_pmem_mapping(pmem_range, false)
                .expect("retained runtime mapping should unmap on retry"),
        );
        mapping.unmap_all().expect("base mapping should unmap");
    }

    #[test]
    fn failed_global_unmap_does_not_flush_pmem_mapping() {
        let page_size = page_size();
        let guest_memory = memory_for_ranges(vec![range(0, page_size)]);
        let pmem_range = range(page_size * 8, page_size);
        let flush_count = Arc::new(AtomicUsize::new(0));
        let host_mappings = vec![counted_pmem_mapping(
            "pmem0",
            memory_for_ranges(vec![pmem_range]),
            pmem_range,
            Arc::clone(&flush_count),
            false,
        )];
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            guest_memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            Arc::new(RecordingMapper::new(None, true)),
        )
        .expect("guest and pmem memory should map");

        let err = mapping
            .unmap_all()
            .expect_err("unmap failure should precede pmem flush");

        assert!(matches!(
            err,
            HvfGuestMemoryMappingError::UnmapFailed { failures } if failures.len() == 2
        ));
        assert_eq!(flush_count.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn dirty_write_tracking_protects_guest_ram_but_not_host_backed_pmem() {
        let page_size = page_size();
        let guest_range = range(0, page_size);
        let guest_memory = memory_for_ranges(vec![guest_range]);
        let pmem_range = range(page_size * 8, page_size);
        let host_memory = memory_for_ranges(vec![pmem_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let host_mappings = vec![counted_pmem_mapping(
            "pmem device `pmem0`",
            host_memory,
            pmem_range,
            Arc::new(AtomicUsize::new(0)),
            false,
        )];
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            guest_memory,
            HvfMemoryPermissions::GUEST_RAM,
            host_mappings,
            mapper.clone(),
        )
        .expect("guest and pmem memory should map");

        let tracker = mapping
            .start_dirty_write_tracking()
            .expect("dirty-write tracker should start");

        assert_eq!(
            mapper.protects(),
            vec![(guest_range, HvfMemoryPermissions::new(true, false, true))]
        );
        tracker.stop().expect("tracker should stop");
        mapping.unmap_all().expect("mapping should unmap");
    }

    #[test]
    fn dirty_write_tracking_includes_existing_and_live_dynamic_ram() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let dynamic_range = range(page_size * 4, page_size);
        let guest_memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            guest_memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("guest memory should map");
        mapping
            .map_dynamic_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
            .expect("dynamic RAM should map before tracking");

        let tracker = mapping
            .start_dirty_write_tracking()
            .expect("dirty-write tracker should include dynamic RAM");
        assert!(matches!(
            mapping.start_dirty_write_tracking(),
            Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "dirty-write tracking is already active"
            ))
        ));

        assert_eq!(
            mapper.protects(),
            vec![
                (base_range, HvfMemoryPermissions::new(true, false, true)),
                (dynamic_range, HvfMemoryPermissions::new(true, false, true)),
            ]
        );
        let live_range = range(page_size * 8, page_size);
        mapping
            .map_dynamic_region(live_range, HvfMemoryPermissions::GUEST_RAM)
            .expect("live dynamic RAM should map under tracker serialization");
        assert_eq!(
            mapper.maps().last().map(|(_, permissions)| *permissions),
            Some(HvfMemoryPermissions::new(true, false, true))
        );
        assert_eq!(
            tracker.dirty_pages().expect("new dynamic RAM should query"),
            vec![live_range.start()]
        );
        mapping
            .unmap_dynamic_region(live_range)
            .expect("tracked live RAM should unmap");
        assert!(
            tracker
                .dirty_pages()
                .expect("removed dynamic RAM should leave no dirty page")
                .is_empty()
        );
        mapping
            .unmap_dynamic_region(dynamic_range)
            .expect("pre-existing tracked dynamic RAM should unmap");
        mapping
            .unmap_all()
            .expect("tracker and mapping should stop");
    }

    #[test]
    fn tracked_dynamic_mapping_failures_preserve_owner_and_epoch_metadata() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let failed_range = range(page_size * 4, page_size);
        let live_range = range(page_size * 8, page_size);
        let guest_memory = memory_for_ranges(vec![base_range]);
        let mapper = Arc::new(RecordingMapper::new(Some(2), false));
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            guest_memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("base guest memory should map");
        let tracker = mapping
            .start_dirty_write_tracking()
            .expect("dirty-write tracking should start");

        assert!(matches!(
            mapping.map_dynamic_region(failed_range, HvfMemoryPermissions::GUEST_RAM),
            Err(HvfGuestMemoryMappingError::DynamicRegionMapFailed {
                owner_cleanup: None,
                ..
            })
        ));
        assert_eq!(
            memory_ranges(mapping.memory().expect("base owner should remain")),
            vec![base_range]
        );
        assert!(
            tracker
                .dirty_pages()
                .expect("failed addition should leave no dirty metadata")
                .is_empty()
        );

        mapping
            .map_dynamic_region(live_range, HvfMemoryPermissions::GUEST_RAM)
            .expect("fresh tracked addition should remain retryable");
        assert_eq!(
            tracker
                .dirty_pages()
                .expect("successful addition should be wholly dirty"),
            vec![live_range.start()]
        );

        mapper.set_fail_unmap(true);
        assert!(matches!(
            mapping.unmap_dynamic_region(live_range),
            Err(HvfGuestMemoryMappingError::UnmapFailed { .. })
        ));
        assert_eq!(
            memory_ranges(mapping.memory().expect("failed unmap owner should remain")),
            vec![base_range, live_range]
        );
        assert_eq!(
            tracker
                .dirty_pages()
                .expect("failed unmap should retain old epoch metadata"),
            vec![live_range.start()]
        );

        mapper.set_fail_unmap(false);
        mapping
            .unmap_dynamic_region(live_range)
            .expect("tracked unmap should retry after mapper recovery");
        assert!(
            tracker
                .dirty_pages()
                .expect("successful removal should drop dirty metadata")
                .is_empty()
        );
        mapping.unmap_all().expect("mapping should shut down");
    }

    #[test]
    fn incomplete_initial_protection_rollback_is_terminal_until_unmap() {
        let page_size = page_size();
        let guest_memory =
            memory_for_ranges(vec![range(0, page_size), range(page_size * 2, page_size)]);
        let mapper = Arc::new(RecordingMapper::default());
        mapper.set_fail_protect_on(vec![2, 3]);
        let mut mapping = HvfGuestMemoryMapping::map_with_mapper(
            guest_memory,
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("guest memory should map");

        let error = mapping
            .start_dirty_write_tracking()
            .expect_err("protection and rollback should fail");

        assert!(error.requires_vm_teardown());
        assert!(mapping.state.protection_poisoned);
        assert!(mapping.active_dirty_write_tracker().is_err());
        assert!(matches!(
            mapping.map_dynamic_region(
                range(page_size * 4, page_size),
                HvfMemoryPermissions::GUEST_RAM
            ),
            Err(HvfGuestMemoryMappingError::InvalidState(
                "guest memory protection rollback requires VM teardown"
            ))
        ));
        mapping
            .unmap_all()
            .expect("unmap should remain the poisoned mapping recovery boundary");
        assert_eq!(mapper.unmap_count(), 2);
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
                    protects: Vec::new(),
                    fail_map_on,
                    fail_unmap,
                    fail_protect_on: Vec::new(),
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

        fn set_fail_map_call(&self, fail_map_on: Option<usize>) {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .fail_map_on = fail_map_on;
        }

        fn protects(&self) -> Vec<(GuestMemoryRange, HvfMemoryPermissions)> {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .protects
                .clone()
        }

        fn set_fail_protect_on(&self, calls: Vec<usize>) {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .fail_protect_on = calls;
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

        fn protect_region(
            &self,
            range: GuestMemoryRange,
            permissions: HvfMemoryPermissions,
        ) -> Result<(), bangbang_runtime::BackendError> {
            let mut state = self
                .state
                .lock()
                .expect("state lock should not be poisoned");
            state.protects.push((range, permissions));
            let call_index = state.protects.len();
            if state.fail_protect_on.contains(&call_index) {
                Err(bangbang_runtime::BackendError::Hypervisor(
                    "injected protection failure".to_string(),
                ))
            } else {
                Ok(())
            }
        }
    }

    #[derive(Debug, Default)]
    struct RecordingMapperState {
        maps: Vec<(HvfMemoryMapRequest, HvfMemoryPermissions)>,
        unmaps: Vec<HvfMappedGuestMemoryRegion>,
        protects: Vec<(GuestMemoryRange, HvfMemoryPermissions)>,
        fail_map_on: Option<usize>,
        fail_unmap: bool,
        fail_protect_on: Vec<usize>,
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
