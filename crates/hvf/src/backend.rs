use std::sync::Arc;

use bangbang_runtime::memory::{GuestMemory, GuestMemoryRange};
use bangbang_runtime::memory_hotplug::VirtioMemDeviceCaptureState;
use bangbang_runtime::pmem::PreparedPmemDevice;
use bangbang_runtime::{BackendError, VmBackend};

use crate::dirty::{
    HvfDirtyWriteEpochResetError, HvfDirtyWriteTracker, HvfDirtyWriteTrackerStartError,
    HvfDirtyWriteTrackerStopError,
};
use crate::gic::{
    HvfGicCreator, HvfGicError, HvfGicMetadata, HvfGicMsiConfiguration, HvfGicMsiSignaler,
    RealHvfGicCreator,
};
use crate::lazy_guest_fault::HvfLazyGuestFaultHandler;
use crate::lazy_host_fault::{HvfLazyGuestMemoryConsumer, HvfLazyPageResolver};
use crate::memory::{
    HvfGuestMemoryMapping, HvfGuestMemoryMappingError, HvfHostMemoryMapping, HvfMemoryMapper,
    HvfMemoryPermissions, HvfPmemFlushExecutor, HvfVirtioMemMappingCaptureError,
    HvfVirtioMemMappingCaptureState, HvfVirtioMemMutationExecutor, RealHvfMemoryMapper,
};
use crate::runner::{HvfVcpuRunner, HvfVcpuRunnerError};
use crate::topology::{HvfVcpuTopology, HvfVcpuTopologyError};
use crate::vcpu::HvfVcpu;

const VM_NOT_CREATED_FOR_MEMORY_MESSAGE: &str = "VM must be created before mapping guest memory";
const GUEST_MEMORY_ALREADY_MAPPED_MESSAGE: &str = "guest memory is already mapped";
const GUEST_MEMORY_NOT_MAPPED_MESSAGE: &str = "guest memory is not mapped";
const VM_NOT_CREATED_FOR_GIC_MESSAGE: &str = "VM must be created before creating a GIC";
const GIC_ALREADY_CREATED_MESSAGE: &str = "GIC is already created";
const VCPU_TOPOLOGY_ALREADY_STARTED_MESSAGE: &str = "GIC must be created before creating vCPUs";
const GIC_NOT_CREATED_FOR_VCPU_TOPOLOGY_MESSAGE: &str =
    "GIC must be created before starting a vCPU topology";
const VCPU_TOPOLOGY_ALREADY_OWNED_MESSAGE: &str = "vCPU topology has already started";
#[derive(Debug)]
pub struct HvfBackend {
    vm_created: bool,
    guest_memory: Option<HvfGuestMemoryMapping>,
    lazy_guest_fault_handler: Option<Arc<HvfLazyGuestFaultHandler>>,
    lazy_guest_memory_consumer: Option<HvfLazyGuestMemoryConsumer>,
    gic: Option<HvfGicMetadata>,
    gic_msi_signaler: Option<HvfGicMsiSignaler>,
    vcpu_topology_started: bool,
    memory_mapper: Arc<dyn HvfMemoryMapper>,
    gic_creator: Arc<dyn HvfGicCreator>,
}

impl Default for HvfBackend {
    fn default() -> Self {
        Self {
            vm_created: false,
            guest_memory: None,
            lazy_guest_fault_handler: None,
            lazy_guest_memory_consumer: None,
            gic: None,
            gic_msi_signaler: None,
            vcpu_topology_started: false,
            memory_mapper: Arc::new(RealHvfMemoryMapper),
            gic_creator: Arc::new(RealHvfGicCreator),
        }
    }
}

