//! Task-local host fault mediation for coordinated lazy guest memory.
//!
//! The public adapter keeps Mach task/thread rights and private writable
//! aliases inside the VMM process. It does not broker a pager connection,
//! install HVF guest permissions, or enable snapshot API behavior.

use std::collections::TryReserveError;
use std::error::Error;
use std::ffi::c_void;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use bangbang_pager::{PageAccess, PagerGeneration, PagerRegionId};
use bangbang_runtime::lazy_memory::{
    LazyGuestMemory, LazyGuestMemoryError, LazyGuestMemoryTerminalReason, LazyPageFault,
};
use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange, GuestMemoryRegion};

use crate::mach_lazy::{
    MACH_ACCESS_READ, MACH_ACCESS_WRITE, MACH_FAULT_FORWARD, MACH_FAULT_HANDLED,
    MACH_FAULT_TERMINAL, MachExceptionOwner, MachLazyContents, MachLazyError, MachLazyMapping,
    is_supported_target,
};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
unsafe extern "C" {
    fn sys_icache_invalidate(start: *mut c_void, length: usize);
}

/// Fixed exit status used when an owned host fault cannot be resolved safely.
pub const HVF_LAZY_HOST_FAULT_TERMINAL_EXIT_CODE: i32 = crate::mach_lazy::MACH_TERMINAL_EXIT_CODE;

/// Offset-only page request presented to an in-process content source.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HvfLazyPageRequest {
    region: PagerRegionId,
    generation: PagerGeneration,
    access: PageAccess,
    offset: u64,
    source_offset: u64,
    length: u32,
}

impl HvfLazyPageRequest {
    /// Returns the opaque pager region identity.
    pub const fn region(self) -> PagerRegionId {
        self.region
    }

    /// Returns the exact nonzero population generation.
    pub const fn generation(self) -> PagerGeneration {
        self.generation
    }

    /// Returns the access that initiated this population request.
    pub const fn access(self) -> PageAccess {
        self.access
    }

    /// Returns the region-relative page offset.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns the peer-owned source offset.
    pub const fn source_offset(self) -> u64 {
        self.source_offset
    }

    /// Returns the exact requested page length.
    pub const fn length(self) -> u32 {
        self.length
    }
}

impl fmt::Debug for HvfLazyPageRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HvfLazyPageRequest(<redacted>)")
    }
}

/// Exact page contents returned by a trusted in-process source adapter.
pub enum HvfLazyPageContents {
    /// One exact data page.
    Data(Vec<u8>),
    /// One exact all-zero page.
    Zero,
}

impl HvfLazyPageContents {
    /// Constructs one data response whose exact length is checked on use.
    pub fn data(bytes: Vec<u8>) -> Self {
        Self::Data(bytes)
    }

    /// Constructs one zero response.
    pub const fn zero() -> Self {
        Self::Zero
    }
}

impl fmt::Debug for HvfLazyPageContents {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HvfLazyPageContents(<redacted>)")
    }
}

/// Redacted failure returned by one page-content source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfLazyPageSourceError;

impl HvfLazyPageSourceError {
    /// Constructs one intentionally detail-free source failure.
    pub const fn failed() -> Self {
        Self
    }
}

impl fmt::Display for HvfLazyPageSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("lazy page source failed")
    }
}

impl Error for HvfLazyPageSourceError {}

/// Trusted in-process adapter that obtains exact lazy-page contents.
///
/// Implementations must remain bounded and must not access a mapping owned by
/// the same bridge while serving a request. Pager socket ownership and timeout
/// policy are added by later delivery slices.
pub trait HvfLazyPageSource: Send + Sync {
    /// Returns exact data or zero contents for one offset-only request.
    fn page(
        &self,
        request: HvfLazyPageRequest,
    ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError>;
}

/// Whether one successful resolver call populated content or reused it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfLazyPageResolution {
    /// This call installed and committed the page contents.
    Populated,
    /// Contents were already committed; only host permission was ensured.
    Present,
}

/// Stable stage for a host-fault bridge failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfLazyHostFaultStage {
    /// Validate memory regions and host-page granularity.
    Validate,
    /// Construct private writable aliases.
    Alias,
    /// Install or remove the task exception owner.
    ExceptionOwner,
    /// Protect or publish one original host mapping.
    Protection,
    /// Acquire or commit one coordinator transition.
    Coordinator,
    /// Obtain exact page contents.
    Source,
    /// Serialize installation or shutdown lifecycle.
    Lifecycle,
}

/// Redacted failure from host-fault bridge construction, resolution, or shutdown.
pub enum HvfLazyHostFaultError {
    /// The compile target cannot install the public Mach bridge.
    UnsupportedTarget,
    /// Configuration or retained mapping metadata is incompatible.
    InvalidConfiguration {
        /// The stage that rejected the configuration.
        stage: HvfLazyHostFaultStage,
    },
    /// Fallible adapter metadata allocation failed.
    MetadataAllocationFailed {
        /// The allocation failure.
        source: TryReserveError,
    },
    /// A native platform operation failed without exposing ports or addresses.
    Platform {
        /// The failed platform stage.
        stage: HvfLazyHostFaultStage,
    },
    /// The backend-neutral coordinator rejected or terminated work.
    Coordinator {
        /// The coordinator failure.
        source: LazyGuestMemoryError,
    },
    /// The content source failed.
    Source {
        /// The redacted source failure.
        source: HvfLazyPageSourceError,
    },
    /// Source data did not have the exact negotiated page length.
    ContentLength,
    /// The resolver is not active or shutdown ownership is inconsistent.
    InvalidLifecycle,
}

impl fmt::Debug for HvfLazyHostFaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget => {
                formatter.write_str("HvfLazyHostFaultError::UnsupportedTarget")
            }
            Self::InvalidConfiguration { stage } => formatter
                .debug_tuple("HvfLazyHostFaultError::InvalidConfiguration")
                .field(stage)
                .finish(),
            Self::MetadataAllocationFailed { .. } => {
                formatter.write_str("HvfLazyHostFaultError::MetadataAllocationFailed(<redacted>)")
            }
            Self::Platform { stage } => formatter
                .debug_tuple("HvfLazyHostFaultError::Platform")
                .field(stage)
                .finish(),
            Self::Coordinator { .. } => {
                formatter.write_str("HvfLazyHostFaultError::Coordinator(<redacted>)")
            }
            Self::Source { .. } => formatter.write_str("HvfLazyHostFaultError::Source(<redacted>)"),
            Self::ContentLength => formatter.write_str("HvfLazyHostFaultError::ContentLength"),
            Self::InvalidLifecycle => {
                formatter.write_str("HvfLazyHostFaultError::InvalidLifecycle")
            }
        }
    }
}

