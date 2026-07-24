//! HVF stage-two fault mediation for coordinated lazy guest memory.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

use bangbang_pager::PageAccess;
use bangbang_runtime::BackendError;
use bangbang_runtime::machine::MAX_SUPPORTED_VCPUS;
use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange};

use crate::exit::{
    HvfExceptionExit, HvfLazyGuestAccess, HvfLazyGuestFault, HvfLazyGuestFaultCandidate,
};
use crate::lazy_host_fault::{
    HvfLazyHostFaultError, HvfLazyPageResolution, HvfLazyPageResolutionLease, HvfLazyPageResolver,
};
use crate::memory::{
    HvfGuestMemoryMappingError, HvfMemoryMapper, HvfMemoryPermissions, host_page_size,
};

const HANDLER_NOT_ACTIVE_MESSAGE: &str = "lazy guest fault handler is not active";
const HANDLER_POISONED_MESSAGE: &str = "lazy guest fault handler is poisoned";
const HANDLER_LOCK_POISONED_MESSAGE: &str = "lazy guest fault handler state is unavailable";
const WRITE_WITHOUT_READ_MESSAGE: &str =
    "lazy writable guest memory requires both read and write permissions";

/// Stable category for one shared resolver failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfLazyGuestResolutionFailure {
    /// The resolver is unavailable on this compile target.
    UnsupportedTarget,
    /// Retained resolver or mapping metadata was inconsistent.
    InvalidConfiguration,
    /// Bounded resolver metadata could not be allocated.
    MetadataAllocation,
    /// A public platform operation failed.
    Platform,
    /// The lazy-memory coordinator rejected the operation.
    Coordinator,
    /// The trusted page source failed.
    Source,
    /// The source returned a page with the wrong length.
    ContentLength,
    /// Resolver admission was no longer active.
    InvalidLifecycle,
}

impl From<&HvfLazyHostFaultError> for HvfLazyGuestResolutionFailure {
    fn from(error: &HvfLazyHostFaultError) -> Self {
        match error {
            HvfLazyHostFaultError::UnsupportedTarget => Self::UnsupportedTarget,
            HvfLazyHostFaultError::InvalidConfiguration { .. } => Self::InvalidConfiguration,
            HvfLazyHostFaultError::MetadataAllocationFailed { .. } => Self::MetadataAllocation,
            HvfLazyHostFaultError::Platform { .. } => Self::Platform,
            HvfLazyHostFaultError::Coordinator { .. } => Self::Coordinator,
            HvfLazyHostFaultError::Source { .. } => Self::Source,
            HvfLazyHostFaultError::ContentLength => Self::ContentLength,
            HvfLazyHostFaultError::InvalidLifecycle => Self::InvalidLifecycle,
        }
    }
}

/// Failure while resolving an owned HVF lazy-memory exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfLazyGuestFaultError {
    /// The guest fault handler was unavailable for the requested operation.
    InvalidState(&'static str),
    /// The topology member identity exceeded the supported bounded vector.
    InvalidMemberIndex {
        /// Rejected member index.
        index: usize,
    },
    /// Shared page resolution failed before guest permission publication.
    Resolution {
        /// Stable value-free failure category.
        failure: HvfLazyGuestResolutionFailure,
    },
    /// HVF rejected a stage-two permission update.
    Protection {
        /// Coordinator-aligned page whose permission update failed.
        page: GuestAddress,
        /// Backend failure returned by the platform mapper.
        source: BackendError,
    },
    /// The same vCPU repeated an unchanged already-satisfied exit.
    NoProgress {
        /// Coordinator-aligned page repeated by the vCPU.
        page: GuestAddress,
        /// Repeated access class.
        access: HvfLazyGuestAccess,
    },
}

impl fmt::Display for HvfLazyGuestFaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidState(message) => {
                write!(formatter, "invalid lazy guest fault state: {message}")
            }
            Self::InvalidMemberIndex { index } => write!(
                formatter,
                "lazy guest fault member index {index} exceeds the supported topology"
            ),
            Self::Resolution { failure } => {
                write!(formatter, "lazy guest page resolution failed: {failure:?}")
            }
            Self::Protection { page, source } => {
                write!(
                    formatter,
                    "failed to publish lazy guest page {page} permissions: {source}"
                )
            }
            Self::NoProgress { page, access } => write!(
                formatter,
                "lazy guest {access:?} retry made no progress for vCPU page {page}"
            ),
        }
    }
}

impl std::error::Error for HvfLazyGuestFaultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Protection { source, .. } => Some(source),
            Self::InvalidState(_)
            | Self::InvalidMemberIndex { .. }
            | Self::Resolution { .. }
            | Self::NoProgress { .. } => None,
        }
    }
}

/// Result of one handled lazy guest-memory access exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfHandledLazyGuestFault {
    fault: HvfLazyGuestFault,
    populated_pages: usize,
    permission_changes: usize,
    stale_exit: bool,
}