impl HvfBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_supported_target() -> bool {
        cfg!(all(target_os = "macos", target_arch = "aarch64"))
    }

    /// Validates fixed target and framework capabilities required by PCI/MSI.
    pub fn validate_pci_support() -> Result<(), BackendError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
            ));
        }

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            crate::gic::validate_real_gic_msi_support()
                .map_err(|source| BackendError::Hypervisor(source.to_string()))?;
        }
        Ok(())
    }

    pub fn map_guest_memory(
        &mut self,
        memory: GuestMemory,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE).into());
        }

        self.map_guest_memory_with_configured_mapper(memory, permissions)
    }

    /// Map resolver-owned anonymous memory and force first guest accesses to
    /// the shared lazy-page coordinator.
    pub fn map_lazy_guest_memory(
        &mut self,
        resolver: HvfLazyPageResolver,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE).into());
        }

        self.map_lazy_guest_memory_with_configured_mapper(resolver, permissions)
    }

    /// Map protected lazy memory while retaining the only view available to
    /// ordinary in-process device and snapshot consumers.
    pub fn map_lazy_guest_memory_with_consumer(
        &mut self,
        consumer: HvfLazyGuestMemoryConsumer,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE).into());
        }

        self.map_lazy_guest_memory_consumer_with_configured_mapper(consumer, permissions)
    }

    pub(crate) fn map_guest_memory_with_pmem_devices(
        &mut self,
        memory: GuestMemory,
        pmem_devices: &[PreparedPmemDevice],
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE).into());
        }

        self.map_guest_memory_with_pmem_devices_and_configured_mapper(
            memory,
            pmem_devices,
            permissions,
        )
    }

    pub(crate) fn map_runtime_pmem_device(
        &mut self,
        device: &PreparedPmemDevice,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let mapping = pmem_host_memory_mapping(device)?;
        self.guest_memory
            .as_mut()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                "guest memory is not mapped",
            ))?
            .map_runtime_pmem_mapping(mapping)
    }

    pub(crate) fn take_runtime_pmem_mapping(
        &mut self,
        range: bangbang_runtime::memory::GuestMemoryRange,
        flush: bool,
    ) -> Result<HvfHostMemoryMapping, HvfGuestMemoryMappingError> {
        self.guest_memory
            .as_mut()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                "guest memory is not mapped",
            ))?
            .take_runtime_pmem_mapping(range, flush)
    }

    pub(crate) fn restore_runtime_pmem_mapping(
        &mut self,
        mapping: HvfHostMemoryMapping,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.guest_memory
            .as_mut()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                "guest memory is not mapped",
            ))?
            .map_runtime_pmem_mapping(mapping)
    }

    pub fn unmap_guest_memory(&mut self) -> Result<(), HvfGuestMemoryMappingError> {
        if let Some(mapping) = self.guest_memory.as_mut() {
            mapping.unmap_all()?;
        }

        self.guest_memory = None;
        self.lazy_guest_fault_handler = None;
        self.lazy_guest_memory_consumer = None;
        Ok(())
    }

    /// Write-protect every currently mapped writable guest-memory range for
    /// guest-CPU dirty observation.
    ///
    /// This low-level primitive must start before any vCPU owner is created.
    /// It does not enable Firecracker's public dirty-tracking flags or account
    /// for VMM/device writes; those complete epoch semantics are owned by the
    /// higher-level snapshot transaction.
    pub fn start_dirty_write_tracking(
        &mut self,
    ) -> Result<Arc<HvfDirtyWriteTracker>, HvfDirtyWriteTrackerStartError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE).into());
        }
        if !self.vm_created {
            return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "VM must be created before dirty-write tracking starts",
            ));
        }
        if self.vcpu_topology_started {
            return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "dirty-write tracking must start before vCPU ownership",
            ));
        }
        if self.lazy_guest_fault_handler.is_some() {
            return Err(HvfDirtyWriteTrackerStartError::InvalidState(
                "dirty-write tracking is unavailable for lazy guest memory",
            ));
        }
        self.guest_memory
            .as_mut()
            .ok_or(HvfDirtyWriteTrackerStartError::InvalidState(
                GUEST_MEMORY_NOT_MAPPED_MESSAGE,
            ))?
            .start_dirty_write_tracking()
    }

    /// Restore write permission after every vCPU owner has shut down.
    pub fn stop_dirty_write_tracking(&mut self) -> Result<(), HvfDirtyWriteTrackerStopError> {
        let Some(mapping) = self.guest_memory.as_mut() else {
            return Ok(());
        };
        mapping.stop_dirty_write_tracking()
    }

    pub(crate) fn reset_dirty_epoch_quiesced(
        &mut self,
    ) -> Result<Option<u64>, HvfDirtyWriteEpochResetError> {
        let Some(mapping) = self.guest_memory.as_mut() else {
            return Ok(None);
        };
        mapping.reset_dirty_epoch_quiesced()
    }

    pub(crate) fn mapped_guest_memory(&self) -> Result<&GuestMemory, HvfGuestMemoryMappingError> {
        if let Some(consumer) = &self.lazy_guest_memory_consumer {
            return Ok(consumer.memory());
        }
        self.guest_memory
            .as_ref()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                GUEST_MEMORY_NOT_MAPPED_MESSAGE,
            ))?
            .memory()
    }

    pub(crate) fn mapped_guest_memory_for_public_access(
        &self,
    ) -> Result<&GuestMemory, HvfGuestMemoryMappingError> {
        if self.lazy_guest_memory_consumer.is_some() {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                "direct guest-memory borrowing is unavailable for lazy guest memory",
            ));
        }
        self.mapped_guest_memory()
    }

    pub(crate) fn capture_virtio_mem_mapping_state(
        &self,
        device: &VirtioMemDeviceCaptureState,
    ) -> Result<HvfVirtioMemMappingCaptureState, HvfVirtioMemMappingCaptureError> {
        self.guest_memory
            .as_ref()
            .ok_or(HvfVirtioMemMappingCaptureError::GuestMemoryUnavailable)?
            .capture_virtio_mem_mapping_state(device)
    }

    pub(crate) fn mapped_guest_memory_mut(
        &mut self,
    ) -> Result<&mut GuestMemory, HvfGuestMemoryMappingError> {
        if let Some(consumer) = &mut self.lazy_guest_memory_consumer {
            return Ok(consumer.memory_mut());
        }
        self.guest_memory
            .as_mut()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                GUEST_MEMORY_NOT_MAPPED_MESSAGE,
            ))?
            .memory_mut()
    }

    pub(crate) fn mapped_guest_memory_for_public_access_mut(
        &mut self,
    ) -> Result<&mut GuestMemory, HvfGuestMemoryMappingError> {
        if self.lazy_guest_memory_consumer.is_some() {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                "direct guest-memory borrowing is unavailable for lazy guest memory",
            ));
        }
        self.mapped_guest_memory_mut()
    }

    pub(crate) fn mapped_guest_memory_and_virtio_mem_executor_mut(
        &mut self,
        permissions: HvfMemoryPermissions,
    ) -> Result<(&mut GuestMemory, HvfVirtioMemMutationExecutor<'_>), HvfGuestMemoryMappingError>
    {
        let mapping =
            self.guest_memory
                .as_mut()
                .ok_or(HvfGuestMemoryMappingError::InvalidState(
                    GUEST_MEMORY_NOT_MAPPED_MESSAGE,
                ))?;
        if let Some(consumer) = &mut self.lazy_guest_memory_consumer {
            return Ok((
                consumer.memory_mut(),
                mapping.virtio_mem_executor_mut(permissions),
            ));
        }
        mapping.memory_and_virtio_mem_executor_mut(permissions)
    }

    pub(crate) fn mapped_guest_memory_and_pmem_flush_executor_mut(
        &mut self,
    ) -> Result<(&mut GuestMemory, HvfPmemFlushExecutor<'_>), HvfGuestMemoryMappingError> {
        let mapping =
            self.guest_memory
                .as_mut()
                .ok_or(HvfGuestMemoryMappingError::InvalidState(
                    GUEST_MEMORY_NOT_MAPPED_MESSAGE,
                ))?;
        if let Some(consumer) = &mut self.lazy_guest_memory_consumer {
            return Ok((consumer.memory_mut(), mapping.pmem_flush_executor()));
        }
        mapping.memory_and_pmem_flush_executor_mut()
    }

    pub(crate) fn cancel_lazy_page_source_on_drop(&mut self) {
        if let Some(consumer) = &mut self.lazy_guest_memory_consumer {
            consumer.cancel_source_on_drop();
        }
    }

    pub(crate) fn shutdown_lazy_page_source_on_drop(&mut self) {
        if let Some(consumer) = &mut self.lazy_guest_memory_consumer {
            consumer.shutdown_source_on_drop();
        }
    }

    /// Insert one owned guest memory region and map it into the active HVF VM.
    ///
    /// This keeps the process-owned `GuestMemory` region and the HVF mapping in
    /// one failure-atomic backend operation. It does not update virtio-mem
    /// plugged-block state or Firecracker-facing memory hotplug status.
    pub fn map_dynamic_guest_memory_region(
        &mut self,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE).into());
        }

        self.map_dynamic_guest_memory_region_with_configured_mapper(range, permissions)
    }

    fn map_dynamic_guest_memory_region_with_configured_mapper(
        &mut self,
        range: GuestMemoryRange,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.guest_memory
            .as_mut()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                GUEST_MEMORY_NOT_MAPPED_MESSAGE,
            ))?
            .map_dynamic_region(range, permissions)
    }

    /// Unmap one dynamically mapped guest memory region and drop its owner.
    ///
    /// Only regions added through `map_dynamic_guest_memory_region` may be
    /// removed this way. Startup DRAM and host-backed mappings remain owned by
    /// the full guest-memory unmap path.
    pub fn unmap_dynamic_guest_memory_region(
        &mut self,
        range: GuestMemoryRange,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.unmap_dynamic_guest_memory_region_with_configured_mapper(range)
    }

    fn unmap_dynamic_guest_memory_region_with_configured_mapper(
        &mut self,
        range: GuestMemoryRange,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.guest_memory
            .as_mut()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                GUEST_MEMORY_NOT_MAPPED_MESSAGE,
            ))?
            .unmap_dynamic_region(range)
    }

    pub fn create_gic(&mut self) -> Result<&HvfGicMetadata, HvfGicError> {
        if !Self::is_supported_target() {
            return Err(HvfGicError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
            ));
        }

        self.create_gic_with_configured_creator(None)
    }

    /// Create a GIC with one demand-sized, message-only SPI range.
    ///
    /// The ordinary [`Self::create_gic`] path remains MSI-free. Callers must
    /// select this before creating any vCPU.
    pub fn create_gic_with_msi(
        &mut self,
        configuration: HvfGicMsiConfiguration,
    ) -> Result<&HvfGicMetadata, HvfGicError> {
        if !Self::is_supported_target() {
            return Err(HvfGicError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
            ));
        }

        self.create_gic_with_configured_creator(Some(configuration))
    }

    pub fn gic_metadata(&self) -> Option<&HvfGicMetadata> {
        self.gic.as_ref()
    }

    /// Return the send-only MSI capability retained by an MSI-enabled GIC.
    pub fn gic_msi_signaler(&self) -> Option<&HvfGicMsiSignaler> {
        self.gic_msi_signaler.as_ref()
    }

    pub(crate) const fn has_created_vm(&self) -> bool {
        self.vm_created
    }

    #[cfg(test)]
    fn has_guest_memory_mapping(&self) -> bool {
        self.guest_memory
            .as_ref()
            .is_some_and(HvfGuestMemoryMapping::has_mapped_regions)
    }

    pub fn create_vcpu(&mut self) -> Result<HvfVcpu<'_>, BackendError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
            ));
        }

        if !self.vm_created {
            return Err(BackendError::InvalidState(
                "VM must be created before creating a vCPU",
            ));
        }
        if self.active_dirty_write_tracker()?.is_some() {
            return Err(BackendError::InvalidState(
                "raw vCPU ownership is unavailable while dirty-write tracking is active",
            ));
        }
        if self.lazy_guest_fault_handler.is_some() {
            return Err(BackendError::InvalidState(
                "raw vCPU ownership is unavailable while lazy guest paging is active",
            ));
        }

        let vcpu = HvfVcpu::new()?;
        self.vcpu_topology_started = true;
        Ok(vcpu)
    }

    pub fn start_vcpu_runner(&mut self) -> Result<HvfVcpuRunner<'_>, HvfVcpuRunnerError> {
        self.validate_vcpu_runner_start()?;

        let tracker = self
            .active_dirty_write_tracker()
            .map_err(HvfVcpuRunnerError::Backend)?;
        let lazy_handler = self
            .active_lazy_guest_fault_handler()
            .map_err(HvfVcpuRunnerError::Backend)?;
        let runner = HvfVcpuRunner::new_with_memory_fault_handlers(0, tracker, lazy_handler)?;
        self.vcpu_topology_started = true;
        Ok(runner)
    }

    pub(crate) fn start_session_vcpu_runner<'vm>(
        &mut self,
    ) -> Result<HvfVcpuRunner<'vm>, HvfVcpuRunnerError> {
        // The session object holds the backend borrow separately; keep this
        // constructor crate-private so arbitrary callers cannot outlive the VM.
        self.validate_vcpu_runner_start()?;

        let tracker = self
            .active_dirty_write_tracker()
            .map_err(HvfVcpuRunnerError::Backend)?;
        let lazy_handler = self
            .active_lazy_guest_fault_handler()
            .map_err(HvfVcpuRunnerError::Backend)?;
        let runner = HvfVcpuRunner::new_with_memory_fault_handlers(0, tracker, lazy_handler)?;
        self.vcpu_topology_started = true;
        Ok(runner)
    }

    pub(crate) fn start_session_vcpu_topology<'vm>(
        &mut self,
        vcpu_count: u8,
    ) -> Result<HvfVcpuTopology<'vm>, HvfVcpuTopologyError> {
        // The session object holds the backend borrow separately; keep this
        // constructor crate-private so arbitrary callers cannot outlive the VM.
        self.validate_vcpu_topology_start()?;

        let tracker = self.active_dirty_write_tracker()?;
        let lazy_handler = self.active_lazy_guest_fault_handler()?;
        let topology = HvfVcpuTopology::create(vcpu_count, tracker, lazy_handler)?;
        self.vcpu_topology_started = true;
        Ok(topology)
    }

    /// Start an ordered set of permanent owner-thread vCPUs for this VM/GIC.
    ///
    /// This internal compatibility prerequisite does not activate multi-vCPU
    /// boot. All runners remain idle until callers explicitly issue commands.
    pub fn start_vcpu_topology(
        &mut self,
        vcpu_count: u8,
    ) -> Result<HvfVcpuTopology<'_>, HvfVcpuTopologyError> {
        self.validate_vcpu_topology_start()?;

        let tracker = self.active_dirty_write_tracker()?;
        let lazy_handler = self.active_lazy_guest_fault_handler()?;
        let topology = HvfVcpuTopology::create(vcpu_count, tracker, lazy_handler)?;
        self.vcpu_topology_started = true;
        Ok(topology)
    }

    fn validate_vcpu_runner_start(&self) -> Result<(), HvfVcpuRunnerError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE).into());
        }

        if !self.vm_created {
            return Err(BackendError::InvalidState(
                "VM must be created before starting a vCPU runner",
            )
            .into());
        }
        if let Some(handler) = &self.lazy_guest_fault_handler {
            handler
                .ensure_active()
                .map_err(HvfVcpuRunnerError::Backend)?;
        }

        Ok(())
    }

    fn active_dirty_write_tracker(
        &self,
    ) -> Result<Option<Arc<HvfDirtyWriteTracker>>, BackendError> {
        self.guest_memory
            .as_ref()
            .map_or(Ok(None), HvfGuestMemoryMapping::active_dirty_write_tracker)
    }

    fn active_lazy_guest_fault_handler(
        &self,
    ) -> Result<Option<Arc<HvfLazyGuestFaultHandler>>, BackendError> {
        if let Some(handler) = &self.lazy_guest_fault_handler {
            handler.ensure_active()?;
            Ok(Some(Arc::clone(handler)))
        } else {
            Ok(None)
        }
    }

    fn validate_vcpu_topology_start(&self) -> Result<(), HvfVcpuTopologyError> {
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE).into());
        }
        if !self.vm_created {
            return Err(BackendError::InvalidState(
                "VM must be created before starting a vCPU topology",
            )
            .into());
        }
        if self.gic.is_none() {
            return Err(
                BackendError::InvalidState(GIC_NOT_CREATED_FOR_VCPU_TOPOLOGY_MESSAGE).into(),
            );
        }
        if self.vcpu_topology_started {
            return Err(BackendError::InvalidState(VCPU_TOPOLOGY_ALREADY_OWNED_MESSAGE).into());
        }

        Ok(())
    }

    fn create_gic_with_configured_creator(
        &mut self,
        msi: Option<HvfGicMsiConfiguration>,
    ) -> Result<&HvfGicMetadata, HvfGicError> {
        if !self.vm_created {
            return Err(HvfGicError::InvalidState(VM_NOT_CREATED_FOR_GIC_MESSAGE));
        }

        if self.gic.is_some() {
            return Err(HvfGicError::InvalidState(GIC_ALREADY_CREATED_MESSAGE));
        }

        if self.vcpu_topology_started {
            return Err(HvfGicError::InvalidState(
                VCPU_TOPOLOGY_ALREADY_STARTED_MESSAGE,
            ));
        }

        let requested_msi = msi.is_some();
        let created = self.gic_creator.create_gic(msi)?;
        if created.metadata.msi.is_some() != requested_msi
            || created.msi_signaler.is_some() != requested_msi
        {
            return Err(HvfGicError::InvalidState(
                "GIC MSI request, metadata, and send capability disagree",
            ));
        }
        self.gic = Some(created.metadata);
        self.gic_msi_signaler = created.msi_signaler;

        self.gic
            .as_ref()
            .ok_or(HvfGicError::InvalidState("created GIC metadata is missing"))
    }

    fn clear_vm_owned_state(&mut self) {
        if let Some(signaler) = &self.gic_msi_signaler {
            signaler.deactivate();
        }
        self.gic = None;
        self.gic_msi_signaler = None;
        self.vcpu_topology_started = false;
    }

    fn map_guest_memory_with_configured_mapper(
        &mut self,
        memory: GuestMemory,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.map_guest_memory_with_host_mappings(memory, permissions, Vec::new())
    }

    fn map_lazy_guest_memory_with_configured_mapper(
        &mut self,
        resolver: HvfLazyPageResolver,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.map_lazy_guest_memory_with_optional_consumer(resolver, None, permissions)
    }

    fn map_lazy_guest_memory_consumer_with_configured_mapper(
        &mut self,
        consumer: HvfLazyGuestMemoryConsumer,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        let resolver = consumer.resolver();
        self.map_lazy_guest_memory_with_optional_consumer(resolver, Some(consumer), permissions)
    }

    fn map_lazy_guest_memory_with_optional_consumer(
        &mut self,
        resolver: HvfLazyPageResolver,
        mut consumer: Option<HvfLazyGuestMemoryConsumer>,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.validate_lazy_guest_memory_mapping_state()?;
        let handler = HvfLazyGuestFaultHandler::prepare(
            resolver.clone(),
            permissions,
            Arc::clone(&self.memory_mapper),
        )?;
        let result = HvfGuestMemoryMapping::map_lazy_with_mapper(
            resolver.mapping_regions(),
            permissions,
            Arc::clone(&self.memory_mapper),
        );
        match result {
            Ok(mapping) => {
                if let Err(error) = handler.activate() {
                    handler.poison();
                    self.guest_memory = Some(mapping);
                    self.lazy_guest_fault_handler = Some(handler);
                    self.lazy_guest_memory_consumer = consumer.take();
                    return Err(error);
                }
                if resolver.bind_guest_fault_handler(&handler).is_err() {
                    handler.poison();
                    self.guest_memory = Some(mapping);
                    self.lazy_guest_fault_handler = Some(handler);
                    self.lazy_guest_memory_consumer = consumer.take();
                    return Err(HvfGuestMemoryMappingError::InvalidState(
                        "lazy guest fault handler binding failed",
                    ));
                }
                self.guest_memory = Some(mapping);
                self.lazy_guest_fault_handler = Some(handler);
                self.lazy_guest_memory_consumer = consumer.take();
                Ok(())
            }
            Err(failed_mapping) => {
                handler.poison();
                if failed_mapping.mapping.has_mapped_regions() {
                    self.guest_memory = Some(failed_mapping.mapping);
                    self.lazy_guest_fault_handler = Some(handler);
                    self.lazy_guest_memory_consumer = consumer.take();
                }
                Err(failed_mapping.error)
            }
        }
    }

    fn validate_lazy_guest_memory_mapping_state(&self) -> Result<(), HvfGuestMemoryMappingError> {
        self.validate_guest_memory_mapping_state()?;
        if self.vcpu_topology_started {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                "lazy guest memory must map before vCPU ownership",
            ));
        }
        Ok(())
    }

    fn map_guest_memory_with_pmem_devices_and_configured_mapper(
        &mut self,
        memory: GuestMemory,
        pmem_devices: &[PreparedPmemDevice],
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.validate_guest_memory_mapping_state()?;
        HvfGuestMemoryMapping::validate_guest_memory(&memory, permissions)?;
        let host_mappings = pmem_host_memory_mappings(pmem_devices)?;
        self.map_guest_memory_with_host_mappings(memory, permissions, host_mappings)
    }

    fn map_guest_memory_with_host_mappings(
        &mut self,
        memory: GuestMemory,
        permissions: HvfMemoryPermissions,
        host_mappings: Vec<HvfHostMemoryMapping>,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.validate_guest_memory_mapping_state()?;

        match HvfGuestMemoryMapping::map_with_mapper_and_host_mappings(
            memory,
            permissions,
            host_mappings,
            Arc::clone(&self.memory_mapper),
        ) {
            Ok(mapping) => {
                self.guest_memory = Some(mapping);
                Ok(())
            }
            Err(failed_mapping) => {
                if failed_mapping.mapping.has_mapped_regions() {
                    self.guest_memory = Some(failed_mapping.mapping);
                }

                Err(failed_mapping.error)
            }
        }
    }

    fn validate_guest_memory_mapping_state(&self) -> Result<(), HvfGuestMemoryMappingError> {
        if !self.vm_created {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                VM_NOT_CREATED_FOR_MEMORY_MESSAGE,
            ));
        }

        if self.guest_memory.is_some() {
            return Err(HvfGuestMemoryMappingError::InvalidState(
                GUEST_MEMORY_ALREADY_MAPPED_MESSAGE,
            ));
        }

        Ok(())
    }

    #[cfg(test)]
    fn new_with_memory_mapper(memory_mapper: Arc<dyn HvfMemoryMapper>) -> Self {
        Self {
            vm_created: false,
            guest_memory: None,
            lazy_guest_fault_handler: None,
            lazy_guest_memory_consumer: None,
            gic: None,
            gic_msi_signaler: None,
            vcpu_topology_started: false,
            memory_mapper,
            gic_creator: Arc::new(RealHvfGicCreator),
        }
    }

    #[cfg(test)]
    fn new_with_gic_creator(gic_creator: Arc<dyn HvfGicCreator>) -> Self {
        Self {
            vm_created: false,
            guest_memory: None,
            lazy_guest_fault_handler: None,
            lazy_guest_memory_consumer: None,
            gic: None,
            gic_msi_signaler: None,
            vcpu_topology_started: false,
            memory_mapper: Arc::new(RealHvfMemoryMapper),
            gic_creator,
        }
    }
}