impl fmt::Display for HvfLazyHostFaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedTarget => {
                formatter.write_str("lazy host fault bridge requires macOS on Apple Silicon")
            }
            Self::InvalidConfiguration { stage } => {
                write!(
                    formatter,
                    "invalid lazy host fault configuration at {stage:?}"
                )
            }
            Self::MetadataAllocationFailed { .. } => {
                formatter.write_str("failed to allocate lazy host fault metadata")
            }
            Self::Platform { stage } => {
                write!(
                    formatter,
                    "lazy host fault platform operation failed at {stage:?}"
                )
            }
            Self::Coordinator { .. } => formatter.write_str("lazy host fault coordinator failed"),
            Self::Source { .. } => formatter.write_str("lazy host page source failed"),
            Self::ContentLength => formatter.write_str("lazy host page content length is invalid"),
            Self::InvalidLifecycle => {
                formatter.write_str("lazy host fault bridge lifecycle is invalid")
            }
        }
    }
}

impl Error for HvfLazyHostFaultError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::MetadataAllocationFailed { source } => Some(source),
            Self::Coordinator { source } => Some(source),
            Self::Source { source } => Some(source),
            _ => None,
        }
    }
}

/// Cloneable resolver shared by the task bridge and HVF guest protection.
#[derive(Clone)]
pub struct HvfLazyPageResolver {
    inner: Arc<ResolverInner>,
}

impl HvfLazyPageResolver {
    /// Resolves one owned guest address through the shared content path.
    ///
    /// The task exception bridge uses the same method after translating an
    /// owned host fault. The HVF guest-fault adapter clones this handle and
    /// calls it before publishing stage-two permissions.
    pub fn resolve_guest_address(
        &self,
        address: GuestAddress,
        access: PageAccess,
    ) -> Result<HvfLazyPageResolution, HvfLazyHostFaultError> {
        let region_index = self.inner.region_for_guest(address).ok_or(
            HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate,
            },
        )?;
        self.inner.resolve(region_index, address, access)
    }

    pub(crate) fn mapping_regions(&self) -> &[GuestMemoryRegion] {
        self.inner.memory.mapping_regions()
    }

    pub(crate) fn page_size(&self) -> u32 {
        self.inner.memory.page_size()
    }

    pub(crate) fn fail_closed(&self) {
        self.inner.fail_closed();
    }

    pub(crate) fn synchronize_instruction_page(
        &self,
        address: GuestAddress,
    ) -> Result<(), HvfLazyHostFaultError> {
        self.inner.synchronize_instruction_page(address)
    }
}

impl fmt::Debug for HvfLazyPageResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HvfLazyPageResolver(<redacted>)")
    }
}

/// Outcome of one explicit bridge shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfLazyHostFaultShutdown {
    prior_handler_restored: bool,
}

impl HvfLazyHostFaultShutdown {
    /// Returns whether shutdown restored the captured task handler.
    ///
    /// `false` means a later owner had replaced the bridge and was preserved.
    pub const fn prior_handler_restored(self) -> bool {
        self.prior_handler_restored
    }
}

/// Installed task-local exception owner for one lazy guest-memory resolver.
pub struct HvfLazyHostFaultBridge {
    resolver: HvfLazyPageResolver,
    exception_owner: Option<MachExceptionOwner>,
    callback_context: Option<Box<CallbackContext>>,
}

impl HvfLazyHostFaultBridge {
    /// Transactionally constructs aliases, installs the exception owner, and
    /// protects every original lazy mapping.
    ///
    /// The supported worker must not install a competing thread-specific
    /// bad-access handler or concurrently replace the task bad-access slot.
    /// Callers must publish this owner before any uncoordinated access to the
    /// retained lazy mappings.
    pub fn install(
        memory: Arc<LazyGuestMemory>,
        source: Arc<dyn HvfLazyPageSource>,
    ) -> Result<Self, HvfLazyHostFaultError> {
        if !is_supported_target() {
            return Err(HvfLazyHostFaultError::UnsupportedTarget);
        }
        let inner = Arc::new(ResolverInner::new(memory, source)?);
        let resolver = HvfLazyPageResolver { inner };
        let mut callback_context = Box::new(CallbackContext {
            resolver: resolver.clone(),
        });
        let context = NonNull::from(callback_context.as_mut()).cast::<c_void>();
        let mut exception_owner = MachExceptionOwner::install(context, mach_fault_callback)
            .map_err(|_| HvfLazyHostFaultError::Platform {
                stage: HvfLazyHostFaultStage::ExceptionOwner,
            })?;

        if let Err(error) = resolver.inner.activate() {
            if exception_owner.shutdown().is_err() {
                crate::mach_lazy::terminal_exit();
            }
            return Err(error);
        }

        Ok(Self {
            resolver,
            exception_owner: Some(exception_owner),
            callback_context: Some(callback_context),
        })
    }

    /// Returns a shared resolver for the HVF guest-fault protection plane.
    pub fn resolver(&self) -> HvfLazyPageResolver {
        self.resolver.clone()
    }

    /// Quiesces work, restores host mappings and the prior task owner, and
    /// joins the exception server.
    ///
    /// The caller must ensure no thread can newly access an absent owned page
    /// during this operation. A teardown failure takes the same fixed
    /// fail-closed worker exit as an owned callback failure.
    pub fn shutdown(mut self) -> Result<HvfLazyHostFaultShutdown, HvfLazyHostFaultError> {
        match self.teardown() {
            Ok(report) => Ok(report),
            Err(_) => {
                self.resolver.inner.fail_closed();
                crate::mach_lazy::terminal_exit();
            }
        }
    }

    fn teardown(&mut self) -> Result<HvfLazyHostFaultShutdown, HvfLazyHostFaultError> {
        let Some(owner) = self.exception_owner.as_mut() else {
            return Err(HvfLazyHostFaultError::InvalidLifecycle);
        };
        self.resolver.inner.close()?;
        self.resolver.inner.mapping.restore_all_rw().map_err(|_| {
            HvfLazyHostFaultError::Platform {
                stage: HvfLazyHostFaultStage::Protection,
            }
        })?;
        let prior_handler_restored =
            owner
                .shutdown()
                .map_err(|_| HvfLazyHostFaultError::Platform {
                    stage: HvfLazyHostFaultStage::ExceptionOwner,
                })?;
        self.exception_owner = None;
        self.callback_context = None;
        Ok(HvfLazyHostFaultShutdown {
            prior_handler_restored,
        })
    }
}