impl HvfHandledLazyGuestFault {
    /// Return the validated fault metadata.
    pub const fn fault(self) -> HvfLazyGuestFault {
        self.fault
    }

    /// Return how many coordinator pages this exit populated.
    pub const fn populated_pages(self) -> usize {
        self.populated_pages
    }

    /// Return how many stage-two page permissions this exit changed.
    pub const fn permission_changes(self) -> usize {
        self.permission_changes
    }

    /// Return whether a peer had already satisfied this admitted exit.
    pub const fn stale_exit(self) -> bool {
        self.stale_exit
    }
}

trait LazyGuestPageResolver: fmt::Debug + Send + Sync {
    fn resolve(
        &self,
        addresses: &[GuestAddress],
        access: PageAccess,
    ) -> Result<HvfLazyPageResolutionLease, HvfLazyGuestResolutionFailure>;

    fn synchronize_instruction(
        &self,
        address: GuestAddress,
        lease: &HvfLazyPageResolutionLease,
    ) -> Result<(), HvfLazyGuestResolutionFailure>;

    fn fail_closed(&self);
}

#[cfg(test)]
#[derive(Debug)]
struct NoopLazyGuestPageResolver;

#[cfg(test)]
impl LazyGuestPageResolver for NoopLazyGuestPageResolver {
    fn resolve(
        &self,
        addresses: &[GuestAddress],
        _access: PageAccess,
    ) -> Result<HvfLazyPageResolutionLease, HvfLazyGuestResolutionFailure> {
        Ok(HvfLazyPageResolutionLease::untracked(vec![
            HvfLazyPageResolution::Present;
            addresses.len()
        ]))
    }

    fn fail_closed(&self) {}

    fn synchronize_instruction(
        &self,
        _address: GuestAddress,
        _lease: &HvfLazyPageResolutionLease,
    ) -> Result<(), HvfLazyGuestResolutionFailure> {
        Ok(())
    }
}

impl LazyGuestPageResolver for HvfLazyPageResolver {
    fn resolve(
        &self,
        addresses: &[GuestAddress],
        access: PageAccess,
    ) -> Result<HvfLazyPageResolutionLease, HvfLazyGuestResolutionFailure> {
        self.resolve_guest_pages_leased(addresses, access)
            .map_err(|error| HvfLazyGuestResolutionFailure::from(&error))
    }

    fn fail_closed(&self) {
        HvfLazyPageResolver::fail_closed(self);
    }

    fn synchronize_instruction(
        &self,
        address: GuestAddress,
        lease: &HvfLazyPageResolutionLease,
    ) -> Result<(), HvfLazyGuestResolutionFailure> {
        self.synchronize_instruction_page(address, lease)
            .map_err(|error| HvfLazyGuestResolutionFailure::from(&error))
    }
}

#[derive(Debug, Clone, Copy)]
struct LazyGuestRegion {
    range: GuestMemoryRange,
    first_page: usize,
    page_count: usize,
    maximum_permissions: HvfMemoryPermissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FaultAdmission {
    range: GuestMemoryRange,
    access: HvfLazyGuestAccess,
    page: GuestAddress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandlerStatus {
    Prepared,
    Active,
    Poisoned,
}

#[derive(Debug)]
struct HandlerState {
    status: HandlerStatus,
    page_permissions: Vec<HvfMemoryPermissions>,
    last_admitted: [Option<FaultAdmission>; MAX_SUPPORTED_VCPUS as usize],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HvfOwnedLazyGuestFaultCandidate {
    candidate: HvfLazyGuestFaultCandidate,
    region_index: usize,
}

/// Shared stage-two permission owner retained by every vCPU runner.
pub(crate) struct HvfLazyGuestFaultHandler {
    resolver: Arc<dyn LazyGuestPageResolver>,
    mapper: Arc<dyn HvfMemoryMapper>,
    regions: Vec<LazyGuestRegion>,
    page_size: u64,
    state: Mutex<HandlerState>,
}

impl HvfLazyGuestFaultHandler {
    pub(crate) fn prepare(
        resolver: HvfLazyPageResolver,
        maximum_permissions: HvfMemoryPermissions,
        mapper: Arc<dyn HvfMemoryMapper>,
    ) -> Result<Arc<Self>, HvfGuestMemoryMappingError> {
        let mapping_regions = resolver.mapping_regions();
        let page_size = u64::from(resolver.page_size());
        let host_page_size = host_page_size()?;
        if mapping_regions.is_empty()
            || page_size < host_page_size
            || !page_size.is_multiple_of(host_page_size)
        {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                "lazy guest memory page size or regions are invalid",
            ));
        }
        if maximum_permissions.is_empty() {
            return Err(HvfGuestMemoryMappingError::EmptyPermissions);
        }
        if maximum_permissions.contains(HvfMemoryPermissions::WRITE)
            && !maximum_permissions.contains(HvfMemoryPermissions::READ)
        {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                WRITE_WITHOUT_READ_MESSAGE,
            ));
        }

