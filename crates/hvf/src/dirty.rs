//! Hypervisor.framework guest-CPU dirty-write observation primitives.

use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use bangbang_runtime::BackendError;
use bangbang_runtime::machine::MAX_SUPPORTED_VCPUS;
use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange};
use bangbang_runtime::memory_dirty::{
    GuestMemoryDirtyTracker, GuestMemoryDirtyTrackerAccessError, GuestMemoryDirtyTrackerError,
};

use crate::exit::HvfExceptionExit;
use crate::memory::{HvfMappedGuestMemoryRegion, HvfMemoryMapper, HvfMemoryPermissions};

const MAX_REPORTED_STOP_FAILURES: usize = 64;
const TRACKER_LOCK_POISONED_MESSAGE: &str = "dirty-write tracker lock is poisoned";
const TRACKER_NOT_ACTIVE_MESSAGE: &str = "dirty-write tracker is not active";
const TRACKER_POISONED_MESSAGE: &str = "dirty-write tracker requires VM teardown";

/// One redacted protection-call failure.
#[derive(Clone, PartialEq, Eq)]
pub struct HvfDirtyWriteProtectionFailure {
    operation_index: usize,
    source: BackendError,
}

impl HvfDirtyWriteProtectionFailure {
    pub const fn operation_index(&self) -> usize {
        self.operation_index
    }

    pub const fn source(&self) -> &BackendError {
        &self.source
    }
}

impl fmt::Debug for HvfDirtyWriteProtectionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HvfDirtyWriteProtectionFailure")
            .field("operation_index", &self.operation_index)
            .field("source", &self.source)
            .finish()
    }
}

impl fmt::Display for HvfDirtyWriteProtectionFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "protection operation {} failed: {}",
            self.operation_index, self.source
        )
    }
}

impl std::error::Error for HvfDirtyWriteProtectionFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfDirtyWriteTrackerStartError {
    Backend(BackendError),
    DirtyBitmap,
    InvalidState(&'static str),
    InvalidHostPageSize {
        page_size: u64,
    },
    AllocationFailed(&'static str),
    ProtectionFailed {
        failure: HvfDirtyWriteProtectionFailure,
        rollback_failures: Vec<HvfDirtyWriteProtectionFailure>,
    },
}

impl HvfDirtyWriteTrackerStartError {
    pub fn requires_vm_teardown(&self) -> bool {
        matches!(
            self,
            Self::ProtectionFailed {
                rollback_failures,
                ..
            } if !rollback_failures.is_empty()
        )
    }
}

impl fmt::Display for HvfDirtyWriteTrackerStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backend(source) => write!(f, "{source}"),
            Self::DirtyBitmap => f.write_str("failed to prepare dirty bitmap"),
            Self::InvalidState(message) => {
                write!(f, "invalid dirty-write tracker start state: {message}")
            }
            Self::InvalidHostPageSize { page_size } => {
                write!(f, "invalid dirty-write tracker host page size {page_size}")
            }
            Self::AllocationFailed(kind) => {
                write!(f, "failed to allocate dirty-write tracker {kind}")
            }
            Self::ProtectionFailed {
                failure,
                rollback_failures,
            } => {
                if rollback_failures.is_empty() {
                    write!(f, "failed to start dirty-write protection: {failure}")
                } else {
                    write!(
                        f,
                        "failed to start dirty-write protection: {failure}; also failed to restore {} completed range(s)",
                        rollback_failures.len()
                    )
                }
            }
        }
    }
}

impl std::error::Error for HvfDirtyWriteTrackerStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backend(source) => Some(source),
            Self::DirtyBitmap => None,
            Self::ProtectionFailed { failure, .. } => Some(failure),
            Self::InvalidState(_)
            | Self::InvalidHostPageSize { .. }
            | Self::AllocationFailed(_) => None,
        }
    }
}

impl From<BackendError> for HvfDirtyWriteTrackerStartError {
    fn from(source: BackendError) -> Self {
        Self::Backend(source)
    }
}

impl From<GuestMemoryDirtyTrackerError> for HvfDirtyWriteTrackerStartError {
    fn from(_source: GuestMemoryDirtyTrackerError) -> Self {
        Self::DirtyBitmap
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfDirtyWriteTrackerStopError {
    InvalidState(&'static str),
    OwnersActive {
        count: usize,
    },
    ProtectionFailed {
        failures: Vec<HvfDirtyWriteProtectionFailure>,
        omitted_failures: usize,
    },
}

impl fmt::Display for HvfDirtyWriteTrackerStopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message) => {
                write!(f, "invalid dirty-write tracker stop state: {message}")
            }
            Self::OwnersActive { count } => write!(
                f,
                "cannot stop dirty-write tracking while {count} vCPU owner(s) remain active"
            ),
            Self::ProtectionFailed {
                failures,
                omitted_failures,
            } => {
                write!(
                    f,
                    "failed to restore {} dirty-write protection range(s)",
                    failures.len()
                )?;
                if *omitted_failures != 0 {
                    write!(f, "; {omitted_failures} additional failure(s) omitted")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for HvfDirtyWriteTrackerStopError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ProtectionFailed { failures, .. } => failures
                .first()
                .map(|failure| failure as &(dyn std::error::Error + 'static)),
            Self::InvalidState(_) | Self::OwnersActive { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfDirtyWriteTrackerQueryError {
    InvalidState(&'static str),
    AllocationFailed,
}

/// Failure while advancing one dirty generation under external quiescence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfDirtyWriteEpochResetError {
    InvalidState(&'static str),
    AllocationFailed,
    ProtectionFailed {
        failure: HvfDirtyWriteProtectionFailure,
        rollback_failures: Vec<HvfDirtyWriteProtectionFailure>,
    },
}

/// Failure while serializing a live guest-memory topology change with faults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfDirtyWriteMappingMutationError {
    InvalidState(&'static str),
    UnalignedRegion,
    OverlappingRegion,
    MissingRegion,
    AllocationFailed,
}

impl fmt::Display for HvfDirtyWriteMappingMutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidState(message) => return formatter.write_str(message),
            Self::UnalignedRegion => "tracked dynamic region is not host-page aligned",
            Self::OverlappingRegion => "tracked dynamic region overlaps existing metadata",
            Self::MissingRegion => "tracked dynamic region metadata is missing",
            Self::AllocationFailed => "failed to allocate tracked dynamic region metadata",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for HvfDirtyWriteMappingMutationError {}

impl HvfDirtyWriteEpochResetError {
    /// Return whether protection is no longer safe for guest execution.
    pub fn requires_vm_teardown(&self) -> bool {
        match self {
            Self::InvalidState(_) => true,
            Self::ProtectionFailed {
                rollback_failures, ..
            } => !rollback_failures.is_empty(),
            Self::AllocationFailed => false,
        }
    }
}

impl fmt::Display for HvfDirtyWriteEpochResetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message) => {
                write!(formatter, "invalid dirty epoch reset state: {message}")
            }
            Self::AllocationFailed => {
                formatter.write_str("failed to allocate dirty epoch reset metadata")
            }
            Self::ProtectionFailed {
                failure,
                rollback_failures,
            } if rollback_failures.is_empty() => {
                write!(formatter, "failed to reset dirty epoch: {failure}")
            }
            Self::ProtectionFailed {
                failure,
                rollback_failures,
            } => write!(
                formatter,
                "failed to reset dirty epoch: {failure}; also failed to restore {} completed range(s)",
                rollback_failures.len()
            ),
        }
    }
}

impl std::error::Error for HvfDirtyWriteEpochResetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ProtectionFailed { failure, .. } => Some(failure),
            Self::InvalidState(_) | Self::AllocationFailed => None,
        }
    }
}

impl fmt::Display for HvfDirtyWriteTrackerQueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message) => {
                write!(f, "invalid dirty-write tracker query state: {message}")
            }
            Self::AllocationFailed => {
                f.write_str("failed to allocate dirty-write tracker query output")
            }
        }
    }
}