impl fmt::Debug for HvfLazyHostFaultBridge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("HvfLazyHostFaultBridge(<redacted>)")
    }
}

impl Drop for HvfLazyHostFaultBridge {
    fn drop(&mut self) {
        if self.exception_owner.is_some() && self.teardown().is_err() {
            self.resolver.inner.fail_closed();
            crate::mach_lazy::terminal_exit();
        }
    }
}

struct CallbackContext {
    resolver: HvfLazyPageResolver,
}

unsafe extern "C" fn mach_fault_callback(context: *mut c_void, address: u64, access: u32) -> u32 {
    let Some(context) = NonNull::new(context.cast::<CallbackContext>()) else {
        return MACH_FAULT_TERMINAL;
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: native installation receives a pointer to a retained Box and
        // successful shutdown joins the server before that Box is dropped.
        let context = unsafe { context.as_ref() };
        context.resolver.inner.resolve_host_address(address, access)
    }));
    match outcome {
        Ok(HostFaultOutcome::Handled) => MACH_FAULT_HANDLED,
        Ok(HostFaultOutcome::Forward) => MACH_FAULT_FORWARD,
        Ok(HostFaultOutcome::Terminal) | Err(_) => MACH_FAULT_TERMINAL,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostFaultOutcome {
    Handled,
    Forward,
    Terminal,
}

#[derive(Clone, Copy)]
struct ResolverRegion {
    guest: GuestMemoryRange,
    host_start: usize,
    host_end: usize,
}

struct ResolverInner {
    // Field drop order is significant: aliases and permission metadata must
    // be destroyed while the retained primary mappings are still live.
    mapping: MachLazyMapping,
    memory: Arc<LazyGuestMemory>,
    source: Arc<dyn HvfLazyPageSource>,
    regions: Vec<ResolverRegion>,
    page_size: usize,
    lifecycle: Mutex<ResolverLifecycle>,
    changed: Condvar,
}

impl ResolverInner {
    fn new(
        memory: Arc<LazyGuestMemory>,
        source: Arc<dyn HvfLazyPageSource>,
    ) -> Result<Self, HvfLazyHostFaultError> {
        let host_page_size = usize::try_from(crate::memory::host_page_size().map_err(|_| {
            HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate,
            }
        })?)
        .map_err(|_| HvfLazyHostFaultError::InvalidConfiguration {
            stage: HvfLazyHostFaultStage::Validate,
        })?;
        let page_size = usize::try_from(memory.page_size()).map_err(|_| {
            HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate,
            }
        })?;
        let mapping_regions = memory.mapping_regions();
        if mapping_regions.is_empty()
            || mapping_regions.len() != memory.region_count()
            || page_size < host_page_size
            || !page_size.is_multiple_of(host_page_size)
        {
            return Err(HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate,
            });
        }

        let mut regions = Vec::new();
        regions
            .try_reserve_exact(mapping_regions.len())
            .map_err(|source| HvfLazyHostFaultError::MetadataAllocationFailed { source })?;
        for mapping_region in mapping_regions {
            let host_start = mapping_region.host_address().as_ptr() as usize;
            let host_end = host_start.checked_add(mapping_region.host_size()).ok_or(
                HvfLazyHostFaultError::InvalidConfiguration {
                    stage: HvfLazyHostFaultStage::Validate,
                },
            )?;
            if host_start == 0
                || mapping_region.host_size() == 0
                || mapping_region.host_size() % page_size != 0
                || regions.iter().any(|existing: &ResolverRegion| {
                    host_start < existing.host_end && existing.host_start < host_end
                })
            {
                return Err(HvfLazyHostFaultError::InvalidConfiguration {
                    stage: HvfLazyHostFaultStage::Validate,
                });
            }
            regions.push(ResolverRegion {
                guest: mapping_region.range(),
                host_start,
                host_end,
            });
        }

        let mapping = MachLazyMapping::new(mapping_regions).map_err(|source| match source {
            MachLazyError::Invalid => HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Alias,
            },
            _ => HvfLazyHostFaultError::Platform {
                stage: HvfLazyHostFaultStage::Alias,
            },
        })?;
        Ok(Self {
            mapping,
            memory,
            source,
            regions,
            page_size,
            lifecycle: Mutex::new(ResolverLifecycle {
                phase: ResolverPhase::Prepared,
                actions: 0,
            }),
            changed: Condvar::new(),
        })
    }

    fn activate(&self) -> Result<(), HvfLazyHostFaultError> {
        self.mapping
            .protect_all_none()
            .map_err(|_| HvfLazyHostFaultError::Platform {
                stage: HvfLazyHostFaultStage::Protection,
            })?;
        let mut lifecycle = self.lock_lifecycle()?;
        if lifecycle.phase != ResolverPhase::Prepared {
            return Err(HvfLazyHostFaultError::InvalidLifecycle);
        }
        lifecycle.phase = ResolverPhase::Active;
        Ok(())
    }

    fn close(&self) -> Result<(), HvfLazyHostFaultError> {
        let mut lifecycle = self.lock_lifecycle()?;
        match lifecycle.phase {
            ResolverPhase::Active => lifecycle.phase = ResolverPhase::Closing,
            ResolverPhase::Closing => {}
            ResolverPhase::Prepared | ResolverPhase::Closed => {
                return Err(HvfLazyHostFaultError::InvalidLifecycle);
            }
        }
        while lifecycle.actions != 0 {
            lifecycle = self
                .changed
                .wait(lifecycle)
                .map_err(|_| HvfLazyHostFaultError::InvalidLifecycle)?;
        }
        lifecycle.phase = ResolverPhase::Closed;
        Ok(())
    }

    fn lock_lifecycle(&self) -> Result<MutexGuard<'_, ResolverLifecycle>, HvfLazyHostFaultError> {
        self.lifecycle
            .lock()
            .map_err(|_| HvfLazyHostFaultError::InvalidLifecycle)
    }

    fn begin_action(&self) -> Result<ResolverAction<'_>, HvfLazyHostFaultError> {
        let mut lifecycle = self.lock_lifecycle()?;
        if lifecycle.phase != ResolverPhase::Active {
            return Err(HvfLazyHostFaultError::InvalidLifecycle);
        }
        lifecycle.actions = lifecycle
            .actions
            .checked_add(1)
            .ok_or(HvfLazyHostFaultError::InvalidLifecycle)?;
        Ok(ResolverAction {
            resolver: self,
            active: true,
        })
    }

    fn finish_action(&self) {
        let Ok(mut lifecycle) = self.lifecycle.lock() else {
            self.fail_closed();
            crate::mach_lazy::terminal_exit();
        };
        let Some(actions) = lifecycle.actions.checked_sub(1) else {
            drop(lifecycle);
            self.fail_closed();
            crate::mach_lazy::terminal_exit();
        };
        lifecycle.actions = actions;
        if actions == 0 {
            self.changed.notify_all();
        }
    }

    fn region_for_guest(&self, address: GuestAddress) -> Option<usize> {
        self.regions
            .iter()
            .position(|region| region.guest.contains(address))
    }

    fn translate_host(&self, address: u64) -> Option<(usize, GuestAddress)> {
        let address = usize::try_from(address).ok()?;
        self.regions.iter().enumerate().find_map(|(index, region)| {
            if address < region.host_start || address >= region.host_end {
                return None;
            }
            let offset = u64::try_from(address.checked_sub(region.host_start)?).ok()?;
            Some((index, region.guest.start().checked_add(offset)?))
        })
    }

    fn resolve_host_address(&self, address: u64, access: u32) -> HostFaultOutcome {
        let Some((region_index, guest_address)) = self.translate_host(address) else {
            return HostFaultOutcome::Forward;
        };
        let access = match access {
            MACH_ACCESS_READ => PageAccess::Read,
            MACH_ACCESS_WRITE => PageAccess::Write,
            _ => return HostFaultOutcome::Forward,
        };
        match self.resolve(region_index, guest_address, access) {
            Ok(_) => HostFaultOutcome::Handled,
            Err(_) => HostFaultOutcome::Terminal,
        }
    }

    fn resolve(
        &self,
        region_index: usize,
        address: GuestAddress,
        access: PageAccess,
    ) -> Result<HvfLazyPageResolution, HvfLazyHostFaultError> {
        let _action = self.begin_action()?;
        let result = self.resolve_inner(region_index, address, access);
        if result.is_err() {
            self.fail_closed();
        }
        result
    }

    fn resolve_inner(
        &self,
        region_index: usize,
        address: GuestAddress,
        access: PageAccess,
    ) -> Result<HvfLazyPageResolution, HvfLazyHostFaultError> {
        let region =
            self.regions
                .get(region_index)
                .ok_or(HvfLazyHostFaultError::InvalidConfiguration {
                    stage: HvfLazyHostFaultStage::Validate,
                })?;
        if !region.guest.contains(address) {
            return Err(HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate,
            });
        }
        match self
            .memory
            .fault_address(address, access)
            .map_err(|source| HvfLazyHostFaultError::Coordinator { source })?
        {
            LazyPageFault::Present => {
                let page_offset = self.page_offset(region.guest, address)?;
                self.mapping
                    .allow(
                        region_index,
                        page_offset,
                        self.page_size,
                        access == PageAccess::Write,
                    )
                    .map_err(|_| HvfLazyHostFaultError::Platform {
                        stage: HvfLazyHostFaultStage::Protection,
                    })?;
                Ok(HvfLazyPageResolution::Present)
            }
            LazyPageFault::Populate(population) => {
                let request = HvfLazyPageRequest {
                    region: population.region(),
                    generation: population.generation(),
                    access: population.access(),
                    offset: population.offset(),
                    source_offset: population.source_offset(),
                    length: population.length(),
                };
                let guest_range = population.guest_range();
                let contents = self
                    .source
                    .page(request)
                    .map_err(|source| HvfLazyHostFaultError::Source { source })?;
                let expected_length = usize::try_from(request.length()).map_err(|_| {
                    HvfLazyHostFaultError::InvalidConfiguration {
                        stage: HvfLazyHostFaultStage::Validate,
                    }
                })?;
                if matches!(&contents, HvfLazyPageContents::Data(data) if data.len() != expected_length)
                {
                    return Err(HvfLazyHostFaultError::ContentLength);
                }

                let page_offset = usize::try_from(
                    guest_range
                        .start()
                        .raw_value()
                        .checked_sub(region.guest.start().raw_value())
                        .ok_or(HvfLazyHostFaultError::InvalidConfiguration {
                            stage: HvfLazyHostFaultStage::Validate,
                        })?,
                )
                .map_err(|_| HvfLazyHostFaultError::InvalidConfiguration {
                    stage: HvfLazyHostFaultStage::Validate,
                })?;
                let mut publication = population
                    .begin_publication()
                    .map_err(|source| HvfLazyHostFaultError::Coordinator { source })?;
                let mut target = publication
                    .target()
                    .map_err(|source| HvfLazyHostFaultError::Coordinator { source })?;
                if target.range() != guest_range || target.len() != expected_length {
                    return Err(HvfLazyHostFaultError::InvalidConfiguration {
                        stage: HvfLazyHostFaultStage::Coordinator,
                    });
                }
                let native_contents = match &contents {
                    HvfLazyPageContents::Data(data) => MachLazyContents::Data(data),
                    HvfLazyPageContents::Zero => MachLazyContents::Zero {
                        length: expected_length,
                    },
                };
                if let Err(error) = self.mapping.publish(
                    region_index,
                    page_offset,
                    native_contents,
                    access == PageAccess::Write,
                ) {
                    let _ = self
                        .mapping
                        .hide(region_index, page_offset, expected_length);
                    return Err(platform_error(error, HvfLazyHostFaultStage::Protection));
                }
                // SAFETY: MachLazyMapping construction created a non-copying
                // alias of this exact retained mapping. `publish` validated
                // the page range, initialized every byte through that alias,
                // issued a sequentially consistent fence, and opened the
                // matching original permission before returning success.
                if let Err(source) = unsafe { target.assume_initialized_by_platform() } {
                    let _ = self
                        .mapping
                        .hide(region_index, page_offset, expected_length);
                    return Err(HvfLazyHostFaultError::Coordinator { source });
                }
                if let Err(source) = publication.commit() {
                    let _ = self
                        .mapping
                        .hide(region_index, page_offset, expected_length);
                    return Err(HvfLazyHostFaultError::Coordinator { source });
                }
                Ok(HvfLazyPageResolution::Populated)
            }
        }
    }

    fn page_offset(
        &self,
        region: GuestMemoryRange,
        address: GuestAddress,
    ) -> Result<usize, HvfLazyHostFaultError> {
        let relative = address
            .raw_value()
            .checked_sub(region.start().raw_value())
            .ok_or(HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate,
            })?;
        let page_size = u64::try_from(self.page_size).map_err(|_| {
            HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate,
            }
        })?;
        usize::try_from(relative - relative % page_size).map_err(|_| {
            HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate,
            }
        })
    }

    fn synchronize_instruction_page(
        &self,
        address: GuestAddress,
    ) -> Result<(), HvfLazyHostFaultError> {
        let _action = self.begin_action()?;
        let region_index =
            self.region_for_guest(address)
                .ok_or(HvfLazyHostFaultError::InvalidConfiguration {
                    stage: HvfLazyHostFaultStage::Validate,
                })?;
        let region =
            self.regions
                .get(region_index)
                .ok_or(HvfLazyHostFaultError::InvalidConfiguration {
                    stage: HvfLazyHostFaultStage::Validate,
                })?;
        let page_offset = self.page_offset(region.guest, address)?;
        let host_address = region.host_start.checked_add(page_offset).ok_or(
            HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate,
            },
        )?;

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            // SAFETY: resolver construction retained this complete mapping,
            // page_offset identifies one aligned coordinator page inside it,
            // and page resolution completed before the guest adapter calls
            // this method. The public libSystem routine only synchronizes
            // instruction visibility for the supplied live byte range.
            unsafe {
                sys_icache_invalidate(host_address as *mut c_void, self.page_size);
            }
            Ok(())
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = host_address;
            Err(HvfLazyHostFaultError::UnsupportedTarget)
        }
    }

    fn fail_closed(&self) {
        let _ = self
            .memory
            .terminate(LazyGuestMemoryTerminalReason::TransitionFailure);
    }
}