        let mut regions = Vec::new();
        regions
            .try_reserve_exact(mapping_regions.len())
            .map_err(
                |source| HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source },
            )?;
        let mut total_pages = 0_usize;
        for mapping_region in mapping_regions {
            let range = mapping_region.range();
            if range.validate_alignment(page_size).is_err() {
                return Err(HvfGuestMemoryMappingError::InvalidState(
                    "lazy guest memory region is not coordinator-page aligned",
                ));
            }
            let page_count = usize::try_from(range.size() / page_size).map_err(|_| {
                HvfGuestMemoryMappingError::InvalidState(
                    "lazy guest memory page count exceeds this host",
                )
            })?;
            if page_count == 0 {
                return Err(HvfGuestMemoryMappingError::InvalidState(
                    "lazy guest memory region contains no pages",
                ));
            }
            let next_total = total_pages.checked_add(page_count).ok_or(
                HvfGuestMemoryMappingError::InvalidState(
                    "lazy guest memory page count exceeds this host",
                ),
            )?;
            regions.push(LazyGuestRegion {
                range,
                first_page: total_pages,
                page_count,
                maximum_permissions,
            });
            total_pages = next_total;
        }

        let mut page_permissions = Vec::new();
        page_permissions
            .try_reserve_exact(total_pages)
            .map_err(
                |source| HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source },
            )?;
        page_permissions.resize(total_pages, HvfMemoryPermissions::new(false, false, false));

        Ok(Arc::new(Self {
            resolver: Arc::new(resolver),
            mapper,
            regions,
            page_size,
            state: Mutex::new(HandlerState {
                status: HandlerStatus::Prepared,
                page_permissions,
                last_admitted: [None; MAX_SUPPORTED_VCPUS as usize],
            }),
        }))
    }

    pub(crate) fn activate(&self) -> Result<(), HvfGuestMemoryMappingError> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                self.resolver.fail_closed();
                return Err(HvfGuestMemoryMappingError::InvalidState(
                    HANDLER_LOCK_POISONED_MESSAGE,
                ));
            }
        };
        if state.status != HandlerStatus::Prepared {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                "lazy guest fault handler activation is invalid",
            ));
        }
        state.status = HandlerStatus::Active;
        Ok(())
    }

    pub(crate) fn poison(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.status = HandlerStatus::Poisoned;
        }
        self.resolver.fail_closed();
    }

    pub(crate) fn ensure_active(&self) -> Result<(), BackendError> {
        let state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                self.resolver.fail_closed();
                return Err(BackendError::InvalidState(HANDLER_LOCK_POISONED_MESSAGE));
            }
        };
        match state.status {
            HandlerStatus::Active => Ok(()),
            HandlerStatus::Prepared => Err(BackendError::InvalidState(HANDLER_NOT_ACTIVE_MESSAGE)),
            HandlerStatus::Poisoned => Err(BackendError::InvalidState(HANDLER_POISONED_MESSAGE)),
        }
    }

    pub(crate) fn classify(
        &self,
        exit: HvfExceptionExit,
    ) -> Result<Option<HvfOwnedLazyGuestFaultCandidate>, HvfLazyGuestFaultError> {
        let Some(candidate) = exit.decode_lazy_guest_fault_candidate() else {
            return Ok(None);
        };
        let Some((region_index, _, _)) = self.owned_page_span(candidate) else {
            return Ok(None);
        };
        self.ensure_fault_active()?;
        Ok(Some(HvfOwnedLazyGuestFaultCandidate {
            candidate,
            region_index,
        }))
    }

    pub(crate) fn handle(
        &self,
        member_index: usize,
        owned: HvfOwnedLazyGuestFaultCandidate,
        pc: u64,
    ) -> Result<Option<HvfHandledLazyGuestFault>, HvfLazyGuestFaultError> {
        if member_index >= usize::from(MAX_SUPPORTED_VCPUS) {
            return Err(HvfLazyGuestFaultError::InvalidMemberIndex {
                index: member_index,
            });
        }
        let Some(fault) = owned.candidate.validate_pc(pc) else {
            return Ok(None);
        };
        let region = self.regions.get(owned.region_index).copied().ok_or(
            HvfLazyGuestFaultError::InvalidState("owned lazy guest region disappeared"),
        )?;
        let (_, first_page, last_page) =
            self.page_span(region, fault.range())
                .ok_or(HvfLazyGuestFaultError::InvalidState(
                    "owned lazy guest page span is invalid",
                ))?;
        let admission = FaultAdmission {
            range: fault.range(),
            access: fault.access(),
            page: region_page_address(region, self.page_size, first_page)?,
        };
        let required_permissions = required_permissions(fault.access());

        let resolver_access = match fault.access() {
            HvfLazyGuestAccess::Read | HvfLazyGuestAccess::Execute => PageAccess::Read,
            HvfLazyGuestAccess::Write => PageAccess::Write,
        };
        let mut pages = Vec::new();
        pages
            .try_reserve_exact(last_page - first_page + 1)
            .map_err(|_| {
                HvfLazyGuestFaultError::InvalidState(
                    "lazy guest resolution page metadata allocation failed",
                )
            })?;
        for page_index in first_page..=last_page {
            pages.push(region_page_address(region, self.page_size, page_index)?);
        }
        let resolution_lease = match self.resolver.resolve(&pages, resolver_access) {
            Ok(lease) => lease,
            Err(failure) => {
                self.poison_after_failure();
                return Err(HvfLazyGuestFaultError::Resolution { failure });
            }
        };
        if resolution_lease.resolutions().len() != pages.len() {
            self.poison_after_failure();
            return Err(HvfLazyGuestFaultError::InvalidState(
                "lazy guest resolver returned incomplete page metadata",
            ));
        }
        let populated_pages = resolution_lease
            .resolutions()
            .iter()
            .filter(|resolution| **resolution == HvfLazyPageResolution::Populated)
            .count();
        if fault.access() == HvfLazyGuestAccess::Execute {
            for page in &pages {
                if let Err(failure) = self
                    .resolver
                    .synchronize_instruction(*page, &resolution_lease)
                {
                    self.poison_after_failure();
                    return Err(HvfLazyGuestFaultError::Resolution { failure });
                }
            }
        }

        let mut state = self.lock_state()?;
        state.ensure_active()?;
        let mut permission_changes = 0_usize;
        for page_index in first_page..=last_page {
            let global_page = region.first_page.checked_add(page_index).ok_or(
                HvfLazyGuestFaultError::InvalidState("lazy guest page index overflowed"),
            )?;
            let current = state.page_permissions.get(global_page).copied().ok_or(
                HvfLazyGuestFaultError::InvalidState("lazy guest permission page disappeared"),
            )?;
            let next = current.union(required_permissions);
            if next == current {
                continue;
            }
            let page = region_page_address(region, self.page_size, page_index)?;
            let page_range = GuestMemoryRange::new(page, self.page_size).map_err(|_| {
                HvfLazyGuestFaultError::InvalidState("lazy guest page range is invalid")
            })?;
            if let Err(source) = self.mapper.protect_region(page_range, next) {
                state.status = HandlerStatus::Poisoned;
                drop(state);
                self.resolver.fail_closed();
                return Err(HvfLazyGuestFaultError::Protection { page, source });
            }
            let permissions = state.page_permissions.get_mut(global_page).ok_or(
                HvfLazyGuestFaultError::InvalidState("lazy guest permission page disappeared"),
            )?;
            *permissions = next;
            permission_changes += 1;
        }
        drop(resolution_lease);

        let stale_exit = permission_changes == 0;
        if stale_exit {
            admit_stale_or_fail(&mut state, member_index, admission)?;
        } else {
            let last = state.last_admitted.get_mut(member_index).ok_or(
                HvfLazyGuestFaultError::InvalidMemberIndex {
                    index: member_index,
                },
            )?;
            *last = Some(admission);
        }
        Ok(Some(HvfHandledLazyGuestFault {
            fault,
            populated_pages,
            permission_changes,
            stale_exit,
        }))
    }

    pub(crate) fn revoke(&self, range: GuestMemoryRange) -> Result<(), HvfLazyGuestFaultError> {
        if range.validate_alignment(self.page_size).is_err() {
            return Err(HvfLazyGuestFaultError::InvalidState(
                "lazy guest revocation range is not coordinator-page aligned",
            ));
        }
        let (region, first_page, last_page) = self
            .regions
            .iter()
            .copied()
            .find_map(|region| {
                self.page_span(region, range)
                    .map(|(_, first, last)| (region, first, last))
            })
            .ok_or(HvfLazyGuestFaultError::InvalidState(
                "lazy guest revocation range is not owned",
            ))?;
        let mut state = self.lock_state()?;
        state.ensure_active()?;
        let empty = HvfMemoryPermissions::new(false, false, false);
        if let Err(source) = self.mapper.protect_region(range, empty) {
            state.status = HandlerStatus::Poisoned;
            drop(state);
            self.resolver.fail_closed();
            return Err(HvfLazyGuestFaultError::Protection {
                page: range.start(),
                source,
            });
        }
        for page_index in first_page..=last_page {
            let global_page = region.first_page.checked_add(page_index).ok_or(
                HvfLazyGuestFaultError::InvalidState("lazy guest page index overflowed"),
            )?;
            let permissions = state.page_permissions.get_mut(global_page).ok_or(
                HvfLazyGuestFaultError::InvalidState("lazy guest permission page disappeared"),
            )?;
            *permissions = empty;
        }
        for last in &mut state.last_admitted {
            if last.is_some_and(|admission| admission.range.overlaps(range)) {
                *last = None;
            }
        }
        Ok(())
    }

    fn owned_page_span(
        &self,
        candidate: HvfLazyGuestFaultCandidate,
    ) -> Option<(usize, usize, usize)> {
        let required = required_permissions(candidate.access());
        self.regions
            .iter()
            .copied()
            .enumerate()
            .find_map(|(region_index, region)| {
                if !region.maximum_permissions.contains(required) {
                    return None;
                }
                self.page_span(region, candidate.range())
                    .map(|(_, first, last)| (region_index, first, last))
            })
    }

    fn page_span(
        &self,
        region: LazyGuestRegion,
        range: GuestMemoryRange,
    ) -> Option<(usize, usize, usize)> {
        if region.range.start().raw_value() > range.start().raw_value()
            || range.end_exclusive().raw_value() > region.range.end_exclusive().raw_value()
        {
            return None;
        }
        let first_offset = range
            .start()
            .raw_value()
            .checked_sub(region.range.start().raw_value())?;
        let last_address = range.end_exclusive().raw_value().checked_sub(1)?;
        let last_offset = last_address.checked_sub(region.range.start().raw_value())?;
        let first = usize::try_from(first_offset / self.page_size).ok()?;
        let last = usize::try_from(last_offset / self.page_size).ok()?;
        if first > last || last >= region.page_count {
            return None;
        }
        Some((region.first_page, first, last))
    }

    fn ensure_fault_active(&self) -> Result<(), HvfLazyGuestFaultError> {
        let state = self.lock_state()?;
        state.ensure_active()
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, HandlerState>, HvfLazyGuestFaultError> {
        match self.state.lock() {
            Ok(state) => Ok(state),
            Err(_) => {
                self.resolver.fail_closed();
                Err(HvfLazyGuestFaultError::InvalidState(
                    HANDLER_LOCK_POISONED_MESSAGE,
                ))
            }
        }
    }

    fn poison_after_failure(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.status = HandlerStatus::Poisoned;
        }
        self.resolver.fail_closed();
    }

    #[cfg(test)]
    fn active_for_test(
        resolver: Arc<dyn LazyGuestPageResolver>,
        mapper: Arc<dyn HvfMemoryMapper>,
        ranges: &[GuestMemoryRange],
        page_size: u64,
        maximum_permissions: HvfMemoryPermissions,
    ) -> Arc<Self> {
        let mut regions = Vec::with_capacity(ranges.len());
        let mut total_pages = 0_usize;
        for range in ranges {
            let page_count =
                usize::try_from(range.size() / page_size).expect("test page count should fit");
            regions.push(LazyGuestRegion {
                range: *range,
                first_page: total_pages,
                page_count,
                maximum_permissions,
            });
            total_pages += page_count;
        }
        Arc::new(Self {
            resolver,
            mapper,
            regions,
            page_size,
            state: Mutex::new(HandlerState {
                status: HandlerStatus::Active,
                page_permissions: vec![HvfMemoryPermissions::new(false, false, false); total_pages],
                last_admitted: [None; MAX_SUPPORTED_VCPUS as usize],
            }),
        })
    }

    #[cfg(test)]
    pub(crate) fn active_noop_for_test(
        mapper: Arc<dyn HvfMemoryMapper>,
        ranges: &[GuestMemoryRange],
        page_size: u64,
        maximum_permissions: HvfMemoryPermissions,
    ) -> Arc<Self> {
        Self::active_for_test(
            Arc::new(NoopLazyGuestPageResolver),
            mapper,
            ranges,
            page_size,
            maximum_permissions,
        )
    }
}