impl std::error::Error for HvfDirtyWriteTrackerQueryError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfDirtyWriteFaultError {
    InvalidState(&'static str),
    InvalidMemberIndex { index: usize },
    UnprotectFailed(HvfDirtyWriteProtectionFailure),
    NoProgress,
}

impl fmt::Display for HvfDirtyWriteFaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message) => {
                write!(f, "invalid dirty-write fault state: {message}")
            }
            Self::InvalidMemberIndex { index } => write!(
                f,
                "dirty-write fault member index {index} exceeds the supported topology"
            ),
            Self::UnprotectFailed(failure) => {
                write!(f, "failed to restore a dirty-write page: {failure}")
            }
            Self::NoProgress => {
                f.write_str("dirty-write retry made no progress for the same vCPU and page")
            }
        }
    }
}

impl std::error::Error for HvfDirtyWriteFaultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnprotectFailed(failure) => Some(failure),
            Self::InvalidState(_) | Self::InvalidMemberIndex { .. } | Self::NoProgress => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvfDirtyWriteFault {
    page: GuestAddress,
    first_write: bool,
}

impl HvfDirtyWriteFault {
    pub(crate) const fn page(self) -> GuestAddress {
        self.page
    }

    pub(crate) const fn first_write(self) -> bool {
        self.first_write
    }
}

/// Shared, preallocated guest-CPU dirty-write tracker.
pub struct HvfDirtyWriteTracker {
    state: Mutex<TrackerState>,
    owner_count: AtomicUsize,
}

pub(crate) struct HvfDirtyWriteMappingMutation<'a> {
    state: MutexGuard<'a, TrackerState>,
}

pub(crate) struct PreparedHvfDirtyWriteRegion {
    insert_index: usize,
    region: TrackedRegion,
}

impl HvfDirtyWriteTracker {
    #[cfg(test)]
    pub(crate) fn start(
        mapped_regions: &[HvfMappedGuestMemoryRegion],
        mapper: Arc<dyn HvfMemoryMapper>,
        page_size: u64,
    ) -> Result<Arc<Self>, HvfDirtyWriteTrackerStartError> {
        Self::start_internal(mapped_regions, mapper, page_size, None)
    }

    pub(crate) fn start_with_dirty_tracker(
        mapped_regions: &[HvfMappedGuestMemoryRegion],
        mapper: Arc<dyn HvfMemoryMapper>,
        page_size: u64,
        dirty_tracker: Arc<GuestMemoryDirtyTracker>,
    ) -> Result<Arc<Self>, HvfDirtyWriteTrackerStartError> {
        Self::start_internal(mapped_regions, mapper, page_size, Some(dirty_tracker))
    }