fn pmem_host_memory_mappings(
    pmem_devices: &[PreparedPmemDevice],
) -> Result<Vec<HvfHostMemoryMapping>, HvfGuestMemoryMappingError> {
    let mut host_mappings = Vec::new();
    host_mappings
        .try_reserve_exact(pmem_devices.len())
        .map_err(|source| HvfGuestMemoryMappingError::MappingMetadataAllocationFailed { source })?;

    for device in pmem_devices {
        host_mappings.push(pmem_host_memory_mapping(device)?);
    }

    Ok(host_mappings)
}

fn pmem_host_memory_mapping(
    device: &PreparedPmemDevice,
) -> Result<HvfHostMemoryMapping, HvfGuestMemoryMappingError> {
    Ok(HvfHostMemoryMapping::new_pmem(
        pmem_mapping_label(device),
        device.guest_range(),
        device.mapping().clone(),
        pmem_memory_permissions(device.mapping().is_read_only()),
    ))
}

fn pmem_memory_permissions(read_only: bool) -> HvfMemoryPermissions {
    if read_only {
        HvfMemoryPermissions::READ
    } else {
        HvfMemoryPermissions::new(true, true, false)
    }
}

fn pmem_mapping_label(device: &PreparedPmemDevice) -> String {
    format!("pmem device `{}`", device.id())
}