impl fmt::Debug for HvfLazyGuestFaultHandler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HvfLazyGuestFaultHandler(<redacted>)")
    }
}

impl HandlerState {
    fn ensure_active(&self) -> Result<(), HvfLazyGuestFaultError> {
        match self.status {
            HandlerStatus::Active => Ok(()),
            HandlerStatus::Prepared => Err(HvfLazyGuestFaultError::InvalidState(
                HANDLER_NOT_ACTIVE_MESSAGE,
            )),
            HandlerStatus::Poisoned => Err(HvfLazyGuestFaultError::InvalidState(
                HANDLER_POISONED_MESSAGE,
            )),
        }
    }
}

fn admit_stale_or_fail(
    state: &mut HandlerState,
    member_index: usize,
    admission: FaultAdmission,
) -> Result<(), HvfLazyGuestFaultError> {
    let last = state.last_admitted.get_mut(member_index).ok_or(
        HvfLazyGuestFaultError::InvalidMemberIndex {
            index: member_index,
        },
    )?;
    if *last == Some(admission) {
        return Err(HvfLazyGuestFaultError::NoProgress {
            page: admission.page,
            access: admission.access,
        });
    }
    *last = Some(admission);
    Ok(())
}

fn region_page_address(
    region: LazyGuestRegion,
    page_size: u64,
    page_index: usize,
) -> Result<GuestAddress, HvfLazyGuestFaultError> {
    let page_index = u64::try_from(page_index)
        .map_err(|_| HvfLazyGuestFaultError::InvalidState("lazy guest page index exceeds u64"))?;
    let offset = page_index
        .checked_mul(page_size)
        .ok_or(HvfLazyGuestFaultError::InvalidState(
            "lazy guest page offset overflowed",
        ))?;
    region
        .range
        .start()
        .checked_add(offset)
        .ok_or(HvfLazyGuestFaultError::InvalidState(
            "lazy guest page address overflowed",
        ))
}