    fn start_internal(
        mapped_regions: &[HvfMappedGuestMemoryRegion],
        mapper: Arc<dyn HvfMemoryMapper>,
        page_size: u64,
        dirty_tracker: Option<Arc<GuestMemoryDirtyTracker>>,
    ) -> Result<Arc<Self>, HvfDirtyWriteTrackerStartError> {
        if page_size == 0 || !page_size.is_power_of_two() {
            return Err(HvfDirtyWriteTrackerStartError::InvalidHostPageSize { page_size });
        }
        if mapped_regions.is_empty() {
            return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "no mapped guest RAM is available",
            ));
        }

        let mut regions = Vec::new();
        regions
            .try_reserve_exact(mapped_regions.len())
            .map_err(|_| HvfDirtyWriteTrackerStartError::AllocationFailed("region metadata"))?;
        let mut previous_range: Option<GuestMemoryRange> = None;
        for mapped in mapped_regions {
            if mapped.range.validate_alignment(page_size).is_err() {
                return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                    "mapped guest RAM is not host-page aligned",
                ));
            }
            let mapped_size = u64::try_from(mapped.size).map_err(|_| {
                HvfDirtyWriteTrackerStartError::InvalidState(
                    "mapped guest RAM size exceeds the guest address space",
                )
            })?;
            if mapped.guest_address != mapped.range.start().raw_value()
                || mapped_size != mapped.range.size()
            {
                return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                    "mapped guest RAM metadata does not cover its exact range",
                ));
            }
            if previous_range
                .is_some_and(|previous| previous.end_exclusive() > mapped.range.start())
            {
                return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                    "mapped guest RAM ranges are not strictly ordered",
                ));
            }
            previous_range = Some(mapped.range);
            if !mapped.permissions.contains(HvfMemoryPermissions::WRITE) {
                continue;
            }
            let page_count = mapped.range.size() / page_size;
            let word_count = page_count
                .checked_add(63)
                .and_then(|count| usize::try_from(count / 64).ok())
                .ok_or(HvfDirtyWriteTrackerStartError::AllocationFailed(
                    "page bitmap",
                ))?;
            let mut restored_write_words = Vec::new();
            restored_write_words
                .try_reserve_exact(word_count)
                .map_err(|_| HvfDirtyWriteTrackerStartError::AllocationFailed("page bitmap"))?;
            restored_write_words.resize(word_count, 0);
            regions.push(TrackedRegion {
                range: mapped.range,
                original_permissions: mapped.permissions,
                restored_write_words,
            });
        }
        if regions.is_empty() {
            return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "no mapped writable guest RAM is available",
            ));
        }
        let dirty_tracker = match dirty_tracker {
            Some(dirty_tracker) => {
                if dirty_tracker.page_size() != page_size
                    || regions
                        .iter()
                        .any(|region| !dirty_tracker.contains_range(region.range))
                {
                    return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                        "dirty bitmap does not cover mapped writable guest RAM",
                    ));
                }
                dirty_tracker
            }
            None => Arc::new(GuestMemoryDirtyTracker::new(
                regions.iter().map(|region| region.range),
                page_size,
            )?),
        };

        let mut rollback_failures = Vec::new();
        rollback_failures
            .try_reserve_exact(regions.len())
            .map_err(|_| {
                HvfDirtyWriteTrackerStartError::AllocationFailed("rollback failure metadata")
            })?;
        for (index, region) in regions.iter().enumerate() {
            let protected = region
                .original_permissions
                .without(HvfMemoryPermissions::WRITE);
            if let Err(source) = mapper.protect_region(region.range, protected) {
                for (rollback_index, completed) in regions.iter().take(index).enumerate().rev() {
                    if let Err(source) =
                        mapper.protect_region(completed.range, completed.original_permissions)
                    {
                        rollback_failures.push(HvfDirtyWriteProtectionFailure {
                            operation_index: rollback_index,
                            source,
                        });
                    }
                }
                return Err(HvfDirtyWriteTrackerStartError::ProtectionFailed {
                    failure: HvfDirtyWriteProtectionFailure {
                        operation_index: index,
                        source,
                    },
                    rollback_failures,
                });
            }
        }

        Ok(Arc::new(Self {
            state: Mutex::new(TrackerState {
                status: TrackerStatus::Active,
                mapper,
                page_size,
                regions,
                dirty_tracker,
                last_admitted_pages: [None; MAX_SUPPORTED_VCPUS as usize],
            }),
            owner_count: AtomicUsize::new(0),
        }))
    }

    pub fn is_active(&self) -> Result<bool, HvfDirtyWriteTrackerQueryError> {
        let state = self.lock_query()?;
        match state.status {
            TrackerStatus::Active => Ok(true),
            TrackerStatus::Stopped => Ok(false),
            TrackerStatus::Stopping => Err(HvfDirtyWriteTrackerQueryError::InvalidState(
                "dirty-write tracker is stopping",
            )),
            TrackerStatus::Poisoned => Err(HvfDirtyWriteTrackerQueryError::InvalidState(
                TRACKER_POISONED_MESSAGE,
            )),
        }
    }

    pub fn dirty_pages(&self) -> Result<Vec<GuestAddress>, HvfDirtyWriteTrackerQueryError> {
        let state = self.lock_query()?;
        match state.status {
            TrackerStatus::Active | TrackerStatus::Stopped => {}
            TrackerStatus::Stopping => {
                return Err(HvfDirtyWriteTrackerQueryError::InvalidState(
                    "dirty-write tracker is stopping",
                ));
            }
            TrackerStatus::Poisoned => {
                return Err(HvfDirtyWriteTrackerQueryError::InvalidState(
                    TRACKER_POISONED_MESSAGE,
                ));
            }
        }
        state
            .dirty_tracker
            .dirty_pages()
            .map_err(|source| match source {
                GuestMemoryDirtyTrackerAccessError::MetadataAllocationFailed { .. } => {
                    HvfDirtyWriteTrackerQueryError::AllocationFailed
                }
                GuestMemoryDirtyTrackerAccessError::InvalidState(message) => {
                    HvfDirtyWriteTrackerQueryError::InvalidState(message)
                }
                GuestMemoryDirtyTrackerAccessError::UntrackedRange { .. } => {
                    HvfDirtyWriteTrackerQueryError::InvalidState(
                        "dirty bitmap does not cover mapped writable guest RAM",
                    )
                }
            })
    }

    pub(crate) fn register_owner(
        &self,
        member_index: usize,
    ) -> Result<(), HvfDirtyWriteFaultError> {
        if member_index >= usize::from(MAX_SUPPORTED_VCPUS) {
            return Err(HvfDirtyWriteFaultError::InvalidMemberIndex {
                index: member_index,
            });
        }
        let state = self.lock_fault()?;
        state.ensure_active()?;
        self.owner_count.fetch_add(1, Ordering::AcqRel);
        drop(state);
        Ok(())
    }

    pub(crate) fn unregister_owner(&self) {
        let _ = self
            .owner_count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_sub(1)
            });
    }

    pub(crate) fn begin_mapping_mutation(
        &self,
    ) -> Result<HvfDirtyWriteMappingMutation<'_>, HvfDirtyWriteMappingMutationError> {
        let state = self.state.lock().map_err(|_| {
            HvfDirtyWriteMappingMutationError::InvalidState(TRACKER_LOCK_POISONED_MESSAGE)
        })?;
        match state.status {
            TrackerStatus::Active => Ok(HvfDirtyWriteMappingMutation { state }),
            TrackerStatus::Poisoned => Err(HvfDirtyWriteMappingMutationError::InvalidState(
                TRACKER_POISONED_MESSAGE,
            )),
            TrackerStatus::Stopping | TrackerStatus::Stopped => Err(
                HvfDirtyWriteMappingMutationError::InvalidState(TRACKER_NOT_ACTIVE_MESSAGE),
            ),
        }
    }

    pub(crate) fn handle_exception(
        &self,
        member_index: usize,
        exit: HvfExceptionExit,
    ) -> Result<Option<HvfDirtyWriteFault>, HvfDirtyWriteFaultError> {
        if !exit.matches_observed_hvf_protected_write_syndrome() {
            return Ok(None);
        }
        if member_index >= usize::from(MAX_SUPPORTED_VCPUS) {
            return Err(HvfDirtyWriteFaultError::InvalidMemberIndex {
                index: member_index,
            });
        }

        let mut state = self.lock_fault()?;
        state.ensure_active()?;
        let page_address = exit.physical_address & !(state.page_size - 1);
        if page_address.checked_add(state.page_size).is_none() {
            return Ok(None);
        }
        let page = GuestAddress::new(page_address);
        let Some(region_index) = state
            .regions
            .iter()
            .position(|region| region.range.contains(page))
        else {
            return Ok(None);
        };
        let page_index = state.page_index(region_index, page)?;
        let first_write = !state.page_has_restored_write(region_index, page_index)?;
        if !first_write {
            // Another member may already have exited before the first member
            // restored WRITE. Once restored, this mapped RAM page cannot raise
            // a new stage-two translation exit until a serialized epoch reset
            // re-protects it or a serialized mapping transaction removes it.
            // Admit that stale exit once per member; an immediate repeat proves
            // the unchanged instruction made no progress.
            let last = state.last_admitted_pages.get_mut(member_index).ok_or(
                HvfDirtyWriteFaultError::InvalidMemberIndex {
                    index: member_index,
                },
            )?;
            if *last == Some(page_address) {
                return Err(HvfDirtyWriteFaultError::NoProgress);
            }
            *last = Some(page_address);
            return Ok(Some(HvfDirtyWriteFault {
                page,
                first_write: false,
            }));
        }

        let page_range = GuestMemoryRange::new(page, state.page_size).map_err(|_| {
            HvfDirtyWriteFaultError::InvalidState("dirty-write page range is invalid")
        })?;
        let original_permissions = state
            .regions
            .get(region_index)
            .ok_or(HvfDirtyWriteFaultError::InvalidState(
                "dirty-write region disappeared",
            ))?
            .original_permissions;
        if let Err(source) = state
            .mapper
            .protect_region(page_range, original_permissions)
        {
            state.status = TrackerStatus::Poisoned;
            return Err(HvfDirtyWriteFaultError::UnprotectFailed(
                HvfDirtyWriteProtectionFailure {
                    operation_index: 0,
                    source,
                },
            ));
        }
        state.set_page_restored_write(region_index, page_index)?;
        if state.dirty_tracker.mark_range(page_range).is_err() {
            state.status = TrackerStatus::Poisoned;
            return Err(HvfDirtyWriteFaultError::InvalidState(
                "dirty bitmap does not cover a protected page",
            ));
        }
        let last = state.last_admitted_pages.get_mut(member_index).ok_or(
            HvfDirtyWriteFaultError::InvalidMemberIndex {
                index: member_index,
            },
        )?;
        *last = Some(page_address);
        Ok(Some(HvfDirtyWriteFault {
            page,
            first_write: true,
        }))
    }

    /// Re-protect every page made writable in this generation and clear it.
    ///
    /// The caller must hold snapshot-ready quiescence across this complete
    /// operation. Permanent vCPU owners may exist, but none may enter the guest
    /// or publish a host/device write until this method returns.
    pub fn reset_epoch_quiesced(&self) -> Result<u64, HvfDirtyWriteEpochResetError> {
        let mut state = self.state.lock().map_err(|_| {
            HvfDirtyWriteEpochResetError::InvalidState(TRACKER_LOCK_POISONED_MESSAGE)
        })?;
        match state.status {
            TrackerStatus::Active => {}
            TrackerStatus::Poisoned => {
                return Err(HvfDirtyWriteEpochResetError::InvalidState(
                    TRACKER_POISONED_MESSAGE,
                ));
            }
            TrackerStatus::Stopping | TrackerStatus::Stopped => {
                return Err(HvfDirtyWriteEpochResetError::InvalidState(
                    TRACKER_NOT_ACTIVE_MESSAGE,
                ));
            }
        }

        let run_count = state.restored_write_run_count()?;
        let mut completed = Vec::new();
        completed
            .try_reserve_exact(run_count)
            .map_err(|_| HvfDirtyWriteEpochResetError::AllocationFailed)?;
        let mut rollback_failures = Vec::new();
        rollback_failures
            .try_reserve_exact(run_count)
            .map_err(|_| HvfDirtyWriteEpochResetError::AllocationFailed)?;

        for region_index in 0..state.regions.len() {
            let page_count = state.region_page_count_for_reset(region_index)?;
            let mut page_index = 0usize;
            while page_index < page_count {
                while page_index < page_count
                    && !state.page_has_restored_write_for_reset(region_index, page_index)?
                {
                    page_index += 1;
                }
                if page_index == page_count {
                    break;
                }
                let run_start = page_index;
                while page_index < page_count
                    && state.page_has_restored_write_for_reset(region_index, page_index)?
                {
                    page_index += 1;
                }
                let (range, original_permissions) =
                    state.restored_write_run(region_index, run_start, page_index)?;
                let protected = original_permissions.without(HvfMemoryPermissions::WRITE);
                let operation_index = completed.len();
                if let Err(source) = state.mapper.protect_region(range, protected) {
                    for (rollback_index, (completed_range, permissions)) in
                        completed.iter().enumerate().rev()
                    {
                        if let Err(source) =
                            state.mapper.protect_region(*completed_range, *permissions)
                        {
                            rollback_failures.push(HvfDirtyWriteProtectionFailure {
                                operation_index: rollback_index,
                                source,
                            });
                        }
                    }
                    if !rollback_failures.is_empty() {
                        state.status = TrackerStatus::Poisoned;
                    }
                    return Err(HvfDirtyWriteEpochResetError::ProtectionFailed {
                        failure: HvfDirtyWriteProtectionFailure {
                            operation_index,
                            source,
                        },
                        rollback_failures,
                    });
                }
                completed.push((range, original_permissions));
            }
        }

        state.clear_restored_writes();
        state.last_admitted_pages.fill(None);
        Ok(state.dirty_tracker.clear_quiesced())
    }

    pub fn stop(&self) -> Result<(), HvfDirtyWriteTrackerStopError> {
        let mut state = self.state.lock().map_err(|_| {
            HvfDirtyWriteTrackerStopError::InvalidState(TRACKER_LOCK_POISONED_MESSAGE)
        })?;
        let owner_count = self.owner_count.load(Ordering::Acquire);
        if owner_count != 0 {
            return Err(HvfDirtyWriteTrackerStopError::OwnersActive { count: owner_count });
        }
        if state.status == TrackerStatus::Stopped {
            return Ok(());
        }
        state.status = TrackerStatus::Stopping;
        let mut failures = Vec::new();
        failures
            .try_reserve_exact(MAX_REPORTED_STOP_FAILURES)
            .map_err(|_| {
                state.status = TrackerStatus::Poisoned;
                HvfDirtyWriteTrackerStopError::InvalidState(
                    "failed to reserve stop failure metadata",
                )
            })?;
        let mut omitted_failures = 0usize;
        let mut operation_index = 0usize;
        for region_index in 0..state.regions.len() {
            let page_count = state.region_page_count(region_index)?;
            let mut page_index = 0usize;
            while page_index < page_count {
                while page_index < page_count
                    && state.page_has_restored_write_for_stop(region_index, page_index)?
                {
                    page_index += 1;
                }
                if page_index == page_count {
                    break;
                }
                let run_start = page_index;
                while page_index < page_count
                    && !state.page_has_restored_write_for_stop(region_index, page_index)?
                {
                    page_index += 1;
                }
                let (range, permissions) = state.clean_run(region_index, run_start, page_index)?;
                if let Err(source) = state.mapper.protect_region(range, permissions) {
                    if failures.len() < MAX_REPORTED_STOP_FAILURES {
                        failures.push(HvfDirtyWriteProtectionFailure {
                            operation_index,
                            source,
                        });
                    } else {
                        omitted_failures = omitted_failures.saturating_add(1);
                    }
                }
                operation_index = operation_index.saturating_add(1);
            }
        }
        if failures.is_empty() && omitted_failures == 0 {
            state.status = TrackerStatus::Stopped;
            Ok(())
        } else {
            state.status = TrackerStatus::Poisoned;
            Err(HvfDirtyWriteTrackerStopError::ProtectionFailed {
                failures,
                omitted_failures,
            })
        }
    }

    fn lock_query(&self) -> Result<MutexGuard<'_, TrackerState>, HvfDirtyWriteTrackerQueryError> {
        self.state.lock().map_err(|_| {
            HvfDirtyWriteTrackerQueryError::InvalidState(TRACKER_LOCK_POISONED_MESSAGE)
        })
    }

    fn lock_fault(&self) -> Result<MutexGuard<'_, TrackerState>, HvfDirtyWriteFaultError> {
        self.state
            .lock()
            .map_err(|_| HvfDirtyWriteFaultError::InvalidState(TRACKER_LOCK_POISONED_MESSAGE))
    }
}