impl fmt::Debug for ResolverInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResolverInner(<redacted>)")
    }
}

struct ResolverAction<'a> {
    resolver: &'a ResolverInner,
    active: bool,
}

impl Drop for ResolverAction<'_> {
    fn drop(&mut self) {
        if self.active {
            self.resolver.finish_action();
            self.active = false;
        }
    }
}

struct ResolverLifecycle {
    phase: ResolverPhase,
    actions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolverPhase {
    Prepared,
    Active,
    Closing,
    Closed,
}

fn platform_error(_source: MachLazyError, stage: HvfLazyHostFaultStage) -> HvfLazyHostFaultError {
    HvfLazyHostFaultError::Platform { stage }
}

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod tests {
    use std::mem::size_of;
    use std::process::Command;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::mpsc::{self, TryRecvError};
    use std::thread;
    use std::time::Duration;

    use bangbang_pager::{
        MAX_FRAME_BYTES, PagerGeneration, PagerLimits, PagerOperations, PagerRegionId,
    };
    use bangbang_runtime::BackendError;
    use bangbang_runtime::lazy_memory::{
        LazyGuestMemoryLimits, LazyGuestMemoryRegion, LazyPageState,
    };

    use crate::exit::{HvfExceptionExit, HvfLazyGuestAccess};
    use crate::lazy_guest_fault::HvfLazyGuestFaultHandler;
    use crate::memory::{
        HvfMappedGuestMemoryRegion, HvfMemoryMapRequest, HvfMemoryMapper, HvfMemoryPermissions,
    };

    use super::*;

    const CHILD_ENV: &str = "BANGBANG_MACH_LAZY_UNIT_CHILD";
    const GUEST_BASE: u64 = 0x8000_0000;
    const SOURCE_BASE: u64 = 0x10_0000;
    const TEST_VALUE: u32 = 0x3141_5926;
    static MACH_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct TestSource {
        requests: Mutex<Vec<HvfLazyPageRequest>>,
        reply: TestReply,
    }

    enum TestReply {
        Data(Vec<u8>),
        Zero,
        Failure,
    }

    #[derive(Debug, Default)]
    struct ConcurrentProtectionMapper {
        protects: Mutex<Vec<(GuestMemoryRange, HvfMemoryPermissions)>>,
    }

    impl HvfMemoryMapper for ConcurrentProtectionMapper {
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
            self.protects
                .lock()
                .map_err(|_| BackendError::InvalidState("test protection log is poisoned"))?
                .push((range, permissions));
            Ok(())
        }
    }