const fn required_permissions(access: HvfLazyGuestAccess) -> HvfMemoryPermissions {
    match access {
        HvfLazyGuestAccess::Read => HvfMemoryPermissions::READ,
        HvfLazyGuestAccess::Write => HvfMemoryPermissions::READ.union(HvfMemoryPermissions::WRITE),
        HvfLazyGuestAccess::Execute => HvfMemoryPermissions::EXECUTE,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::memory::{HvfMappedGuestMemoryRegion, HvfMemoryMapRequest};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Event {
        Resolve {
            page: GuestAddress,
            access: PageAccess,
        },
        Protect {
            range: GuestMemoryRange,
            permissions: HvfMemoryPermissions,
        },
        Synchronize {
            page: GuestAddress,
        },
    }

    #[derive(Debug)]
    struct RecordingResolver {
        events: Arc<Mutex<Vec<Event>>>,
        fail: AtomicBool,
        populated: Mutex<Vec<GuestAddress>>,
        fail_closed: AtomicUsize,
    }

    impl RecordingResolver {
        fn new(events: Arc<Mutex<Vec<Event>>>) -> Self {
            Self {
                events,
                fail: AtomicBool::new(false),
                populated: Mutex::new(Vec::new()),
                fail_closed: AtomicUsize::new(0),
            }
        }

        fn fail_next(&self) {
            self.fail.store(true, Ordering::Release);
        }
    }

    impl LazyGuestPageResolver for RecordingResolver {
        fn resolve(
            &self,
            addresses: &[GuestAddress],
            access: PageAccess,
        ) -> Result<HvfLazyPageResolutionLease, HvfLazyGuestResolutionFailure> {
            if self.fail.swap(false, Ordering::AcqRel) {
                if let Some(address) = addresses.first().copied() {
                    self.events
                        .lock()
                        .expect("event log should lock")
                        .push(Event::Resolve {
                            page: address,
                            access,
                        });
                }
                return Err(HvfLazyGuestResolutionFailure::Source);
            }
            let mut populated = self.populated.lock().expect("page set should lock");
            let mut resolutions = Vec::with_capacity(addresses.len());
            for address in addresses {
                self.events
                    .lock()
                    .expect("event log should lock")
                    .push(Event::Resolve {
                        page: *address,
                        access,
                    });
                if populated.contains(address) {
                    resolutions.push(HvfLazyPageResolution::Present);
                } else {
                    populated.push(*address);
                    resolutions.push(HvfLazyPageResolution::Populated);
                }
            }
            Ok(HvfLazyPageResolutionLease::untracked(resolutions))
        }

        fn fail_closed(&self) {
            self.fail_closed.fetch_add(1, Ordering::Relaxed);
        }

        fn synchronize_instruction(
            &self,
            address: GuestAddress,
            _lease: &HvfLazyPageResolutionLease,
        ) -> Result<(), HvfLazyGuestResolutionFailure> {
            self.events
                .lock()
                .expect("event log should lock")
                .push(Event::Synchronize { page: address });
            Ok(())
        }
    }

    #[derive(Debug)]
    struct RecordingMapper {
        events: Arc<Mutex<Vec<Event>>>,
        fail_protect: AtomicBool,
    }

    impl RecordingMapper {
        fn new(events: Arc<Mutex<Vec<Event>>>) -> Self {
            Self {
                events,
                fail_protect: AtomicBool::new(false),
            }
        }

        fn fail_next_protect(&self) {
            self.fail_protect.store(true, Ordering::Release);
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
            self.events
                .lock()
                .expect("event log should lock")
                .push(Event::Protect { range, permissions });
            if self.fail_protect.swap(false, Ordering::AcqRel) {
                Err(BackendError::Hypervisor(
                    "injected stage-two protect failure".to_string(),
                ))
            } else {
                Ok(())
            }
        }
    }

    fn range(start: u64, size: u64) -> GuestMemoryRange {
        GuestMemoryRange::new(GuestAddress::new(start), size).expect("test range should be valid")
    }

    fn data_exit(address: u64, write: bool, doubleword: bool) -> HvfExceptionExit {
        let mut syndrome = if doubleword { 0x93c1_8007 } else { 0x9381_0007 };
        if write {
            syndrome |= 1 << 6;
        }
        HvfExceptionExit {
            syndrome,
            virtual_address: address,
            physical_address: address,
        }
    }

    fn instruction_exit(virtual_address: u64, physical_address: u64) -> HvfExceptionExit {
        HvfExceptionExit {
            syndrome: 0x8200_0006,
            virtual_address,
            physical_address,
        }
    }

    type TestHandler = (
        Arc<HvfLazyGuestFaultHandler>,
        Arc<RecordingResolver>,
        Arc<RecordingMapper>,
        Arc<Mutex<Vec<Event>>>,
    );

    fn test_handler(ranges: &[GuestMemoryRange], page_size: u64) -> TestHandler {
        let events = Arc::new(Mutex::new(Vec::new()));
        let resolver = Arc::new(RecordingResolver::new(Arc::clone(&events)));
        let mapper = Arc::new(RecordingMapper::new(Arc::clone(&events)));
        let handler = HvfLazyGuestFaultHandler::active_for_test(
            resolver.clone(),
            mapper.clone(),
            ranges,
            page_size,
            HvfMemoryPermissions::GUEST_RAM,
        );
        (handler, resolver, mapper, events)
    }

    fn handle(
        handler: &HvfLazyGuestFaultHandler,
        member_index: usize,
        exit: HvfExceptionExit,
        pc: u64,
    ) -> Result<Option<HvfHandledLazyGuestFault>, HvfLazyGuestFaultError> {
        let candidate = handler
            .classify(exit)?
            .expect("owned test fault should classify");
        handler.handle(member_index, candidate, pc)
    }

    #[test]
    fn publishes_read_write_and_execute_as_serialized_permission_unions() {
        let page_size = 0x4000;
        let page = range(0x8000, page_size);
        let (handler, _, _, events) = test_handler(&[page], page_size);

        let read = handle(&handler, 0, data_exit(0x8120, false, false), 0x1000)
            .expect("read should resolve")
            .expect("read should be handled");
        assert_eq!(read.fault().access(), HvfLazyGuestAccess::Read);
        assert_eq!(read.populated_pages(), 1);
        assert_eq!(read.permission_changes(), 1);
        assert!(!read.stale_exit());

        let write = handle(&handler, 0, data_exit(0x8120, true, false), 0x1004)
            .expect("write should resolve")
            .expect("write should be handled");
        assert_eq!(write.fault().access(), HvfLazyGuestAccess::Write);
        assert_eq!(write.permission_changes(), 1);

        let execute = handle(&handler, 0, instruction_exit(0x2000, 0x8120), 0x2000)
            .expect("instruction should resolve")
            .expect("instruction should be handled");
        assert_eq!(execute.fault().access(), HvfLazyGuestAccess::Execute);
        assert_eq!(execute.permission_changes(), 1);

        assert_eq!(
            *events.lock().expect("event log should lock"),
            vec![
                Event::Resolve {
                    page: GuestAddress::new(0x8000),
                    access: PageAccess::Read,
                },
                Event::Protect {
                    range: page,
                    permissions: HvfMemoryPermissions::READ,
                },
                Event::Resolve {
                    page: GuestAddress::new(0x8000),
                    access: PageAccess::Write,
                },
                Event::Protect {
                    range: page,
                    permissions: HvfMemoryPermissions::new(true, true, false),
                },
                Event::Resolve {
                    page: GuestAddress::new(0x8000),
                    access: PageAccess::Read,
                },
                Event::Synchronize {
                    page: GuestAddress::new(0x8000),
                },
                Event::Protect {
                    range: page,
                    permissions: HvfMemoryPermissions::GUEST_RAM,
                },
            ]
        );
    }

    #[test]
    fn resolves_every_cross_page_byte_before_publishing_any_permission() {
        let page_size = 0x4000;
        let region = range(0x8000, page_size * 2);
        let (handler, _, _, events) = test_handler(&[region], page_size);

        let handled = handle(
            &handler,
            0,
            data_exit(0x8000 + page_size - 4, false, true),
            0x1000,
        )
        .expect("cross-page read should resolve")
        .expect("cross-page read should be handled");
        assert_eq!(handled.fault().range().size(), 8);
        assert_eq!(handled.populated_pages(), 2);
        assert_eq!(handled.permission_changes(), 2);

        assert_eq!(
            *events.lock().expect("event log should lock"),
            vec![
                Event::Resolve {
                    page: GuestAddress::new(0x8000),
                    access: PageAccess::Read,
                },
                Event::Resolve {
                    page: GuestAddress::new(0xc000),
                    access: PageAccess::Read,
                },
                Event::Protect {
                    range: range(0x8000, page_size),
                    permissions: HvfMemoryPermissions::READ,
                },
                Event::Protect {
                    range: range(0xc000, page_size),
                    permissions: HvfMemoryPermissions::READ,
                },
            ]
        );
    }

    #[test]
    fn peer_stale_exit_is_admitted_once_then_reports_no_progress() {
        let page_size = 0x4000;
        let page = range(0x8000, page_size);
        let (handler, _, _, events) = test_handler(&[page], page_size);
        let exit = data_exit(0x8120, false, false);

        handle(&handler, 0, exit, 0x1000)
            .expect("first member should resolve")
            .expect("first member should be handled");
        let stale = handle(&handler, 1, exit, 0x2000)
            .expect("peer stale exit should be admitted")
            .expect("peer stale exit should be handled");
        assert!(stale.stale_exit());
        assert_eq!(stale.populated_pages(), 0);
        assert_eq!(stale.permission_changes(), 0);

        assert_eq!(
            handle(&handler, 1, exit, 0x2000),
            Err(HvfLazyGuestFaultError::NoProgress {
                page: GuestAddress::new(0x8000),
                access: HvfLazyGuestAccess::Read,
            })
        );
        assert_eq!(
            events.lock().expect("event log should lock").len(),
            4,
            "stale exits revalidate content but must not repeat protection work"
        );
    }

    #[test]
    fn resolver_and_protection_failures_poison_without_later_publication() {
        let page_size = 0x4000;
        let page = range(0x8000, page_size);
        let (handler, resolver, _, events) = test_handler(&[page], page_size);
        resolver.fail_next();
        assert_eq!(
            handle(&handler, 0, data_exit(0x8120, false, false), 0x1000),
            Err(HvfLazyGuestFaultError::Resolution {
                failure: HvfLazyGuestResolutionFailure::Source,
            })
        );
        assert_eq!(resolver.fail_closed.load(Ordering::Relaxed), 1);
        assert_eq!(
            *events.lock().expect("event log should lock"),
            vec![Event::Resolve {
                page: GuestAddress::new(0x8000),
                access: PageAccess::Read,
            }]
        );
        assert_eq!(
            handler.classify(data_exit(0x8120, false, false)),
            Err(HvfLazyGuestFaultError::InvalidState(
                HANDLER_POISONED_MESSAGE
            ))
        );

        let (handler, resolver, mapper, events) = test_handler(&[page], page_size);
        mapper.fail_next_protect();
        let error = handle(&handler, 0, data_exit(0x8120, false, false), 0x1000)
            .expect_err("injected protection failure should surface");
        assert!(matches!(
            error,
            HvfLazyGuestFaultError::Protection { page, .. }
                if page == GuestAddress::new(0x8000)
        ));
        assert_eq!(resolver.fail_closed.load(Ordering::Relaxed), 1);
        assert_eq!(events.lock().expect("event log should lock").len(), 2);
        assert_eq!(
            handler.classify(data_exit(0x8120, false, false)),
            Err(HvfLazyGuestFaultError::InvalidState(
                HANDLER_POISONED_MESSAGE
            ))
        );

        let (handler, resolver, _, _) = test_handler(&[page], page_size);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe({
            let handler = Arc::clone(&handler);
            move || {
                let _state = handler.state.lock().expect("handler state should lock");
                panic!("inject handler lock poison");
            }
        }));
        assert!(unwind.is_err());
        assert_eq!(
            handler.classify(data_exit(0x8120, false, false)),
            Err(HvfLazyGuestFaultError::InvalidState(
                HANDLER_LOCK_POISONED_MESSAGE
            ))
        );
        assert_eq!(resolver.fail_closed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unowned_disallowed_and_invalid_vcpu_faults_remain_unhandled() {
        let page_size = 0x4000;
        let page = range(0x8000, page_size);
        let events = Arc::new(Mutex::new(Vec::new()));
        let resolver = Arc::new(RecordingResolver::new(Arc::clone(&events)));
        let mapper = Arc::new(RecordingMapper::new(Arc::clone(&events)));
        let handler = HvfLazyGuestFaultHandler::active_for_test(
            resolver,
            mapper,
            &[page],
            page_size,
            HvfMemoryPermissions::READ,
        );

        assert_eq!(
            handler
                .classify(data_exit(0x4000, false, false))
                .expect("unowned fault should not fail"),
            None
        );
        assert_eq!(
            handler
                .classify(data_exit(0x8120, true, false))
                .expect("disallowed write should not fail"),
            None
        );
        let candidate = handler
            .classify(instruction_exit(0x2000, 0x8120))
            .expect("instruction classification should not fail");
        assert_eq!(candidate, None, "READ-only memory must reject execute");

        let handler = HvfLazyGuestFaultHandler::active_for_test(
            Arc::new(RecordingResolver::new(Arc::clone(&events))),
            Arc::new(RecordingMapper::new(Arc::clone(&events))),
            &[page],
            page_size,
            HvfMemoryPermissions::GUEST_RAM,
        );
        let candidate = handler
            .classify(instruction_exit(0x2000, 0x8120))
            .expect("owned instruction should classify")
            .expect("owned instruction candidate should exist");
        assert_eq!(
            handler
                .handle(0, candidate, 0x2004)
                .expect("mismatched PC should fall through"),
            None
        );
        assert!(events.lock().expect("event log should lock").is_empty());
    }

    #[test]
    fn debug_and_resolution_errors_do_not_expose_guest_addresses() {
        let page_size = 0x4000;
        let (handler, resolver, _, _) = test_handler(&[range(0x8000, page_size)], page_size);
        assert_eq!(
            format!("{handler:?}"),
            "HvfLazyGuestFaultHandler(<redacted>)"
        );
        resolver.fail_next();
        let error = handle(&handler, 0, data_exit(0x8120, false, false), 0x1000)
            .expect_err("injected resolver failure should surface");
        assert!(!format!("{error:?}").contains("8120"));
        assert!(!error.to_string().contains("8120"));
    }
}