impl VmBackend for HvfBackend {
    fn create_vm(&mut self) -> Result<(), BackendError> {
        if self.vm_created {
            return Ok(());
        }

        crate::ffi::create_vm()?;
        self.vm_created = true;
        Ok(())
    }

    fn destroy_vm(&mut self) -> Result<(), BackendError> {
        if self.vm_created {
            if let Some(signaler) = &self.gic_msi_signaler {
                signaler.deactivate();
            }
            self.unmap_guest_memory()
                .map_err(|err| BackendError::Hypervisor(err.to_string()))?;
            crate::ffi::destroy_vm()?;
            self.vm_created = false;
            self.clear_vm_owned_state();
        }
        Ok(())
    }
}

impl Drop for HvfBackend {
    fn drop(&mut self) {
        if self.vm_created {
            let mut mapping_after_failed_unmap = None;
            let lazy_handler = self.lazy_guest_fault_handler.take();
            let lazy_consumer = self.lazy_guest_memory_consumer.take();

            if let Some(signaler) = &self.gic_msi_signaler {
                signaler.deactivate();
            }
            if let Some(mut mapping) = self.guest_memory.take()
                && mapping.unmap_all().is_err()
                && mapping.has_mapped_regions()
            {
                mapping_after_failed_unmap = Some(mapping);
            }
            let vm_destroyed = crate::ffi::destroy_vm().is_ok();
            self.vm_created = false;
            self.clear_vm_owned_state();

            if vm_destroyed && let Some(mapping) = mapping_after_failed_unmap.take() {
                mapping.release_after_vm_destroy();
            }
            if !vm_destroyed && mapping_after_failed_unmap.is_some() {
                // The VM may still retain stage-two references after both
                // unmap and destruction fail. Preserve every host and fault
                // owner rather than releasing memory that HVF may still use.
                std::mem::forget(mapping_after_failed_unmap);
                std::mem::forget(lazy_handler);
                std::mem::forget(lazy_consumer);
                return;
            }
            drop(mapping_after_failed_unmap);
            drop(lazy_handler);
            drop(lazy_consumer);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::num::NonZeroU32;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use bangbang_pager::{MAX_FRAME_BYTES, PagerLimits, PagerOperations, PagerRegionId};
    use bangbang_runtime::BackendError;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use bangbang_runtime::lazy_memory::{
        LazyGuestMemory, LazyGuestMemoryLimits, LazyGuestMemoryRegion,
    };
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::pmem::{
        PmemConfig, PmemConfigInput, PmemConfigs, PreparedPmemDevice, PreparedPmemDevices,
        VIRTIO_PMEM_ALIGNMENT,
    };

    use super::HvfBackend;
    use crate::lazy_guest_fault::HvfLazyGuestFaultHandler;
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use crate::lazy_host_fault::{
        HvfLazyGuestMemoryConsumer, HvfLazyHostFaultBridge, HvfLazyPageContents,
        HvfLazyPageRequest, HvfLazyPageSource, HvfLazyPageSourceError, MACH_LAZY_TEST_LOCK,
    };
    use crate::memory::{
        HvfGuestMemoryMapping, HvfGuestMemoryMappingError, HvfMappedGuestMemoryRegion,
        HvfMemoryMapRequest, HvfMemoryMapper, HvfMemoryPermissions, host_page_size,
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

    fn page_size() -> u64 {
        host_page_size().expect("host page size should be available for tests")
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    struct ZeroLazyPageSource;

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    impl HvfLazyPageSource for ZeroLazyPageSource {
        fn page(
            &self,
            _request: HvfLazyPageRequest,
        ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
            Ok(HvfLazyPageContents::zero())
        }
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn lazy_consumer(region_count: u16) -> HvfLazyGuestMemoryConsumer {
        let page_size =
            u32::try_from(page_size()).expect("host page size should fit the pager protocol");
        let pager = PagerLimits::new(
            page_size,
            region_count,
            region_count,
            u32::try_from(MAX_FRAME_BYTES).expect("maximum frame size should fit u32"),
            PagerOperations::v1(),
        )
        .expect("test pager limits should validate");
        let limits = LazyGuestMemoryLimits::new(pager, u64::from(region_count), 8)
            .expect("test lazy-memory limits should validate");
        let mut regions = Vec::new();
        for index in 0..region_count {
            let offset = u64::from(index)
                .checked_mul(u64::from(page_size))
                .expect("test region offset should fit");
            regions.push(
                LazyGuestMemoryRegion::new(
                    PagerRegionId::new(u32::from(index) + 1)
                        .expect("test region identity should be nonzero"),
                    range(0x8000_0000 + offset, u64::from(page_size)),
                    offset,
                    page_size,
                )
                .expect("test lazy region should validate"),
            );
        }
        let memory = Arc::new(
            LazyGuestMemory::new(limits, regions).expect("test lazy memory should construct"),
        );
        HvfLazyHostFaultBridge::install(memory, Arc::new(ZeroLazyPageSource))
            .expect("test lazy host bridge should install")
            .into_guest_memory_consumer()
            .expect("test lazy consumer should claim once")
    }

    fn pmem_config(input: PmemConfigInput) -> PmemConfig {
        PmemConfig::try_from(input).expect("pmem config should be valid for test")
    }

    fn gic_metadata() -> crate::gic::HvfGicMetadata {
        crate::gic::HvfGicMetadata {
            distributor: crate::gic::HvfGicRegion {
                base: 0x3fff_0000,
                size: 0x1_0000,
            },
            redistributor: crate::gic::HvfGicRedistributor {
                region: crate::gic::HvfGicRegion {
                    base: 0x3ffd_0000,
                    size: 0x2_0000,
                },
                single_redistributor_size: 0x2_0000,
            },
            spi_interrupt_range: crate::gic::HvfGicInterruptRange {
                base: 32,
                count: 96,
            },
            timer_interrupts: crate::gic::HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 27,
                el1_physical_timer_intid: 30,
            },
            msi: None,
        }
    }

    fn gic_metadata_with_msi() -> crate::gic::HvfGicMetadata {
        crate::gic::HvfGicMetadata {
            spi_interrupt_range: crate::gic::HvfGicInterruptRange {
                base: 32,
                count: 95,
            },
            msi: Some(crate::gic::HvfGicMsiMetadata {
                region: crate::gic::HvfGicRegion {
                    base: 0x3ffc_0000,
                    size: 0x1_0000,
                },
                interrupt_range: crate::gic::HvfGicInterruptRange {
                    base: 127,
                    count: 1,
                },
            }),
            ..gic_metadata()
        }
    }

    #[test]
    fn supported_target_matches_compile_target() {
        assert_eq!(
            HvfBackend::is_supported_target(),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
    }

    #[test]
    fn dirty_write_tracking_before_vm_reports_state_or_target_error() {
        let mut backend = HvfBackend::new();
        let error = backend
            .start_dirty_write_tracking()
            .expect_err("dirty-write tracking should require a supported live VM");

        if HvfBackend::is_supported_target() {
            assert_eq!(
                error,
                crate::dirty::HvfDirtyWriteTrackerStartError::InvalidState(
                    "VM must be created before dirty-write tracking starts"
                )
            );
        } else {
            assert_eq!(
                error,
                crate::dirty::HvfDirtyWriteTrackerStartError::Backend(BackendError::Unsupported(
                    crate::ffi::UNSUPPORTED_TARGET_MESSAGE
                ))
            );
        }
    }

    #[test]
    fn dirty_write_tracking_requires_mapping_and_precedes_vcpu_ownership() {
        if !HvfBackend::is_supported_target() {
            return;
        }

        let mut backend = HvfBackend::new();
        backend.vm_created = true;
        assert_eq!(
            backend
                .start_dirty_write_tracking()
                .expect_err("dirty-write tracking should require mapped RAM"),
            crate::dirty::HvfDirtyWriteTrackerStartError::InvalidState(
                super::GUEST_MEMORY_NOT_MAPPED_MESSAGE
            )
        );

        backend.vcpu_topology_started = true;
        assert_eq!(
            backend
                .start_dirty_write_tracking()
                .expect_err("vCPU ownership should fail before mapping lookup"),
            crate::dirty::HvfDirtyWriteTrackerStartError::InvalidState(
                "dirty-write tracking must start before vCPU ownership"
            )
        );
    }

    #[test]
    fn active_dirty_write_tracking_blocks_raw_vcpu_and_stops_idempotently() {
        if !HvfBackend::is_supported_target() {
            return;
        }

        let page_size = page_size();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper);
        backend.vm_created = true;
        backend
            .map_guest_memory_with_configured_mapper(
                memory_for_ranges(vec![range(0, page_size)]),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("test RAM should map");
        let tracker = backend
            .start_dirty_write_tracking()
            .expect("tracker should start before vCPU ownership");

        assert!(tracker.is_active().expect("tracker query should succeed"));
        assert_eq!(
            backend
                .create_vcpu()
                .expect_err("raw vCPU must not bypass an active tracker"),
            BackendError::InvalidState(
                "raw vCPU ownership is unavailable while dirty-write tracking is active"
            )
        );
        backend
            .stop_dirty_write_tracking()
            .expect("owner-free tracker should stop");
        backend
            .stop_dirty_write_tracking()
            .expect("tracker stop should be idempotent");
        assert!(!tracker.is_active().expect("stopped tracker should query"));
        backend.unmap_guest_memory().expect("test RAM should unmap");
    }

    #[test]
    fn lazy_mapping_blocks_dirty_and_raw_ownership_until_unmapped() {
        if !HvfBackend::is_supported_target() {
            return;
        }

        let page_size = page_size();
        let guest_range = range(0, page_size);
        let memory = memory_for_ranges(vec![guest_range]);
        let mapper = Arc::new(RecordingMapper::default());
        let mapping = HvfGuestMemoryMapping::map_lazy_with_mapper(
            memory.regions(),
            HvfMemoryPermissions::GUEST_RAM,
            mapper.clone(),
        )
        .expect("test lazy mapping should zero-protect");
        let handler = HvfLazyGuestFaultHandler::active_noop_for_test(
            mapper.clone(),
            &[guest_range],
            page_size,
            HvfMemoryPermissions::GUEST_RAM,
        );
        let mut backend = HvfBackend::new_with_memory_mapper(mapper);
        backend.vm_created = true;
        backend.guest_memory = Some(mapping);
        backend.lazy_guest_fault_handler = Some(handler);

        assert_eq!(
            backend
                .start_dirty_write_tracking()
                .expect_err("dirty tracking must not bypass lazy WRITE ownership"),
            crate::dirty::HvfDirtyWriteTrackerStartError::InvalidState(
                "dirty-write tracking is unavailable for lazy guest memory"
            )
        );
        assert_eq!(
            backend
                .create_vcpu()
                .expect_err("raw vCPU must not bypass lazy fault handling"),
            BackendError::InvalidState(
                "raw vCPU ownership is unavailable while lazy guest paging is active"
            )
        );

        backend
            .unmap_guest_memory()
            .expect("lazy mapping should unmap");
        assert!(backend.guest_memory.is_none());
        assert!(backend.lazy_guest_fault_handler.is_none());
    }

    #[test]
    fn lazy_mapping_state_rejects_every_post_vcpu_attempt() {
        let mut backend = HvfBackend::default();
        backend.vm_created = true;
        backend.vcpu_topology_started = true;

        assert!(matches!(
            backend.validate_lazy_guest_memory_mapping_state(),
            Err(HvfGuestMemoryMappingError::InvalidState(
                "lazy guest memory must map before vCPU ownership"
            ))
        ));
        assert!(backend.guest_memory.is_none());
        assert!(backend.lazy_guest_fault_handler.is_none());
    }

    #[test]
    fn create_vcpu_before_vm_reports_state_or_target_error() {
        let mut backend = HvfBackend::new();
        let err = backend
            .create_vcpu()
            .expect_err("creating a vCPU before VM creation should fail");

        if HvfBackend::is_supported_target() {
            assert_eq!(
                err,
                BackendError::InvalidState("VM must be created before creating a vCPU")
            );
        } else {
            assert_eq!(
                err,
                BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE)
            );
        }
    }

    #[test]
    fn start_vcpu_runner_before_vm_reports_state_or_target_error() {
        let mut backend = HvfBackend::new();
        let err = backend
            .start_vcpu_runner()
            .expect_err("starting a vCPU runner before VM creation should fail");

        if HvfBackend::is_supported_target() {
            assert_eq!(
                err,
                crate::runner::HvfVcpuRunnerError::Backend(BackendError::InvalidState(
                    "VM must be created before starting a vCPU runner"
                ))
            );
        } else {
            assert_eq!(
                err,
                crate::runner::HvfVcpuRunnerError::Backend(BackendError::Unsupported(
                    crate::ffi::UNSUPPORTED_TARGET_MESSAGE
                ))
            );
        }
    }

    #[test]
    fn start_vcpu_topology_before_vm_reports_state_or_target_error() {
        let mut backend = HvfBackend::new();
        let err = backend
            .start_vcpu_topology(2)
            .expect_err("starting a topology before VM creation should fail");

        if HvfBackend::is_supported_target() {
            assert_eq!(
                err,
                crate::topology::HvfVcpuTopologyError::Backend(BackendError::InvalidState(
                    "VM must be created before starting a vCPU topology"
                ))
            );
        } else {
            assert_eq!(
                err,
                crate::topology::HvfVcpuTopologyError::Backend(BackendError::Unsupported(
                    crate::ffi::UNSUPPORTED_TARGET_MESSAGE
                ))
            );
        }
        assert!(!backend.vcpu_topology_started);
    }

    #[test]
    fn start_vcpu_topology_requires_gic_before_count_validation() {
        if !HvfBackend::is_supported_target() {
            return;
        }

        let mut backend = HvfBackend::new();
        backend.vm_created = true;

        assert_eq!(
            backend
                .start_vcpu_topology(0)
                .expect_err("missing GIC should fail first"),
            crate::topology::HvfVcpuTopologyError::Backend(BackendError::InvalidState(
                super::GIC_NOT_CREATED_FOR_VCPU_TOPOLOGY_MESSAGE
            ))
        );
        assert!(!backend.vcpu_topology_started);
    }

    #[test]
    fn failed_topology_validation_does_not_publish_backend_state() {
        if !HvfBackend::is_supported_target() {
            return;
        }

        let mut backend = HvfBackend::new();
        backend.vm_created = true;
        backend.gic = Some(gic_metadata());

        assert_eq!(
            backend
                .start_vcpu_topology(0)
                .expect_err("zero topology should fail"),
            crate::topology::HvfVcpuTopologyError::InvalidVcpuCount {
                requested: 0,
                max: 32,
            }
        );
        assert!(!backend.vcpu_topology_started);
    }

    #[test]
    fn duplicate_topology_start_is_rejected_before_count_validation() {
        if !HvfBackend::is_supported_target() {
            return;
        }

        let mut backend = HvfBackend::new();
        backend.vm_created = true;
        backend.gic = Some(gic_metadata());
        backend.vcpu_topology_started = true;

        assert_eq!(
            backend
                .start_vcpu_topology(0)
                .expect_err("duplicate topology should fail first"),
            crate::topology::HvfVcpuTopologyError::Backend(BackendError::InvalidState(
                super::VCPU_TOPOLOGY_ALREADY_OWNED_MESSAGE
            ))
        );
    }

    #[test]
    fn create_gic_before_vm_reports_state_error_without_calling_creator() {
        let creator = Arc::new(RecordingGicCreator::with_metadata(gic_metadata()));
        let mut backend = HvfBackend::new_with_gic_creator(creator.clone());

        assert_eq!(
            backend.create_gic_with_configured_creator(None),
            Err(crate::gic::HvfGicError::InvalidState(
                super::VM_NOT_CREATED_FOR_GIC_MESSAGE
            ))
        );
        assert_eq!(creator.create_count(), 0);
        assert_eq!(backend.gic_metadata(), None);
    }

    #[test]
    fn duplicate_gic_creation_is_rejected_without_calling_creator_again() {
        let creator = Arc::new(RecordingGicCreator::with_metadata(gic_metadata()));
        let mut backend = HvfBackend::new_with_gic_creator(creator.clone());
        backend.vm_created = true;

        assert_eq!(
            backend.create_gic_with_configured_creator(None),
            Ok(&gic_metadata())
        );
        assert_eq!(
            backend.create_gic_with_configured_creator(None),
            Err(crate::gic::HvfGicError::InvalidState(
                super::GIC_ALREADY_CREATED_MESSAGE
            ))
        );
        assert_eq!(creator.create_count(), 1);
        assert_eq!(backend.gic_metadata(), Some(&gic_metadata()));
    }

    #[test]
    fn explicit_msi_configuration_is_forwarded_once_to_the_creator() {
        let expected_metadata = gic_metadata_with_msi();
        let creator = Arc::new(RecordingGicCreator::with_msi_metadata(expected_metadata));
        let mut backend = HvfBackend::new_with_gic_creator(creator.clone());
        backend.vm_created = true;
        let configuration = crate::gic::HvfGicMsiConfiguration::new(
            NonZeroU32::new(8).expect("test MSI count should be nonzero"),
        );

        assert_eq!(
            backend.create_gic_with_configured_creator(Some(configuration)),
            Ok(&expected_metadata)
        );
        assert_eq!(creator.requests(), vec![Some(configuration)]);
        assert_eq!(creator.create_count(), 1);
        assert_eq!(backend.gic_metadata(), Some(&expected_metadata));
        assert!(backend.gic_msi_signaler().is_some());
    }

    #[test]
    fn inconsistent_msi_creator_result_is_not_published() {
        let creator = Arc::new(RecordingGicCreator::with_metadata(gic_metadata()));
        let mut backend = HvfBackend::new_with_gic_creator(creator.clone());
        backend.vm_created = true;
        let configuration = crate::gic::HvfGicMsiConfiguration::new(
            NonZeroU32::new(1).expect("test MSI count should be nonzero"),
        );

        assert_eq!(
            backend.create_gic_with_configured_creator(Some(configuration)),
            Err(crate::gic::HvfGicError::InvalidState(
                "GIC MSI request, metadata, and send capability disagree"
            ))
        );
        assert_eq!(creator.requests(), vec![Some(configuration)]);
        assert_eq!(backend.gic_metadata(), None);
        assert!(backend.gic_msi_signaler().is_none());
    }

    #[test]
    fn failed_gic_creation_does_not_store_metadata() {
        let creator = Arc::new(RecordingGicCreator::with_error(
            crate::gic::HvfGicError::Backend(BackendError::Hypervisor(
                "injected GIC failure".to_string(),
            )),
        ));
        let mut backend = HvfBackend::new_with_gic_creator(creator.clone());
        backend.vm_created = true;

        assert_eq!(
            backend.create_gic_with_configured_creator(None),
            Err(crate::gic::HvfGicError::Backend(BackendError::Hypervisor(
                "injected GIC failure".to_string()
            )))
        );
        assert_eq!(creator.create_count(), 1);
        assert_eq!(backend.gic_metadata(), None);
    }

    #[test]
    fn gic_creation_after_vcpu_topology_started_is_rejected() {
        let creator = Arc::new(RecordingGicCreator::with_metadata(gic_metadata()));
        let mut backend = HvfBackend::new_with_gic_creator(creator.clone());
        backend.vm_created = true;
        backend.vcpu_topology_started = true;

        assert_eq!(
            backend.create_gic_with_configured_creator(None),
            Err(crate::gic::HvfGicError::InvalidState(
                super::VCPU_TOPOLOGY_ALREADY_STARTED_MESSAGE
            ))
        );
        assert_eq!(creator.create_count(), 0);
        assert_eq!(backend.gic_metadata(), None);
    }

    #[test]
    fn clearing_vm_owned_state_removes_gic_metadata_and_topology_flag() {
        let mut backend = HvfBackend::new();
        let metadata = gic_metadata_with_msi();
        backend.gic = Some(metadata);
        let signaler = crate::gic::HvfGicMsiSignaler::for_backend_test(
            metadata.msi.expect("test MSI metadata should exist"),
        );
        let retained_signaler = signaler.clone();
        let interrupt = signaler
            .allocator()
            .allocate()
            .expect("test MSI should allocate");
        backend.gic_msi_signaler = Some(signaler);
        backend.vcpu_topology_started = true;

        backend.clear_vm_owned_state();

        assert_eq!(backend.gic_metadata(), None);
        assert!(backend.gic_msi_signaler().is_none());
        assert!(!backend.vcpu_topology_started);
        assert_eq!(
            retained_signaler
                .send(&interrupt)
                .expect_err("cleared VM state should revoke retained MSI clones"),
            crate::gic::HvfGicMsiSignalError::InvalidState("HVF GIC MSI signaler is inactive")
        );
    }

    #[test]
    fn map_guest_memory_before_vm_reports_state_error() {
        let page_size = page_size();
        let mut backend = HvfBackend::new_with_memory_mapper(Arc::new(RecordingMapper::default()));
        let memory = memory_for_ranges(vec![range(0, page_size)]);

        let err = backend
            .map_guest_memory_with_configured_mapper(memory, HvfMemoryPermissions::GUEST_RAM)
            .expect_err("mapping guest memory before VM creation should fail");

        assert!(matches!(
            err,
            crate::memory::HvfGuestMemoryMappingError::InvalidState(
                super::VM_NOT_CREATED_FOR_MEMORY_MESSAGE
            )
        ));
    }

    #[test]
    fn pmem_mapping_before_vm_checks_state_before_host_registration() {
        let page_size = page_size();
        let mut backend = HvfBackend::new_with_memory_mapper(Arc::new(RecordingMapper::default()));
        let layout =
            GuestMemoryLayout::new(vec![range(0, page_size)]).expect("layout should be valid");
        let memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        let backing = TempPmemFile::new("before-vm-pmem", VIRTIO_PMEM_ALIGNMENT)
            .expect("pmem backing should be created");
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new(
            "pmem0",
            backing.path_text(),
        )));
        let devices =
            PreparedPmemDevices::from_configs(&configs, &layout).expect("pmem should prepare");
        std::fs::OpenOptions::new()
            .write(true)
            .open(backing.path())
            .expect("pmem backing should open for truncation")
            .set_len(1)
            .expect("pmem backing should truncate");

        let err = backend
            .map_guest_memory_with_pmem_devices_and_configured_mapper(
                memory,
                devices.as_slice(),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect_err("mapping pmem before VM creation should fail on state first");

        assert!(matches!(
            err,
            crate::memory::HvfGuestMemoryMappingError::InvalidState(
                super::VM_NOT_CREATED_FOR_MEMORY_MESSAGE
            )
        ));
    }

    #[test]
    fn pmem_mapping_checks_guest_permissions_before_host_registration() {
        let page_size = page_size();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;
        let layout =
            GuestMemoryLayout::new(vec![range(0, page_size)]).expect("layout should be valid");
        let memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        let backing = TempPmemFile::new("invalid-permissions-pmem", VIRTIO_PMEM_ALIGNMENT)
            .expect("pmem backing should be created");
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new(
            "pmem0",
            backing.path_text(),
        )));
        let devices =
            PreparedPmemDevices::from_configs(&configs, &layout).expect("pmem should prepare");
        std::fs::OpenOptions::new()
            .write(true)
            .open(backing.path())
            .expect("pmem backing should open for truncation")
            .set_len(1)
            .expect("pmem backing should truncate");

        let err = backend
            .map_guest_memory_with_pmem_devices_and_configured_mapper(
                memory,
                devices.as_slice(),
                HvfMemoryPermissions::new(false, false, false),
            )
            .expect_err("invalid permissions should fail before pmem host registration");

        assert!(matches!(
            err,
            crate::memory::HvfGuestMemoryMappingError::EmptyPermissions
        ));
        assert_eq!(mapper.map_count(), 0);
    }

    #[test]
    fn duplicate_guest_memory_mapping_is_rejected() {
        let page_size = page_size();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;

        backend
            .map_guest_memory_with_configured_mapper(
                memory_for_ranges(vec![range(0, page_size)]),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("first guest memory mapping should succeed");
        let err = backend
            .map_guest_memory_with_configured_mapper(
                memory_for_ranges(vec![range(0, page_size)]),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect_err("second guest memory mapping should fail");

        assert!(matches!(
            err,
            crate::memory::HvfGuestMemoryMappingError::InvalidState(
                super::GUEST_MEMORY_ALREADY_MAPPED_MESSAGE
            )
        ));
        assert!(backend.has_guest_memory_mapping());
        assert_eq!(mapper.map_count(), 1);
    }

    #[test]
    fn duplicate_pmem_mapping_checks_state_before_host_registration() {
        let page_size = page_size();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;

        backend
            .map_guest_memory_with_configured_mapper(
                memory_for_ranges(vec![range(0, page_size)]),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("first guest memory mapping should succeed");

        let layout =
            GuestMemoryLayout::new(vec![range(0, page_size)]).expect("layout should be valid");
        let memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        let backing = TempPmemFile::new("duplicate-pmem", VIRTIO_PMEM_ALIGNMENT)
            .expect("pmem backing should be created");
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new(
            "pmem0",
            backing.path_text(),
        )));
        let devices =
            PreparedPmemDevices::from_configs(&configs, &layout).expect("pmem should prepare");
        std::fs::OpenOptions::new()
            .write(true)
            .open(backing.path())
            .expect("pmem backing should open for truncation")
            .set_len(1)
            .expect("pmem backing should truncate");

        let err = backend
            .map_guest_memory_with_pmem_devices_and_configured_mapper(
                memory,
                devices.as_slice(),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect_err("duplicate pmem mapping should fail on state first");

        assert!(matches!(
            err,
            crate::memory::HvfGuestMemoryMappingError::InvalidState(
                super::GUEST_MEMORY_ALREADY_MAPPED_MESSAGE
            )
        ));
        assert!(backend.has_guest_memory_mapping());
        assert_eq!(mapper.map_count(), 1);
    }

    #[test]
    fn mapped_guest_memory_access_requires_mapping() {
        let mut backend = HvfBackend::new();

        assert!(matches!(
            backend.mapped_guest_memory(),
            Err(crate::memory::HvfGuestMemoryMappingError::InvalidState(
                super::GUEST_MEMORY_NOT_MAPPED_MESSAGE
            ))
        ));
        assert!(matches!(
            backend.mapped_guest_memory_mut(),
            Err(crate::memory::HvfGuestMemoryMappingError::InvalidState(
                super::GUEST_MEMORY_NOT_MAPPED_MESSAGE
            ))
        ));
    }

    #[test]
    fn mapped_guest_memory_access_borrows_backend_owned_memory() {
        let page_size = page_size();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;
        let memory_size = page_size;

        backend
            .map_guest_memory_with_configured_mapper(
                memory_for_ranges(vec![range(0, memory_size)]),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("guest memory mapping should succeed");
        backend
            .mapped_guest_memory_mut()
            .expect("mapped guest memory should be mutable")
            .write_slice(&[0xab], GuestAddress::new(0))
            .expect("mapped guest memory write should succeed");
        let mut byte = [0];
        backend
            .mapped_guest_memory()
            .expect("mapped guest memory should be readable")
            .read_slice(&mut byte, GuestAddress::new(0))
            .expect("mapped guest memory read should succeed");

        assert_eq!(byte, [0xab]);
        assert_eq!(
            backend
                .mapped_guest_memory()
                .expect("mapped guest memory should remain available")
                .total_size(),
            memory_size
        );
        assert_eq!(mapper.map_count(), 1);
    }

    #[test]
    fn dynamic_guest_memory_mapping_requires_active_mapping() {
        let page_size = page_size();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper);
        backend.vm_created = true;
        let dynamic_range = range(page_size, page_size);

        assert!(matches!(
            backend.map_dynamic_guest_memory_region_with_configured_mapper(
                dynamic_range,
                HvfMemoryPermissions::GUEST_RAM
            ),
            Err(crate::memory::HvfGuestMemoryMappingError::InvalidState(
                super::GUEST_MEMORY_NOT_MAPPED_MESSAGE
            ))
        ));
        assert!(matches!(
            backend.unmap_dynamic_guest_memory_region_with_configured_mapper(dynamic_range),
            Err(crate::memory::HvfGuestMemoryMappingError::InvalidState(
                super::GUEST_MEMORY_NOT_MAPPED_MESSAGE
            ))
        ));
    }

    #[test]
    fn dynamic_guest_memory_mapping_keeps_backend_instances_independent() {
        let page_size = page_size();
        let base_range = range(0, page_size);
        let dynamic_range = range(page_size, page_size);
        let first_mapper = Arc::new(RecordingMapper::default());
        let second_mapper = Arc::new(RecordingMapper::default());
        let mut first = HvfBackend::new_with_memory_mapper(first_mapper.clone());
        let mut second = HvfBackend::new_with_memory_mapper(second_mapper.clone());
        first.vm_created = true;
        second.vm_created = true;

        first
            .map_guest_memory_with_configured_mapper(
                memory_for_ranges(vec![base_range]),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("first backend initial memory should map");
        second
            .map_guest_memory_with_configured_mapper(
                memory_for_ranges(vec![base_range]),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("second backend initial memory should map");

        first
            .map_dynamic_guest_memory_region_with_configured_mapper(
                dynamic_range,
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("first backend dynamic range should map");
        second
            .map_dynamic_guest_memory_region_with_configured_mapper(
                dynamic_range,
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("second backend dynamic range should map independently");

        first
            .unmap_dynamic_guest_memory_region_with_configured_mapper(dynamic_range)
            .expect("first backend dynamic range should unmap");

        assert_eq!(
            first
                .mapped_guest_memory()
                .expect("first backend memory should remain mapped")
                .total_size(),
            page_size
        );
        assert_eq!(
            second
                .mapped_guest_memory()
                .expect("second backend memory should remain mapped")
                .total_size(),
            page_size * 2
        );
        assert_eq!(first_mapper.map_count(), 2);
        assert_eq!(second_mapper.map_count(), 2);
        assert_eq!(first_mapper.unmap_count(), 1);
        assert_eq!(second_mapper.unmap_count(), 0);
    }

    #[test]
    fn map_guest_memory_with_pmem_devices_maps_dram_then_pmem_permissions() {
        let page_size = page_size();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;
        let layout =
            GuestMemoryLayout::new(vec![range(0, page_size)]).expect("layout should be valid");
        let memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        let writable = TempPmemFile::new("writable", VIRTIO_PMEM_ALIGNMENT)
            .expect("writable pmem file should be created");
        let readonly = TempPmemFile::new("readonly", VIRTIO_PMEM_ALIGNMENT)
            .expect("readonly pmem file should be created");
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new(
            "pmem0",
            writable.path_text(),
        )));
        configs.upsert(pmem_config(
            PmemConfigInput::new("pmem1", readonly.path_text()).with_read_only(true),
        ));
        let devices =
            PreparedPmemDevices::from_configs(&configs, &layout).expect("pmem should prepare");
        let pmem_ranges: Vec<_> = devices
            .as_slice()
            .iter()
            .map(PreparedPmemDevice::guest_range)
            .collect();

        backend
            .map_guest_memory_with_pmem_devices_and_configured_mapper(
                memory,
                devices.as_slice(),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("guest and pmem memory should map");

        assert!(backend.has_guest_memory_mapping());
        let maps = mapper.maps();
        let mut mapped = maps
            .iter()
            .map(|(request, permissions)| (request.range(), *permissions));
        assert_eq!(
            mapped.next(),
            Some((range(0, page_size), HvfMemoryPermissions::GUEST_RAM))
        );
        assert_eq!(
            mapped.next(),
            Some((pmem_ranges[0], HvfMemoryPermissions::new(true, true, false)))
        );
        assert_eq!(
            mapped.next(),
            Some((pmem_ranges[1], HvfMemoryPermissions::READ))
        );
        assert_eq!(mapped.next(), None);
    }

    #[test]
    fn pmem_host_mapping_registers_exact_runtime_mapping() {
        let payload = [0x11, 0x22, 0x33, 0x44, 0x55];
        let backing = TempPmemFile::with_bytes("direct-owner", &payload)
            .expect("pmem backing file should be created");
        let page_size = page_size();
        let layout =
            GuestMemoryLayout::new(vec![range(0, page_size)]).expect("layout should be valid");
        let memory = GuestMemory::allocate(&layout).expect("guest memory should allocate");
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new(
            "pmem0",
            backing.path_text(),
        )));
        let devices =
            PreparedPmemDevices::from_configs(&configs, &layout).expect("pmem should prepare");
        let device = &devices.as_slice()[0];
        let expected_address = device.mapping().host_address().as_ptr() as usize;
        let expected_size = device.mapping().host_size();
        let expected_range = device.guest_range();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;

        backend
            .map_guest_memory_with_pmem_devices_and_configured_mapper(
                memory,
                devices.as_slice(),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("guest and direct pmem memory should map");

        let maps = mapper.maps();
        let (request, permissions) = maps
            .get(1)
            .copied()
            .expect("pmem should be the second map request");
        assert_eq!(request.range(), expected_range);
        assert_eq!(request.host_address(), expected_address);
        assert_eq!(request.size(), expected_size);
        assert_eq!(permissions, HvfMemoryPermissions::new(true, true, false));

        let mut observed = [0; 5];
        // SAFETY: the prepared device retains the live mapping and the
        // destination fits within its nonzero file-backed prefix.
        unsafe {
            std::ptr::copy_nonoverlapping(
                device.mapping().host_address().as_ptr().cast::<u8>(),
                observed.as_mut_ptr(),
                observed.len(),
            );
        }
        assert_eq!(observed, payload);
    }

    #[test]
    fn pmem_host_mapping_does_not_reopen_the_configured_path() {
        let backing = TempPmemFile::with_bytes("direct-no-reopen", b"authoritative")
            .expect("pmem backing file should be created");
        let page_size = page_size();
        let layout =
            GuestMemoryLayout::new(vec![range(0, page_size)]).expect("layout should be valid");
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new(
            "pmem0",
            backing.path_text(),
        )));
        let devices =
            PreparedPmemDevices::from_configs(&configs, &layout).expect("pmem should prepare");
        let moved_path = backing.path().with_extension("moved");
        std::fs::rename(backing.path(), &moved_path)
            .expect("configured path should move after preparation");

        let host_mappings = super::pmem_host_memory_mappings(devices.as_slice())
            .expect("host mapping should use the retained mapping lease");

        std::fs::rename(&moved_path, backing.path())
            .expect("test backing path should be restored for cleanup");
        assert_eq!(host_mappings.len(), 1);
    }
    #[test]
    fn unmap_guest_memory_clears_active_mapping() {
        let page_size = page_size();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;

        backend
            .map_guest_memory_with_configured_mapper(
                memory_for_ranges(vec![range(0, page_size)]),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("guest memory mapping should succeed");

        backend
            .unmap_guest_memory()
            .expect("guest memory should unmap cleanly");

        assert!(!backend.has_guest_memory_mapping());
        assert_eq!(mapper.unmap_count(), 1);
    }

    #[test]
    fn unmap_guest_memory_keeps_active_mapping_when_unmap_fails() {
        let page_size = page_size();
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;

        backend
            .map_guest_memory_with_configured_mapper(
                memory_for_ranges(vec![range(0, page_size)]),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("guest memory mapping should succeed");

        mapper.set_fail_unmap(true);
        let err = backend
            .unmap_guest_memory()
            .expect_err("failed unmap should be reported");

        assert!(matches!(
            err,
            crate::memory::HvfGuestMemoryMappingError::UnmapFailed { failures }
                if failures.len() == 1
        ));
        assert!(backend.has_guest_memory_mapping());
        assert_eq!(mapper.unmap_count(), 1);

        mapper.set_fail_unmap(false);
        backend
            .unmap_guest_memory()
            .expect("retry should clear retained mapping");

        assert!(!backend.has_guest_memory_mapping());
        assert_eq!(mapper.unmap_count(), 2);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn lazy_composite_closes_public_borrows_and_survives_failed_unmap() {
        let _test_lock = MACH_LAZY_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        let mapper = Arc::new(RecordingMapper::default());
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;
        backend
            .map_lazy_guest_memory_consumer_with_configured_mapper(
                lazy_consumer(1),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect("protected lazy consumer should map");

        assert!(
            backend
                .mapped_guest_memory()
                .expect("internal lazy memory should be available")
                .is_protected_lazy()
        );
        assert!(
            backend
                .mapped_guest_memory_and_pmem_flush_executor_mut()
                .expect("pmem dispatch should borrow protected lazy memory")
                .0
                .is_protected_lazy()
        );
        assert!(
            backend
                .mapped_guest_memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM,)
                .expect("virtio-mem dispatch should borrow protected lazy memory")
                .0
                .is_protected_lazy()
        );
        assert!(matches!(
            backend.mapped_guest_memory_for_public_access(),
            Err(HvfGuestMemoryMappingError::InvalidState(
                "direct guest-memory borrowing is unavailable for lazy guest memory"
            ))
        ));
        assert!(matches!(
            backend.mapped_guest_memory_for_public_access_mut(),
            Err(HvfGuestMemoryMappingError::InvalidState(
                "direct guest-memory borrowing is unavailable for lazy guest memory"
            ))
        ));

        mapper.set_fail_unmap(true);
        assert!(
            backend
                .unmap_guest_memory()
                .expect_err("injected lazy unmap failure should be retained")
                .to_string()
                .contains("unmap")
        );
        assert!(backend.lazy_guest_memory_consumer.is_some());
        assert!(
            backend
                .mapped_guest_memory()
                .expect("consumer must remain after failed unmap")
                .is_protected_lazy()
        );

        mapper.set_fail_unmap(false);
        backend
            .unmap_guest_memory()
            .expect("retry should tear down guest mapping before host bridge");
        assert!(backend.lazy_guest_memory_consumer.is_none());
        assert_eq!(mapper.unmap_count(), 2);
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn partial_lazy_mapping_retains_composite_until_cleanup() {
        let _test_lock = MACH_LAZY_TEST_LOCK
            .lock()
            .expect("Mach lazy test lock should not be poisoned");
        let mapper = Arc::new(RecordingMapper::default());
        mapper.set_fail_map_call(Some(2));
        mapper.set_fail_unmap(true);
        let mut backend = HvfBackend::new_with_memory_mapper(mapper.clone());
        backend.vm_created = true;

        backend
            .map_lazy_guest_memory_consumer_with_configured_mapper(
                lazy_consumer(2),
                HvfMemoryPermissions::GUEST_RAM,
            )
            .expect_err("second lazy mapping should fail");
        assert!(backend.has_guest_memory_mapping());
        assert!(backend.lazy_guest_memory_consumer.is_some());
        assert!(backend.lazy_guest_fault_handler.is_some());

        mapper.set_fail_map_call(None);
        mapper.set_fail_unmap(false);
        backend
            .unmap_guest_memory()
            .expect("partial lazy mapping should clean up while bridge is retained");
        assert!(backend.lazy_guest_memory_consumer.is_none());
        assert_eq!(mapper.unmap_count(), 2);
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn unsupported_target_rejects_vm_creation() {
        use bangbang_runtime::VmBackend;

        let mut backend = HvfBackend::new();

        assert_eq!(
            backend.create_vm(),
            Err(BackendError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE
            ))
        );
    }

    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    #[test]
    fn unsupported_target_rejects_gic_creation() {
        let mut backend = HvfBackend::new();

        assert_eq!(
            backend.create_gic(),
            Err(crate::gic::HvfGicError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE
            ))
        );
        assert_eq!(backend.gic_metadata(), None);
    }

    #[derive(Debug)]
    struct RecordingMapper {
        state: Mutex<RecordingMapperState>,
    }

    impl Default for RecordingMapper {
        fn default() -> Self {
            Self::new(false)
        }
    }

    impl RecordingMapper {
        fn new(fail_unmap: bool) -> Self {
            Self {
                state: Mutex::new(RecordingMapperState {
                    maps: Vec::new(),
                    unmaps: Vec::new(),
                    fail_map_call: None,
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

        fn set_fail_unmap(&self, fail_unmap: bool) {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .fail_unmap = fail_unmap;
        }

        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        fn set_fail_map_call(&self, fail_map_call: Option<usize>) {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .fail_map_call = fail_map_call;
        }
    }

    impl HvfMemoryMapper for RecordingMapper {
        fn map_region(
            &self,
            request: HvfMemoryMapRequest,
            permissions: HvfMemoryPermissions,
        ) -> Result<(), BackendError> {
            let mut state = self
                .state
                .lock()
                .expect("state lock should not be poisoned");
            state.maps.push((request, permissions));
            if state.fail_map_call == Some(state.maps.len()) {
                return Err(BackendError::Hypervisor("injected map failure".to_string()));
            }
            Ok(())
        }

        fn unmap_region(
            &self,
            mapped_region: HvfMappedGuestMemoryRegion,
        ) -> Result<(), BackendError> {
            let mut state = self
                .state
                .lock()
                .expect("state lock should not be poisoned");
            state.unmaps.push(mapped_region);

            if state.fail_unmap {
                return Err(BackendError::Hypervisor(
                    "injected unmap failure".to_string(),
                ));
            }

            Ok(())
        }

        fn protect_region(
            &self,
            _: GuestMemoryRange,
            _: HvfMemoryPermissions,
        ) -> Result<(), BackendError> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingMapperState {
        maps: Vec<(HvfMemoryMapRequest, HvfMemoryPermissions)>,
        unmaps: Vec<HvfMappedGuestMemoryRegion>,
        fail_map_call: Option<usize>,
        fail_unmap: bool,
    }

    #[derive(Debug)]
    struct TempPmemFile {
        path: std::path::PathBuf,
    }

    impl TempPmemFile {
        fn new(name: &str, size: u64) -> std::io::Result<Self> {
            static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "bangbang-hvf-backend-{name}-{}-{id}",
                std::process::id()
            ));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.set_len(size)?;

            Ok(Self { path })
        }

        fn with_bytes(name: &str, bytes: &[u8]) -> std::io::Result<Self> {
            let file = Self::new(
                name,
                u64::try_from(bytes.len()).expect("test payload length should fit in u64"),
            )?;
            let mut backing = std::fs::OpenOptions::new().write(true).open(&file.path)?;
            backing.write_all(bytes)?;
            backing.flush()?;
            Ok(file)
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }

        fn path_text(&self) -> String {
            self.path.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempPmemFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[derive(Debug)]
    struct RecordingGicCreator {
        result: Result<crate::gic::HvfGicMetadata, crate::gic::HvfGicError>,
        publish_signaler: bool,
        create_count: Mutex<usize>,
        requests: Mutex<Vec<Option<crate::gic::HvfGicMsiConfiguration>>>,
    }

    impl RecordingGicCreator {
        fn with_metadata(metadata: crate::gic::HvfGicMetadata) -> Self {
            Self {
                result: Ok(metadata),
                publish_signaler: false,
                create_count: Mutex::new(0),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn with_msi_metadata(metadata: crate::gic::HvfGicMetadata) -> Self {
            Self {
                result: Ok(metadata),
                publish_signaler: true,
                create_count: Mutex::new(0),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn with_error(error: crate::gic::HvfGicError) -> Self {
            Self {
                result: Err(error),
                publish_signaler: false,
                create_count: Mutex::new(0),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn create_count(&self) -> usize {
            *self
                .create_count
                .lock()
                .expect("create count lock should not be poisoned")
        }

        fn requests(&self) -> Vec<Option<crate::gic::HvfGicMsiConfiguration>> {
            self.requests
                .lock()
                .expect("GIC request lock should not be poisoned")
                .clone()
        }
    }

    impl crate::gic::HvfGicCreator for RecordingGicCreator {
        fn create_gic(
            &self,
            msi: Option<crate::gic::HvfGicMsiConfiguration>,
        ) -> Result<crate::gic::CreatedHvfGic, crate::gic::HvfGicError> {
            *self
                .create_count
                .lock()
                .expect("create count lock should not be poisoned") += 1;
            self.requests
                .lock()
                .expect("GIC request lock should not be poisoned")
                .push(msi);
            self.result
                .clone()
                .map(|metadata| crate::gic::CreatedHvfGic {
                    msi_signaler: self.publish_signaler.then(|| {
                        crate::gic::HvfGicMsiSignaler::for_backend_test(
                            metadata.msi.expect("test MSI metadata should exist"),
                        )
                    }),
                    metadata,
                })
        }
    }
}