impl HvfDirtyWriteMappingMutation<'_> {
    pub(crate) fn prepare_add(
        &mut self,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    ) -> Result<Option<PreparedHvfDirtyWriteRegion>, HvfDirtyWriteMappingMutationError> {
        if !permissions.contains(HvfMemoryPermissions::WRITE) {
            return Ok(None);
        }
        if range.validate_alignment(self.state.page_size).is_err() {
            return Err(HvfDirtyWriteMappingMutationError::UnalignedRegion);
        }
        let insert_index = self
            .state
            .regions
            .iter()
            .position(|region| range.start() < region.range.start())
            .unwrap_or(self.state.regions.len());
        if self
            .state
            .regions
            .iter()
            .any(|region| region.range.overlaps(range))
        {
            return Err(HvfDirtyWriteMappingMutationError::OverlappingRegion);
        }
        self.state
            .regions
            .try_reserve_exact(1)
            .map_err(|_| HvfDirtyWriteMappingMutationError::AllocationFailed)?;
        let page_count = usize::try_from(range.size() / self.state.page_size)
            .map_err(|_| HvfDirtyWriteMappingMutationError::AllocationFailed)?;
        let word_count = page_count
            .checked_add(63)
            .map(|count| count / 64)
            .ok_or(HvfDirtyWriteMappingMutationError::AllocationFailed)?;
        let mut restored_write_words = Vec::new();
        restored_write_words
            .try_reserve_exact(word_count)
            .map_err(|_| HvfDirtyWriteMappingMutationError::AllocationFailed)?;
        restored_write_words.resize(word_count, 0);
        Ok(Some(PreparedHvfDirtyWriteRegion {
            insert_index,
            region: TrackedRegion {
                range,
                original_permissions: permissions,
                restored_write_words,
            },
        }))
    }

    pub(crate) fn commit_add(&mut self, prepared: PreparedHvfDirtyWriteRegion) {
        debug_assert!(
            self.state
                .dirty_tracker
                .contains_range(prepared.region.range),
            "guest-memory dirty metadata must precede protected mapping publication"
        );
        self.state
            .regions
            .insert(prepared.insert_index, prepared.region);
    }

    pub(crate) fn prepare_remove(
        &self,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    ) -> Result<Option<usize>, HvfDirtyWriteMappingMutationError> {
        if !permissions.contains(HvfMemoryPermissions::WRITE) {
            return Ok(None);
        }
        self.state
            .regions
            .iter()
            .position(|region| region.range == range)
            .map(Some)
            .ok_or(HvfDirtyWriteMappingMutationError::MissingRegion)
    }

    pub(crate) fn commit_remove(&mut self, region_index: Option<usize>) {
        if let Some(region_index) = region_index {
            self.state.regions.remove(region_index);
        }
    }
}