    struct BlockingZeroSource {
        requests: AtomicU64,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl HvfLazyPageSource for BlockingZeroSource {
        fn page(
            &self,
            _request: HvfLazyPageRequest,
        ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
            self.requests.fetch_add(1, Ordering::Relaxed);
            self.entered
                .send(())
                .map_err(|_| HvfLazyPageSourceError::failed())?;
            self.release
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .recv()
                .map_err(|_| HvfLazyPageSourceError::failed())?;
            Ok(HvfLazyPageContents::zero())
        }
    }

    impl HvfLazyPageSource for TestSource {
        fn page(
            &self,
            request: HvfLazyPageRequest,
        ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
            self.requests
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .push(request);
            match &self.reply {
                TestReply::Data(page) => Ok(HvfLazyPageContents::data(page.clone())),
                TestReply::Zero => Ok(HvfLazyPageContents::zero()),
                TestReply::Failure => Err(HvfLazyPageSourceError::failed()),
            }
        }
    }

    fn memory_with_region(
        page_size: u32,
        region_size: u64,
    ) -> Result<Arc<LazyGuestMemory>, &'static str> {
        let pager = PagerLimits::new(
            page_size,
            1,
            2,
            u32::try_from(MAX_FRAME_BYTES).map_err(|_| "maximum frame size should fit u32")?,
            PagerOperations::v1(),
        )
        .map_err(|_| "test pager limits should validate")?;
        let page_count = region_size
            .checked_div(u64::from(page_size))
            .ok_or("test region page count should divide")?;
        let limits = LazyGuestMemoryLimits::new(pager, page_count, 8)
            .map_err(|_| "test lazy-memory limits should validate")?;
        let range = GuestMemoryRange::new(GuestAddress::new(GUEST_BASE), region_size)
            .map_err(|_| "test guest range should validate")?;
        let region = LazyGuestMemoryRegion::new(
            PagerRegionId::new(1).map_err(|_| "test region id should validate")?,
            range,
            SOURCE_BASE,
            page_size,
        )
        .map_err(|_| "test lazy region should validate")?;
        LazyGuestMemory::new(limits, vec![region])
            .map(Arc::new)
            .map_err(|_| "test lazy memory should construct")
    }