impl fmt::Debug for HvfDirtyWriteTracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let owners = self.owner_count.load(Ordering::Acquire);
        match self.state.lock() {
            Ok(state) => f
                .debug_struct("HvfDirtyWriteTracker")
                .field("status", &state.status)
                .field("region_count", &state.regions.len())
                .field("owner_count", &owners)
                .finish(),
            Err(_) => f
                .debug_struct("HvfDirtyWriteTracker")
                .field("status", &"poisoned")
                .field("owner_count", &owners)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackerStatus {
    Active,
    Stopping,
    Poisoned,
    Stopped,
}

struct TrackerState {
    status: TrackerStatus,
    mapper: Arc<dyn HvfMemoryMapper>,
    page_size: u64,
    regions: Vec<TrackedRegion>,
    dirty_tracker: Arc<GuestMemoryDirtyTracker>,
    last_admitted_pages: [Option<u64>; MAX_SUPPORTED_VCPUS as usize],
}

impl TrackerState {
    fn ensure_active(&self) -> Result<(), HvfDirtyWriteFaultError> {
        match self.status {
            TrackerStatus::Active => Ok(()),
            TrackerStatus::Poisoned => Err(HvfDirtyWriteFaultError::InvalidState(
                TRACKER_POISONED_MESSAGE,
            )),
            TrackerStatus::Stopping | TrackerStatus::Stopped => Err(
                HvfDirtyWriteFaultError::InvalidState(TRACKER_NOT_ACTIVE_MESSAGE),
            ),
        }
    }

    fn page_index(
        &self,
        region_index: usize,
        page: GuestAddress,
    ) -> Result<usize, HvfDirtyWriteFaultError> {
        let region =
            self.regions
                .get(region_index)
                .ok_or(HvfDirtyWriteFaultError::InvalidState(
                    "dirty-write region index is invalid",
                ))?;
        let offset = page
            .raw_value()
            .checked_sub(region.range.start().raw_value())
            .ok_or(HvfDirtyWriteFaultError::InvalidState(
                "dirty-write page precedes its region",
            ))?;
        usize::try_from(offset / self.page_size).map_err(|_| {
            HvfDirtyWriteFaultError::InvalidState("dirty-write page index exceeds this host")
        })
    }

    fn bitmap_location(page_index: usize) -> (usize, u64) {
        (page_index / 64, 1u64 << (page_index % 64))
    }

    fn page_has_restored_write(
        &self,
        region_index: usize,
        page_index: usize,
    ) -> Result<bool, HvfDirtyWriteFaultError> {
        let (word_index, bit) = Self::bitmap_location(page_index);
        let word = self
            .regions
            .get(region_index)
            .and_then(|region| region.restored_write_words.get(word_index))
            .ok_or(HvfDirtyWriteFaultError::InvalidState(
                "dirty-write page is outside its protection bitmap",
            ))?;
        Ok(*word & bit != 0)
    }

    fn set_page_restored_write(
        &mut self,
        region_index: usize,
        page_index: usize,
    ) -> Result<(), HvfDirtyWriteFaultError> {
        let (word_index, bit) = Self::bitmap_location(page_index);
        let word = self
            .regions
            .get_mut(region_index)
            .and_then(|region| region.restored_write_words.get_mut(word_index))
            .ok_or(HvfDirtyWriteFaultError::InvalidState(
                "dirty-write page is outside its protection bitmap",
            ))?;
        *word |= bit;
        Ok(())
    }

    fn region_page_count(
        &self,
        region_index: usize,
    ) -> Result<usize, HvfDirtyWriteTrackerStopError> {
        let region =
            self.regions
                .get(region_index)
                .ok_or(HvfDirtyWriteTrackerStopError::InvalidState(
                    "tracked region index is invalid",
                ))?;
        usize::try_from(region.range.size() / self.page_size).map_err(|_| {
            HvfDirtyWriteTrackerStopError::InvalidState(
                "tracked region page count exceeds this host",
            )
        })
    }

    fn page_has_restored_write_for_stop(
        &self,
        region_index: usize,
        page_index: usize,
    ) -> Result<bool, HvfDirtyWriteTrackerStopError> {
        let (word_index, bit) = Self::bitmap_location(page_index);
        let word = self
            .regions
            .get(region_index)
            .and_then(|region| region.restored_write_words.get(word_index))
            .ok_or(HvfDirtyWriteTrackerStopError::InvalidState(
                "tracked page is outside its protection bitmap",
            ))?;
        Ok(*word & bit != 0)
    }

    fn restored_write_run_count(&self) -> Result<usize, HvfDirtyWriteEpochResetError> {
        let mut count = 0usize;
        for region_index in 0..self.regions.len() {
            let page_count = self.region_page_count_for_reset(region_index)?;
            let mut in_run = false;
            for page_index in 0..page_count {
                let restored = self.page_has_restored_write_for_reset(region_index, page_index)?;
                if restored && !in_run {
                    count = count
                        .checked_add(1)
                        .ok_or(HvfDirtyWriteEpochResetError::AllocationFailed)?;
                }
                in_run = restored;
            }
        }
        Ok(count)
    }

    fn region_page_count_for_reset(
        &self,
        region_index: usize,
    ) -> Result<usize, HvfDirtyWriteEpochResetError> {
        let region =
            self.regions
                .get(region_index)
                .ok_or(HvfDirtyWriteEpochResetError::InvalidState(
                    "tracked region index is invalid",
                ))?;
        usize::try_from(region.range.size() / self.page_size).map_err(|_| {
            HvfDirtyWriteEpochResetError::InvalidState(
                "tracked region page count exceeds this host",
            )
        })
    }

    fn page_has_restored_write_for_reset(
        &self,
        region_index: usize,
        page_index: usize,
    ) -> Result<bool, HvfDirtyWriteEpochResetError> {
        let (word_index, bit) = Self::bitmap_location(page_index);
        let word = self
            .regions
            .get(region_index)
            .and_then(|region| region.restored_write_words.get(word_index))
            .ok_or(HvfDirtyWriteEpochResetError::InvalidState(
                "tracked page is outside its protection bitmap",
            ))?;
        Ok(*word & bit != 0)
    }

    fn restored_write_run(
        &self,
        region_index: usize,
        start_page: usize,
        end_page: usize,
    ) -> Result<(GuestMemoryRange, HvfMemoryPermissions), HvfDirtyWriteEpochResetError> {
        let region =
            self.regions
                .get(region_index)
                .ok_or(HvfDirtyWriteEpochResetError::InvalidState(
                    "tracked region index is invalid",
                ))?;
        let start_page = u64::try_from(start_page).map_err(|_| {
            HvfDirtyWriteEpochResetError::InvalidState(
                "restored-write range start exceeds this host",
            )
        })?;
        let end_page = u64::try_from(end_page).map_err(|_| {
            HvfDirtyWriteEpochResetError::InvalidState("restored-write range end exceeds this host")
        })?;
        let offset = start_page.checked_mul(self.page_size).ok_or(
            HvfDirtyWriteEpochResetError::InvalidState("restored-write range start overflowed"),
        )?;
        let size = end_page
            .checked_sub(start_page)
            .and_then(|pages| pages.checked_mul(self.page_size))
            .ok_or(HvfDirtyWriteEpochResetError::InvalidState(
                "restored-write range size overflowed",
            ))?;
        let start = region.range.start().checked_add(offset).ok_or(
            HvfDirtyWriteEpochResetError::InvalidState("restored-write range address overflowed"),
        )?;
        let range = GuestMemoryRange::new(start, size).map_err(|_| {
            HvfDirtyWriteEpochResetError::InvalidState("restored-write range is invalid")
        })?;
        Ok((range, region.original_permissions))
    }

    fn clear_restored_writes(&mut self) {
        for region in &mut self.regions {
            region.restored_write_words.fill(0);
        }
    }

    fn clean_run(
        &self,
        region_index: usize,
        start_page: usize,
        end_page: usize,
    ) -> Result<(GuestMemoryRange, HvfMemoryPermissions), HvfDirtyWriteTrackerStopError> {
        let region =
            self.regions
                .get(region_index)
                .ok_or(HvfDirtyWriteTrackerStopError::InvalidState(
                    "tracked region index is invalid",
                ))?;
        let start_page = u64::try_from(start_page).map_err(|_| {
            HvfDirtyWriteTrackerStopError::InvalidState("clean range start exceeds this host")
        })?;
        let end_page = u64::try_from(end_page).map_err(|_| {
            HvfDirtyWriteTrackerStopError::InvalidState("clean range end exceeds this host")
        })?;
        let offset = start_page.checked_mul(self.page_size).ok_or(
            HvfDirtyWriteTrackerStopError::InvalidState("clean range start overflowed"),
        )?;
        let size = end_page
            .checked_sub(start_page)
            .and_then(|pages| pages.checked_mul(self.page_size))
            .ok_or(HvfDirtyWriteTrackerStopError::InvalidState(
                "clean range size overflowed",
            ))?;
        let start = region.range.start().checked_add(offset).ok_or(
            HvfDirtyWriteTrackerStopError::InvalidState("clean range address overflowed"),
        )?;
        let range = GuestMemoryRange::new(start, size)
            .map_err(|_| HvfDirtyWriteTrackerStopError::InvalidState("clean range is invalid"))?;
        Ok((range, region.original_permissions))
    }
}

struct TrackedRegion {
    range: GuestMemoryRange,
    original_permissions: HvfMemoryPermissions,
    restored_write_words: Vec<u64>,
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier, Mutex};
    use std::thread;

    use bangbang_runtime::BackendError;
    use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange};

    use super::{
        HvfDirtyWriteEpochResetError, HvfDirtyWriteFaultError, HvfDirtyWriteTracker,
        HvfDirtyWriteTrackerStartError, HvfDirtyWriteTrackerStopError,
    };
    use crate::exit::HvfExceptionExit;
    use crate::memory::{
        HvfMappedGuestMemoryRegion, HvfMemoryMapRequest, HvfMemoryMapper, HvfMemoryPermissions,
    };

    const PAGE_SIZE: u64 = 0x1000;
    const ESR_EC_DATA_ABORT_LOWER_EL: u64 = 0x24;
    const ESR_EC_SHIFT: u64 = 26;
    const ESR_ISS_WNR: u64 = 1 << 6;
    const ESR_ISS_LEVEL_THREE_TRANSLATION: u64 = 0x07;
    const ESR_ISS_LEVEL_THREE_PERMISSION: u64 = 0x0f;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct ProtectionCall {
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    }

    #[derive(Debug, Default)]
    struct RecordingMapper {
        state: Mutex<RecordingMapperState>,
    }

    #[derive(Debug, Default)]
    struct RecordingMapperState {
        calls: Vec<ProtectionCall>,
        failing_call_indexes: Vec<usize>,
    }

    impl RecordingMapper {
        fn failing_on(failing_call_indexes: &[usize]) -> Self {
            Self {
                state: Mutex::new(RecordingMapperState {
                    calls: Vec::new(),
                    failing_call_indexes: failing_call_indexes.to_vec(),
                }),
            }
        }

        fn calls(&self) -> Vec<ProtectionCall> {
            self.state
                .lock()
                .expect("recording mapper lock should be available")
                .calls
                .clone()
        }
    }

    impl HvfMemoryMapper for RecordingMapper {
        fn map_region(
            &self,
            _request: HvfMemoryMapRequest,
            _permissions: HvfMemoryPermissions,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn unmap_region(
            &self,
            _mapped_region: HvfMappedGuestMemoryRegion,
        ) -> Result<(), BackendError> {
            Ok(())
        }

        fn protect_region(
            &self,
            range: GuestMemoryRange,
            permissions: HvfMemoryPermissions,
        ) -> Result<(), BackendError> {
            let mut state = self
                .state
                .lock()
                .expect("recording mapper lock should be available");
            let call_index = state.calls.len();
            state.calls.push(ProtectionCall { range, permissions });
            if state.failing_call_indexes.contains(&call_index) {
                Err(BackendError::Hypervisor(format!(
                    "injected protection failure {call_index}"
                )))
            } else {
                Ok(())
            }
        }
    }

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size).expect("test range should be valid")
    }

    fn mapped_region(
        start: u64,
        page_count: u64,
        permissions: HvfMemoryPermissions,
    ) -> HvfMappedGuestMemoryRegion {
        let size = page_count
            .checked_mul(PAGE_SIZE)
            .expect("test mapping size should not overflow");
        HvfMappedGuestMemoryRegion {
            range: range(start, size),
            guest_address: start,
            size: usize::try_from(size).expect("test mapping should fit this host"),
            permissions,
        }
    }

    fn tracked_write_fault(physical_address: u64) -> HvfExceptionExit {
        tracked_write_fault_with_dfsc(physical_address, ESR_ISS_LEVEL_THREE_TRANSLATION)
    }

    fn reprotected_write_fault(physical_address: u64) -> HvfExceptionExit {
        tracked_write_fault_with_dfsc(physical_address, ESR_ISS_LEVEL_THREE_PERMISSION)
    }

    fn tracked_write_fault_with_dfsc(physical_address: u64, dfsc: u64) -> HvfExceptionExit {
        HvfExceptionExit {
            syndrome: (ESR_EC_DATA_ABORT_LOWER_EL << ESR_EC_SHIFT) | ESR_ISS_WNR | dfsc,
            virtual_address: 0xfeed_face,
            physical_address,
        }
    }

    fn start_tracker(
        mapped_regions: &[HvfMappedGuestMemoryRegion],
        mapper: Arc<RecordingMapper>,
    ) -> Arc<HvfDirtyWriteTracker> {
        HvfDirtyWriteTracker::start(mapped_regions, mapper, PAGE_SIZE)
            .expect("tracker should start")
    }

    #[test]
    fn validates_all_mapping_metadata_before_protection() {
        let mapper = Arc::new(RecordingMapper::default());

        assert!(matches!(
            HvfDirtyWriteTracker::start(&[], mapper.clone(), PAGE_SIZE),
            Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "no mapped guest RAM is available"
            ))
        ));
        assert!(matches!(
            HvfDirtyWriteTracker::start(
                &[mapped_region(
                    0,
                    1,
                    HvfMemoryPermissions::new(true, false, false)
                )],
                mapper.clone(),
                PAGE_SIZE
            ),
            Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "no mapped writable guest RAM is available"
            ))
        ));

        let mut mismatched = mapped_region(0, 1, HvfMemoryPermissions::GUEST_RAM);
        mismatched.guest_address = PAGE_SIZE;
        assert!(matches!(
            HvfDirtyWriteTracker::start(&[mismatched], mapper.clone(), PAGE_SIZE),
            Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "mapped guest RAM metadata does not cover its exact range"
            ))
        ));

        let overlapping = [
            mapped_region(PAGE_SIZE, 2, HvfMemoryPermissions::GUEST_RAM),
            mapped_region(PAGE_SIZE * 2, 1, HvfMemoryPermissions::GUEST_RAM),
        ];
        assert!(matches!(
            HvfDirtyWriteTracker::start(&overlapping, mapper.clone(), PAGE_SIZE),
            Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "mapped guest RAM ranges are not strictly ordered"
            ))
        ));
        assert!(matches!(
            HvfDirtyWriteTracker::start(
                &[mapped_region(0, 1, HvfMemoryPermissions::GUEST_RAM)],
                mapper.clone(),
                3
            ),
            Err(HvfDirtyWriteTrackerStartError::InvalidHostPageSize { page_size: 3 })
        ));
        assert!(matches!(
            HvfDirtyWriteTracker::start(
                &[mapped_region(1, 1, HvfMemoryPermissions::GUEST_RAM)],
                mapper.clone(),
                PAGE_SIZE
            ),
            Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "mapped guest RAM is not host-page aligned"
            ))
        ));
        assert!(mapper.calls().is_empty());
    }

    #[test]
    fn protects_only_writable_ranges_without_elevating_permissions() {
        let mapper = Arc::new(RecordingMapper::default());
        let read_only = HvfMemoryPermissions::new(true, false, false);
        let read_write = HvfMemoryPermissions::new(true, true, false);
        let regions = [
            mapped_region(0, 1, read_only),
            mapped_region(PAGE_SIZE, 2, read_write),
            mapped_region(PAGE_SIZE * 3, 1, HvfMemoryPermissions::GUEST_RAM),
        ];

        let tracker = start_tracker(&regions, mapper.clone());

        assert_eq!(
            mapper.calls(),
            vec![
                ProtectionCall {
                    range: range(PAGE_SIZE, PAGE_SIZE * 2),
                    permissions: read_only,
                },
                ProtectionCall {
                    range: range(PAGE_SIZE * 3, PAGE_SIZE),
                    permissions: HvfMemoryPermissions::new(true, false, true),
                },
            ]
        );
        assert!(tracker.is_active().expect("tracker query should succeed"));
    }

    #[test]
    fn rolls_back_completed_ranges_in_reverse_order() {
        let mapper = Arc::new(RecordingMapper::failing_on(&[2]));
        let regions = [
            mapped_region(0, 1, HvfMemoryPermissions::GUEST_RAM),
            mapped_region(PAGE_SIZE, 1, HvfMemoryPermissions::GUEST_RAM),
            mapped_region(PAGE_SIZE * 2, 1, HvfMemoryPermissions::GUEST_RAM),
        ];

        let error = HvfDirtyWriteTracker::start(&regions, mapper.clone(), PAGE_SIZE)
            .expect_err("third protection call should fail");

        let HvfDirtyWriteTrackerStartError::ProtectionFailed {
            failure,
            rollback_failures,
        } = error
        else {
            panic!("expected protection failure");
        };
        assert_eq!(failure.operation_index(), 2);
        assert!(rollback_failures.is_empty());
        assert!(!error_requires_teardown(&failure, &rollback_failures));
        assert_eq!(
            mapper.calls(),
            vec![
                ProtectionCall {
                    range: range(0, PAGE_SIZE),
                    permissions: HvfMemoryPermissions::new(true, false, true),
                },
                ProtectionCall {
                    range: range(PAGE_SIZE, PAGE_SIZE),
                    permissions: HvfMemoryPermissions::new(true, false, true),
                },
                ProtectionCall {
                    range: range(PAGE_SIZE * 2, PAGE_SIZE),
                    permissions: HvfMemoryPermissions::new(true, false, true),
                },
                ProtectionCall {
                    range: range(PAGE_SIZE, PAGE_SIZE),
                    permissions: HvfMemoryPermissions::GUEST_RAM,
                },
                ProtectionCall {
                    range: range(0, PAGE_SIZE),
                    permissions: HvfMemoryPermissions::GUEST_RAM,
                },
            ]
        );
    }

    fn error_requires_teardown(
        failure: &super::HvfDirtyWriteProtectionFailure,
        rollback_failures: &[super::HvfDirtyWriteProtectionFailure],
    ) -> bool {
        HvfDirtyWriteTrackerStartError::ProtectionFailed {
            failure: failure.clone(),
            rollback_failures: rollback_failures.to_vec(),
        }
        .requires_vm_teardown()
    }

    #[test]
    fn reports_incomplete_initial_rollback_without_guest_addresses() {
        let mapper = Arc::new(RecordingMapper::failing_on(&[1, 2]));
        let regions = [
            mapped_region(0x1234_5000, 1, HvfMemoryPermissions::GUEST_RAM),
            mapped_region(0x1234_6000, 1, HvfMemoryPermissions::GUEST_RAM),
        ];

        let error = HvfDirtyWriteTracker::start(&regions, mapper, PAGE_SIZE)
            .expect_err("protection and rollback should fail");

        assert!(error.requires_vm_teardown());
        let display = error.to_string();
        assert!(!display.contains("12345000"));
        assert!(!display.contains("12346000"));
        let HvfDirtyWriteTrackerStartError::ProtectionFailed {
            failure,
            rollback_failures,
        } = error
        else {
            panic!("expected protection failure");
        };
        assert_eq!(failure.operation_index(), 1);
        assert_eq!(rollback_failures.len(), 1);
        assert_eq!(rollback_failures[0].operation_index(), 0);
    }

    #[test]
    fn first_owned_write_unprotects_marks_and_retries_without_accepting_other_faults() {
        let mapper = Arc::new(RecordingMapper::default());
        let tracker = start_tracker(
            &[mapped_region(0x4000, 2, HvfMemoryPermissions::GUEST_RAM)],
            mapper.clone(),
        );
        tracker
            .register_owner(0)
            .expect("owner registration should succeed");

        let mut read_fault = tracked_write_fault(0x4008);
        read_fault.syndrome &= !ESR_ISS_WNR;
        assert_eq!(
            tracker
                .handle_exception(0, read_fault)
                .expect("read fault should remain unhandled"),
            None
        );
        assert_eq!(
            tracker
                .handle_exception(0, tracked_write_fault(0x9008))
                .expect("unowned write fault should remain unhandled"),
            None
        );

        let handled = tracker
            .handle_exception(0, tracked_write_fault(0x4abc))
            .expect("owned write fault should succeed")
            .expect("owned write fault should be handled");
        assert_eq!(handled.page(), GuestAddress::new(0x4000));
        assert!(handled.first_write());
        assert_eq!(
            tracker.dirty_pages().expect("query should succeed"),
            vec![GuestAddress::new(0x4000)]
        );
        assert_eq!(
            mapper.calls().last(),
            Some(&ProtectionCall {
                range: range(0x4000, PAGE_SIZE),
                permissions: HvfMemoryPermissions::GUEST_RAM,
            })
        );

        assert_eq!(
            tracker.handle_exception(0, tracked_write_fault(0x4000)),
            Err(HvfDirtyWriteFaultError::NoProgress)
        );
        tracker.unregister_owner();
        tracker.stop().expect("tracker should stop");
    }

    #[test]
    fn reprotected_permission_fault_still_requires_exact_tracker_ownership() {
        let mapper = Arc::new(RecordingMapper::default());
        let tracker = start_tracker(
            &[mapped_region(0x5000, 1, HvfMemoryPermissions::GUEST_RAM)],
            mapper,
        );

        assert_eq!(
            tracker
                .handle_exception(0, reprotected_write_fault(0x9000))
                .expect("unowned permission fault should remain unhandled"),
            None
        );
        let handled = tracker
            .handle_exception(0, reprotected_write_fault(0x5fff))
            .expect("owned reprotected write should succeed")
            .expect("owned reprotected write should be handled");
        assert_eq!(handled.page(), GuestAddress::new(0x5000));
        assert!(handled.first_write());
        assert_eq!(
            tracker.dirty_pages().expect("permission fault should mark"),
            vec![GuestAddress::new(0x5000)]
        );
        tracker.stop().expect("tracker should stop");
    }

    #[test]
    fn concurrent_same_page_first_writes_unprotect_once_and_bound_stale_retries() {
        let mapper = Arc::new(RecordingMapper::default());
        let tracker = start_tracker(
            &[mapped_region(0x8000, 1, HvfMemoryPermissions::GUEST_RAM)],
            mapper.clone(),
        );
        tracker
            .register_owner(0)
            .expect("first owner should register");
        tracker
            .register_owner(1)
            .expect("second owner should register");
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for member_index in 0..2 {
            let tracker = tracker.clone();
            let barrier = barrier.clone();
            threads.push(thread::spawn(move || {
                barrier.wait();
                tracker
                    .handle_exception(member_index, tracked_write_fault(0x8fff))
                    .expect("concurrent fault should be handled")
                    .expect("owned fault should be admitted")
            }));
        }
        barrier.wait();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("fault thread should not panic"))
            .collect();

        assert_eq!(
            results.iter().filter(|fault| fault.first_write()).count(),
            1
        );
        assert_eq!(
            mapper
                .calls()
                .iter()
                .filter(|call| call.permissions.contains(HvfMemoryPermissions::WRITE))
                .count(),
            1
        );
        assert_eq!(
            tracker.handle_exception(0, tracked_write_fault(0x8000)),
            Err(HvfDirtyWriteFaultError::NoProgress)
        );
        assert_eq!(
            tracker.handle_exception(1, tracked_write_fault(0x8000)),
            Err(HvfDirtyWriteFaultError::NoProgress)
        );
        tracker.unregister_owner();
        tracker.unregister_owner();
        tracker.stop().expect("tracker should stop");
    }

    #[test]
    fn unprotect_failure_marks_no_page_and_requires_cleanup_before_reuse() {
        let mapper = Arc::new(RecordingMapper::failing_on(&[1]));
        let tracker = start_tracker(
            &[mapped_region(0xa000, 1, HvfMemoryPermissions::GUEST_RAM)],
            mapper.clone(),
        );

        assert!(matches!(
            tracker.handle_exception(0, tracked_write_fault(0xa000)),
            Err(HvfDirtyWriteFaultError::UnprotectFailed(_))
        ));
        assert_eq!(
            tracker
                .state
                .lock()
                .expect("tracker state should be available")
                .regions[0]
                .restored_write_words,
            vec![0]
        );
        assert!(tracker.dirty_pages().is_err());
        assert!(tracker.is_active().is_err());
        assert!(matches!(
            tracker.register_owner(0),
            Err(HvfDirtyWriteFaultError::InvalidState(_))
        ));
        tracker.stop().expect("cleanup retry should succeed");
        assert!(!tracker.is_active().expect("query should succeed"));
        assert_eq!(mapper.calls().len(), 3);
    }

    #[test]
    fn userspace_dirty_page_remains_protected_until_its_first_guest_write() {
        let mapper = Arc::new(RecordingMapper::default());
        let tracker = start_tracker(
            &[mapped_region(0xc000, 1, HvfMemoryPermissions::GUEST_RAM)],
            mapper.clone(),
        );
        let dirty_tracker = Arc::clone(
            &tracker
                .state
                .lock()
                .expect("tracker state should lock")
                .dirty_tracker,
        );
        dirty_tracker
            .mark_range(range(0xc000, 1))
            .expect("userspace mark should succeed");

        let fault = tracker
            .handle_exception(0, tracked_write_fault(0xc000))
            .expect("protected userspace-dirty page should handle")
            .expect("fault should belong to tracker");

        assert!(fault.first_write());
        assert_eq!(
            tracker.dirty_pages().expect("query should succeed").len(),
            1
        );
        assert_eq!(mapper.calls().len(), 2);
        tracker.stop().expect("tracker should stop");
    }

    #[test]
    fn committed_reset_reprotects_cpu_pages_and_clears_cpu_userspace_union() {
        let mapper = Arc::new(RecordingMapper::default());
        let tracker = start_tracker(
            &[mapped_region(0x30_000, 3, HvfMemoryPermissions::GUEST_RAM)],
            mapper.clone(),
        );
        tracker
            .handle_exception(0, tracked_write_fault(0x30_000))
            .expect("CPU page should handle")
            .expect("CPU page should belong to tracker");
        let dirty_tracker = Arc::clone(
            &tracker
                .state
                .lock()
                .expect("tracker state should lock")
                .dirty_tracker,
        );
        dirty_tracker
            .mark_range(range(0x32_000, PAGE_SIZE))
            .expect("userspace page should mark");
        assert_eq!(
            tracker.dirty_pages().expect("union should query"),
            [GuestAddress::new(0x30_000), GuestAddress::new(0x32_000)]
        );

        assert_eq!(tracker.reset_epoch_quiesced(), Ok(1));
        assert!(
            tracker
                .dirty_pages()
                .expect("new epoch should query")
                .is_empty()
        );
        assert_eq!(
            mapper.calls().last(),
            Some(&ProtectionCall {
                range: range(0x30_000, PAGE_SIZE),
                permissions: HvfMemoryPermissions::new(true, false, true),
            })
        );

        tracker
            .handle_exception(0, tracked_write_fault(0x31_000))
            .expect("second epoch CPU page should handle")
            .expect("second epoch CPU page should belong to tracker");
        assert_eq!(
            tracker.dirty_pages().expect("second epoch should query"),
            vec![GuestAddress::new(0x31_000)]
        );
        assert_eq!(tracker.reset_epoch_quiesced(), Ok(2));
        tracker.stop().expect("tracker should stop");
    }

    #[test]
    fn failed_reset_rolls_back_or_poisons_without_clearing_the_epoch() {
        for (failures, requires_teardown) in [(&[4][..], false), (&[4, 5][..], true)] {
            let mapper = Arc::new(RecordingMapper::failing_on(failures));
            let tracker = start_tracker(
                &[mapped_region(0x40_000, 3, HvfMemoryPermissions::GUEST_RAM)],
                mapper,
            );
            tracker
                .handle_exception(0, tracked_write_fault(0x40_000))
                .expect("first CPU page should handle")
                .expect("first CPU page should belong to tracker");
            tracker
                .handle_exception(0, tracked_write_fault(0x42_000))
                .expect("second CPU page should handle")
                .expect("second CPU page should belong to tracker");

            let error = tracker
                .reset_epoch_quiesced()
                .expect_err("injected reset should fail");
            assert_eq!(error.requires_vm_teardown(), requires_teardown);
            assert!(matches!(
                error,
                HvfDirtyWriteEpochResetError::ProtectionFailed { .. }
            ));
            if requires_teardown {
                assert!(tracker.dirty_pages().is_err());
            } else {
                assert_eq!(
                    tracker.dirty_pages().expect("old epoch should remain"),
                    [GuestAddress::new(0x40_000), GuestAddress::new(0x42_000)]
                );
                assert_eq!(tracker.reset_epoch_quiesced(), Ok(1));
                tracker.stop().expect("recovered tracker should stop");
            }
        }
    }

    #[test]
    fn invalid_reset_state_requires_teardown_but_preflight_allocation_does_not() {
        assert!(
            HvfDirtyWriteEpochResetError::InvalidState("injected invariant failure")
                .requires_vm_teardown()
        );
        assert!(!HvfDirtyWriteEpochResetError::AllocationFailed.requires_vm_teardown());
    }

    #[test]
    fn stop_waits_for_owners_and_restores_only_contiguous_clean_runs() {
        let mapper = Arc::new(RecordingMapper::default());
        let tracker = start_tracker(
            &[mapped_region(0x10_000, 5, HvfMemoryPermissions::GUEST_RAM)],
            mapper.clone(),
        );
        tracker.register_owner(0).expect("owner should register");
        tracker
            .handle_exception(0, tracked_write_fault(0x11_000))
            .expect("first fault should succeed")
            .expect("first fault should be handled");
        tracker
            .handle_exception(0, tracked_write_fault(0x13_000))
            .expect("second fault should succeed")
            .expect("second fault should be handled");

        assert_eq!(
            tracker.stop(),
            Err(HvfDirtyWriteTrackerStopError::OwnersActive { count: 1 })
        );
        tracker.unregister_owner();
        tracker.stop().expect("stop should restore clean runs");
        tracker.stop().expect("stop should be idempotent");
        assert_eq!(
            tracker
                .dirty_pages()
                .expect("stopped tracker should retain its result"),
            vec![GuestAddress::new(0x11_000), GuestAddress::new(0x13_000)]
        );

        assert_eq!(
            &mapper.calls()[3..],
            &[
                ProtectionCall {
                    range: range(0x10_000, PAGE_SIZE),
                    permissions: HvfMemoryPermissions::GUEST_RAM,
                },
                ProtectionCall {
                    range: range(0x12_000, PAGE_SIZE),
                    permissions: HvfMemoryPermissions::GUEST_RAM,
                },
                ProtectionCall {
                    range: range(0x14_000, PAGE_SIZE),
                    permissions: HvfMemoryPermissions::GUEST_RAM,
                },
            ]
        );
    }

    #[test]
    fn failed_stop_is_typed_and_retryable() {
        let mapper = Arc::new(RecordingMapper::failing_on(&[1]));
        let tracker = start_tracker(
            &[mapped_region(0x20_000, 2, HvfMemoryPermissions::GUEST_RAM)],
            mapper.clone(),
        );

        let error = tracker.stop().expect_err("first stop should fail");
        let HvfDirtyWriteTrackerStopError::ProtectionFailed {
            failures,
            omitted_failures,
        } = error
        else {
            panic!("expected stop protection failure");
        };
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].operation_index(), 0);
        assert_eq!(omitted_failures, 0);
        assert!(tracker.is_active().is_err());

        tracker.stop().expect("second stop should retry cleanup");
        assert!(!tracker.is_active().expect("query should succeed"));
        assert_eq!(mapper.calls().len(), 3);
    }
}