    fn memory(page_size: u32, page_count: u64) -> Result<Arc<LazyGuestMemory>, &'static str> {
        let region_size = u64::from(page_size)
            .checked_mul(page_count)
            .ok_or("test region size should fit")?;
        memory_with_region(page_size, region_size)
    }

    #[test]
    fn resolves_real_task_local_accesses_in_subprocess() {
        let _test_lock = MACH_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        if std::env::var_os(CHILD_ENV).is_none() {
            let executable =
                std::env::current_exe().expect("current unit-test executable should resolve");
            let output = Command::new(executable)
                .args([
                    "--exact",
                    "lazy_host_fault::tests::resolves_real_task_local_accesses_in_subprocess",
                    "--nocapture",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .expect("Mach lazy child should launch");
            assert!(
                output.status.success(),
                "Mach lazy child failed: status={:?}\nstdout={}\nstderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }

        let page_size =
            u32::try_from(crate::memory::host_page_size().expect("host page size should resolve"))
                .expect("host page size should fit u32");
        let memory = memory(page_size, 4).expect("test lazy memory should construct");
        let pointer = memory.mapping_regions()[0]
            .host_address()
            .as_ptr()
            .cast::<u8>();
        let page_bytes = usize::try_from(page_size).expect("page size should fit usize");
        let mut page = vec![0_u8; page_bytes];
        page[..size_of::<u32>()].copy_from_slice(&TEST_VALUE.to_ne_bytes());
        let source = Arc::new(TestSource {
            requests: Mutex::new(Vec::new()),
            reply: TestReply::Data(page),
        });
        let bridge =
            HvfLazyHostFaultBridge::install(Arc::clone(&memory), Arc::<TestSource>::clone(&source))
                .expect("real task-local bridge should install");

        // SAFETY: the retained lazy mapping is aligned and valid for one u32.
        // The bridge owns its absent protection and resolves this read before
        // the instruction retries.
        let value = unsafe { std::ptr::read_volatile(pointer.cast::<u32>()) };
        assert_eq!(value, TEST_VALUE);
        let write_value = 0xa5a5_5a5a;
        // SAFETY: the second retained page is aligned and valid for one u32;
        // the bridge populates it before the write instruction retries.
        unsafe {
            std::ptr::write_volatile(pointer.add(page_bytes).cast::<u32>(), write_value);
        }
        // SAFETY: the third retained page is aligned for AtomicU64 and remains
        // live. The atomic write fault is resolved before retry.
        let atomic_old = unsafe {
            (&*pointer.add(page_bytes * 2).cast::<AtomicU64>()).fetch_add(1, Ordering::SeqCst)
        };
        assert_eq!(atomic_old, u64::from(TEST_VALUE));
        let raw = 0x8877_6655_4433_2211_u64.to_ne_bytes();
        // SAFETY: the fourth retained page is valid for the exact source
        // length; the bridge resolves the raw-pointer write before retry.
        unsafe {
            std::ptr::copy_nonoverlapping(raw.as_ptr(), pointer.add(page_bytes * 3), raw.len());
        }

        let region_id = PagerRegionId::new(1).expect("test region id should validate");
        for page_index in 0..4_u64 {
            assert_eq!(
                memory
                    .page_state(region_id, u64::from(page_size) * page_index)
                    .expect("page state should resolve"),
                LazyPageState::Present
            );
        }
        // SAFETY: all four pages are present and readable after resolution.
        unsafe {
            assert_eq!(
                std::ptr::read_volatile(pointer.add(page_bytes).cast::<u32>()),
                write_value
            );
            assert_eq!(
                std::ptr::read_volatile(pointer.add(page_bytes * 3).cast::<u64>()),
                u64::from_ne_bytes(raw)
            );
        }
        let requests = source
            .requests
            .lock()
            .expect("test request log should not be poisoned");
        assert_eq!(requests.len(), 4);
        assert_eq!(requests[0].access(), PageAccess::Read);
        let accesses = requests
            .iter()
            .map(|request| request.access())
            .collect::<Vec<_>>();
        assert_eq!(
            accesses,
            [
                PageAccess::Read,
                PageAccess::Write,
                PageAccess::Read,
                PageAccess::Write,
            ],
            "the atomic read-modify-write first faults on its load and upgrades host permission on its retrying store"
        );
        assert_eq!(
            requests
                .iter()
                .map(|request| request.offset())
                .collect::<Vec<_>>(),
            (0..4_u64)
                .map(|index| u64::from(page_size) * index)
                .collect::<Vec<_>>()
        );
        drop(requests);
        assert!(
            bridge
                .shutdown()
                .expect("real task-local bridge should shut down")
                .prior_handler_restored()
        );
    }

    #[test]
    fn rejects_sub_host_page_granularity_before_exception_install() {
        let _test_lock = MACH_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        let host_page_size =
            crate::memory::host_page_size().expect("host page size should resolve");
        let memory = memory_with_region(4 * 1024, host_page_size)
            .expect("sub-host-page lazy memory should construct");
        let source = Arc::new(TestSource {
            requests: Mutex::new(Vec::new()),
            reply: TestReply::Data(vec![0; 4 * 1024]),
        });
        assert!(matches!(
            HvfLazyHostFaultBridge::install(memory, source),
            Err(HvfLazyHostFaultError::InvalidConfiguration {
                stage: HvfLazyHostFaultStage::Validate
            })
        ));
    }

    #[test]
    fn shared_resolver_publishes_zero_without_faulting_itself() {
        let _test_lock = MACH_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        let page_size =
            u32::try_from(crate::memory::host_page_size().expect("host page size should resolve"))
                .expect("host page size should fit u32");
        let memory = memory(page_size, 1).expect("test lazy memory should construct");
        let pointer = memory.mapping_regions()[0]
            .host_address()
            .as_ptr()
            .cast::<u32>();
        let source = Arc::new(TestSource {
            requests: Mutex::new(Vec::new()),
            reply: TestReply::Zero,
        });
        let bridge =
            HvfLazyHostFaultBridge::install(Arc::clone(&memory), Arc::<TestSource>::clone(&source))
                .expect("test bridge should install");
        assert_eq!(
            bridge
                .resolver()
                .resolve_guest_address(GuestAddress::new(GUEST_BASE), PageAccess::Read,)
                .expect("shared resolver should publish zero"),
            HvfLazyPageResolution::Populated
        );
        // SAFETY: the resolver committed this retained page and opened it for
        // host reads before returning.
        assert_eq!(unsafe { std::ptr::read_volatile(pointer) }, 0);
        assert_eq!(
            source
                .requests
                .lock()
                .expect("request log should not be poisoned")
                .len(),
            1
        );
        bridge.shutdown().expect("test bridge should shut down");
    }

    #[test]
    fn concurrent_guest_handlers_coalesce_source_and_publish_one_permission_union() {
        let _test_lock = MACH_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        let page_size =
            u32::try_from(crate::memory::host_page_size().expect("host page size should resolve"))
                .expect("host page size should fit u32");
        let memory = memory(page_size, 1).expect("test lazy memory should construct");
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let source = Arc::new(BlockingZeroSource {
            requests: AtomicU64::new(0),
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        });
        let bridge = HvfLazyHostFaultBridge::install(
            Arc::clone(&memory),
            Arc::<BlockingZeroSource>::clone(&source),
        )
        .expect("test bridge should install");
        let mapper = Arc::new(ConcurrentProtectionMapper::default());
        let handler = HvfLazyGuestFaultHandler::prepare(
            bridge.resolver(),
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("guest handler should prepare");
        handler.activate().expect("guest handler should activate");
        let exit = HvfExceptionExit {
            syndrome: 0x9381_0007,
            virtual_address: GUEST_BASE,
            physical_address: GUEST_BASE,
        };
        let candidate = handler
            .classify(exit)
            .expect("fault classification should succeed")
            .expect("owned read should classify");

        let first_handler = Arc::clone(&handler);
        let first = thread::spawn(move || first_handler.handle(0, candidate, 0x1000));
        entered_receiver
            .recv()
            .expect("first member should enter the source");
        let second_handler = Arc::clone(&handler);
        let second = thread::spawn(move || second_handler.handle(1, candidate, 0x2000));

        for _ in 0..1_000 {
            if memory.waiter_count().expect("waiter count should resolve") == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            memory.waiter_count().expect("waiter count should resolve"),
            1
        );
        release_sender
            .send(())
            .expect("source release should be sent");
        release_sender
            .send(())
            .expect("defensive duplicate release should be sent");

        let first = first
            .join()
            .expect("first member should join")
            .expect("first member should resolve")
            .expect("first member should be handled");
        let second = second
            .join()
            .expect("second member should join")
            .expect("second member should resolve")
            .expect("second member should be handled");
        assert_eq!(first.fault().access(), HvfLazyGuestAccess::Read);
        assert_eq!(second.fault().access(), HvfLazyGuestAccess::Read);
        assert_eq!(first.permission_changes() + second.permission_changes(), 1);
        assert_eq!(
            usize::from(first.stale_exit()) + usize::from(second.stale_exit()),
            1
        );
        assert_eq!(source.requests.load(Ordering::Relaxed), 1);
        assert_eq!(
            *mapper.protects.lock().expect("protection log should lock"),
            vec![(
                GuestMemoryRange::new(GuestAddress::new(GUEST_BASE), u64::from(page_size))
                    .expect("test page range should be valid"),
                HvfMemoryPermissions::READ,
            )]
        );

        drop(handler);
        bridge.shutdown().expect("test bridge should shut down");
    }

    #[test]
    fn wrong_length_and_source_failure_terminalize_before_permission() {
        let _test_lock = MACH_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        let page_size =
            u32::try_from(crate::memory::host_page_size().expect("host page size should resolve"))
                .expect("host page size should fit u32");
        for reply in [
            TestReply::Data(vec![
                0;
                usize::try_from(page_size)
                    .expect("page size should fit")
                    - 1
            ]),
            TestReply::Failure,
        ] {
            let memory = memory(page_size, 1).expect("test lazy memory should construct");
            let source = Arc::new(TestSource {
                requests: Mutex::new(Vec::new()),
                reply,
            });
            let bridge = HvfLazyHostFaultBridge::install(Arc::clone(&memory), source)
                .expect("test bridge should install");
            assert!(
                bridge
                    .resolver()
                    .resolve_guest_address(GuestAddress::new(GUEST_BASE), PageAccess::Read,)
                    .is_err()
            );
            assert_eq!(
                memory
                    .terminal_reason()
                    .expect("terminal reason should resolve"),
                Some(LazyGuestMemoryTerminalReason::TransitionFailure)
            );
            bridge
                .shutdown()
                .expect("terminal bridge should still restore ownership");
        }
    }

    #[test]
    fn owner_busy_install_rolls_back_candidate_aliases_without_protection() {
        let _test_lock = MACH_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        let page_size =
            u32::try_from(crate::memory::host_page_size().expect("host page size should resolve"))
                .expect("host page size should fit u32");
        let active_memory = memory(page_size, 1).expect("active lazy memory should construct");
        let active_bridge = HvfLazyHostFaultBridge::install(
            active_memory,
            Arc::new(TestSource {
                requests: Mutex::new(Vec::new()),
                reply: TestReply::Zero,
            }),
        )
        .expect("first bridge should install");

        let candidate = memory(page_size, 1).expect("candidate lazy memory should construct");
        let candidate_pointer = candidate.mapping_regions()[0]
            .host_address()
            .as_ptr()
            .cast::<u64>();
        assert!(matches!(
            HvfLazyHostFaultBridge::install(
                Arc::clone(&candidate),
                Arc::new(TestSource {
                    requests: Mutex::new(Vec::new()),
                    reply: TestReply::Zero,
                }),
            ),
            Err(HvfLazyHostFaultError::Platform {
                stage: HvfLazyHostFaultStage::ExceptionOwner
            })
        ));

        // SAFETY: failed installation must have destroyed its aliases without
        // protecting this retained candidate mapping.
        unsafe {
            std::ptr::write_volatile(candidate_pointer, TEST_VALUE.into());
            assert_eq!(
                std::ptr::read_volatile(candidate_pointer),
                u64::from(TEST_VALUE)
            );
        }
        assert_eq!(
            candidate
                .page_state(
                    PagerRegionId::new(1).expect("test region id should validate"),
                    0,
                )
                .expect("candidate page state should resolve"),
            LazyPageState::Absent
        );
        active_bridge
            .shutdown()
            .expect("first bridge should still shut down");
    }

    #[test]
    fn shutdown_waits_for_an_admitted_host_population() {
        struct BlockingSource {
            entered: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
            page: Vec<u8>,
        }

        impl HvfLazyPageSource for BlockingSource {
            fn page(
                &self,
                _request: HvfLazyPageRequest,
            ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
                self.entered
                    .send(())
                    .map_err(|_| HvfLazyPageSourceError::failed())?;
                self.release
                    .lock()
                    .map_err(|_| HvfLazyPageSourceError::failed())?
                    .recv()
                    .map_err(|_| HvfLazyPageSourceError::failed())?;
                Ok(HvfLazyPageContents::data(self.page.clone()))
            }
        }

        let _test_lock = MACH_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        let page_size =
            u32::try_from(crate::memory::host_page_size().expect("host page size should resolve"))
                .expect("host page size should fit u32");
        let memory = memory(page_size, 1).expect("test lazy memory should construct");
        let pointer = memory.mapping_regions()[0].host_address().as_ptr() as usize;
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let source = Arc::new(BlockingSource {
            entered: entered_tx,
            release: Mutex::new(release_rx),
            page: vec![0; usize::try_from(page_size).expect("page size should fit usize")],
        });
        let bridge = HvfLazyHostFaultBridge::install(memory, source)
            .expect("blocking bridge should install");
        let worker = thread::spawn(move || {
            // SAFETY: the retained mapping outlives this worker and its active
            // bridge repairs the host read before the instruction retries.
            unsafe { std::ptr::read_volatile(pointer as *const u8) }
        });
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("source should observe the admitted host fault");

        let (shutdown_started_tx, shutdown_started_rx) = mpsc::channel();
        let (shutdown_done_tx, shutdown_done_rx) = mpsc::channel();
        let shutdown = thread::spawn(move || {
            shutdown_started_tx
                .send(())
                .expect("shutdown start should publish");
            let result = bridge.shutdown();
            shutdown_done_tx
                .send(result)
                .expect("shutdown result should publish");
        });
        shutdown_started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("shutdown worker should start");
        for _ in 0..64 {
            thread::yield_now();
            assert!(
                matches!(shutdown_done_rx.try_recv(), Err(TryRecvError::Empty)),
                "shutdown must not pass an admitted population"
            );
        }

        release_tx
            .send(())
            .expect("blocked source should be released");
        assert_eq!(
            worker.join().expect("faulting worker should join"),
            0,
            "zero page should become readable"
        );
        assert!(
            shutdown_done_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("shutdown should complete after population")
                .expect("shutdown should succeed")
                .prior_handler_restored()
        );
        shutdown.join().expect("shutdown worker should join");
    }

    #[test]
    fn public_diagnostics_redact_fault_authority_and_contents() {
        let request = HvfLazyPageRequest {
            region: PagerRegionId::new(77).expect("test region id should validate"),
            generation: PagerGeneration::new(88).expect("test generation should validate"),
            access: PageAccess::Write,
            offset: 0x1234_0000,
            source_offset: 0x5678_0000,
            length: 16 * 1024,
        };
        assert_eq!(format!("{request:?}"), "HvfLazyPageRequest(<redacted>)");
        assert_eq!(
            format!("{:?}", HvfLazyPageContents::data(vec![0x5a; 32])),
            "HvfLazyPageContents(<redacted>)"
        );
        assert_eq!(
            format!("{:?}", HvfLazyPageContents::zero()),
            "HvfLazyPageContents(<redacted>)"
        );
        assert_eq!(
            format!(
                "{:?}",
                HvfLazyHostFaultError::Source {
                    source: HvfLazyPageSourceError::failed(),
                }
            ),
            "HvfLazyHostFaultError::Source(<redacted>)"
        );

        let _test_lock = MACH_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        let page_size =
            u32::try_from(crate::memory::host_page_size().expect("host page size should resolve"))
                .expect("host page size should fit u32");
        let bridge = HvfLazyHostFaultBridge::install(
            memory(page_size, 1).expect("redaction lazy memory should construct"),
            Arc::new(TestSource {
                requests: Mutex::new(Vec::new()),
                reply: TestReply::Zero,
            }),
        )
        .expect("redaction bridge should install");
        assert_eq!(format!("{bridge:?}"), "HvfLazyHostFaultBridge(<redacted>)");
        assert_eq!(
            format!("{:?}", bridge.resolver()),
            "HvfLazyPageResolver(<redacted>)"
        );
        bridge
            .shutdown()
            .expect("redaction bridge should shut down");
    }
}

#[cfg(all(test, not(all(target_os = "macos", target_arch = "aarch64"))))]
mod unsupported_tests {
    use std::sync::Arc;

    use bangbang_pager::{MAX_FRAME_BYTES, PagerLimits, PagerOperations, PagerRegionId};
    use bangbang_runtime::lazy_memory::{
        LazyGuestMemory, LazyGuestMemoryLimits, LazyGuestMemoryRegion,
    };

    use super::*;

    struct UnusedSource;

    impl HvfLazyPageSource for UnusedSource {
        fn page(
            &self,
            _request: HvfLazyPageRequest,
        ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
            Err(HvfLazyPageSourceError::failed())
        }
    }

    #[test]
    fn public_bridge_reports_the_explicit_unsupported_target() {
        const PAGE_SIZE: u32 = 4 * 1024;
        let pager = PagerLimits::new(
            PAGE_SIZE,
            1,
            1,
            u32::try_from(MAX_FRAME_BYTES).expect("maximum frame size should fit u32"),
            PagerOperations::v1(),
        )
        .expect("unsupported-target pager limits should validate");
        let limits = LazyGuestMemoryLimits::new(pager, 1, 1)
            .expect("unsupported-target lazy limits should validate");
        let range = GuestMemoryRange::new(GuestAddress::new(0x8000_0000), u64::from(PAGE_SIZE))
            .expect("unsupported-target guest range should validate");
        let region = LazyGuestMemoryRegion::new(
            PagerRegionId::new(1).expect("unsupported-target region id should validate"),
            range,
            0,
            PAGE_SIZE,
        )
        .expect("unsupported-target lazy region should validate");
        let memory = Arc::new(
            LazyGuestMemory::new(limits, vec![region])
                .expect("unsupported-target lazy memory should construct"),
        );

        assert!(matches!(
            HvfLazyHostFaultBridge::install(memory, Arc::new(UnusedSource)),
            Err(HvfLazyHostFaultError::UnsupportedTarget)
        ));
    }
}
