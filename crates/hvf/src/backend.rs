use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use bangbang_runtime::memory::{GuestMemory, GuestMemoryLayout, GuestMemoryRange};
use bangbang_runtime::pmem::PreparedPmemDevice;
use bangbang_runtime::{BackendError, VmBackend};

use crate::gic::{HvfGicCreator, HvfGicError, HvfGicMetadata, RealHvfGicCreator};
use crate::memory::{
    HvfGuestMemoryMapping, HvfGuestMemoryMappingError, HvfHostMemoryMapping, HvfMemoryMapper,
    HvfMemoryPermissions, RealHvfMemoryMapper,
};
use crate::runner::{HvfVcpuRunner, HvfVcpuRunnerError};
use crate::vcpu::HvfVcpu;

const VM_NOT_CREATED_FOR_MEMORY_MESSAGE: &str = "VM must be created before mapping guest memory";
const GUEST_MEMORY_ALREADY_MAPPED_MESSAGE: &str = "guest memory is already mapped";
const GUEST_MEMORY_NOT_MAPPED_MESSAGE: &str = "guest memory is not mapped";
const VM_NOT_CREATED_FOR_GIC_MESSAGE: &str = "VM must be created before creating a GIC";
const GIC_ALREADY_CREATED_MESSAGE: &str = "GIC is already created";
const VCPU_TOPOLOGY_ALREADY_STARTED_MESSAGE: &str = "GIC must be created before creating vCPUs";
const PMEM_SHADOW_COPY_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug)]
pub struct HvfBackend {
    vm_created: bool,
    guest_memory: Option<HvfGuestMemoryMapping>,
    gic: Option<HvfGicMetadata>,
    vcpu_topology_started: bool,
    memory_mapper: Arc<dyn HvfMemoryMapper>,
    gic_creator: Arc<dyn HvfGicCreator>,
}

impl Default for HvfBackend {
    fn default() -> Self {
        Self {
            vm_created: false,
            guest_memory: None,
            gic: None,
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

    pub fn unmap_guest_memory(&mut self) -> Result<(), HvfGuestMemoryMappingError> {
        if let Some(mapping) = self.guest_memory.as_mut() {
            mapping.unmap_all()?;
        }

        self.guest_memory = None;
        Ok(())
    }

    pub(crate) fn mapped_guest_memory(&self) -> Result<&GuestMemory, HvfGuestMemoryMappingError> {
        self.guest_memory
            .as_ref()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                GUEST_MEMORY_NOT_MAPPED_MESSAGE,
            ))?
            .memory()
    }

    pub(crate) fn mapped_guest_memory_mut(
        &mut self,
    ) -> Result<&mut GuestMemory, HvfGuestMemoryMappingError> {
        self.guest_memory
            .as_mut()
            .ok_or(HvfGuestMemoryMappingError::InvalidState(
                GUEST_MEMORY_NOT_MAPPED_MESSAGE,
            ))?
            .memory_mut()
    }

    pub fn create_gic(&mut self) -> Result<&HvfGicMetadata, HvfGicError> {
        if !Self::is_supported_target() {
            return Err(HvfGicError::Unsupported(
                crate::ffi::UNSUPPORTED_TARGET_MESSAGE,
            ));
        }

        self.create_gic_with_configured_creator()
    }

    pub fn gic_metadata(&self) -> Option<&HvfGicMetadata> {
        self.gic.as_ref()
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

        let vcpu = HvfVcpu::new()?;
        self.vcpu_topology_started = true;
        Ok(vcpu)
    }

    pub fn start_vcpu_runner(&mut self) -> Result<HvfVcpuRunner<'_>, HvfVcpuRunnerError> {
        self.validate_vcpu_runner_start()?;

        let runner = HvfVcpuRunner::new()?;
        self.vcpu_topology_started = true;
        Ok(runner)
    }

    pub(crate) fn start_session_vcpu_runner<'vm>(
        &mut self,
    ) -> Result<HvfVcpuRunner<'vm>, HvfVcpuRunnerError> {
        // The session object holds the backend borrow separately; keep this
        // constructor crate-private so arbitrary callers cannot outlive the VM.
        self.validate_vcpu_runner_start()?;

        let runner = HvfVcpuRunner::new()?;
        self.vcpu_topology_started = true;
        Ok(runner)
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

        Ok(())
    }

    fn create_gic_with_configured_creator(&mut self) -> Result<&HvfGicMetadata, HvfGicError> {
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

        let metadata = self.gic_creator.create_gic()?;
        self.gic = Some(metadata);

        self.gic
            .as_ref()
            .ok_or(HvfGicError::InvalidState("created GIC metadata is missing"))
    }

    fn clear_vm_owned_state(&mut self) {
        self.gic = None;
        self.vcpu_topology_started = false;
    }

    fn map_guest_memory_with_configured_mapper(
        &mut self,
        memory: GuestMemory,
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.map_guest_memory_with_host_mappings(memory, permissions, Vec::new())
    }

    fn map_guest_memory_with_pmem_devices_and_configured_mapper(
        &mut self,
        memory: GuestMemory,
        pmem_devices: &[PreparedPmemDevice],
        permissions: HvfMemoryPermissions,
    ) -> Result<(), HvfGuestMemoryMappingError> {
        self.validate_guest_memory_mapping_state()?;
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
            gic: None,
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
            gic: None,
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
        let label = pmem_mapping_label(device);
        let memory = pmem_shadow_memory(device).map_err(|source| {
            HvfGuestMemoryMappingError::host_mapping(&label, device.guest_range(), source)
        })?;
        host_mappings.push(HvfHostMemoryMapping::new(
            label,
            memory,
            pmem_memory_permissions(device.mapping().is_read_only()),
        ));
    }

    Ok(host_mappings)
}

fn pmem_shadow_memory(
    device: &PreparedPmemDevice,
) -> Result<GuestMemory, HvfGuestMemoryMappingError> {
    let range = device.guest_range();
    let layout = GuestMemoryLayout::new(vec![range]).map_err(|source| {
        BackendError::Hypervisor(format!(
            "failed to build HVF pmem shadow layout for range {range}: {source}"
        ))
    })?;
    let mut memory = GuestMemory::allocate(&layout).map_err(|source| {
        BackendError::Hypervisor(format!(
            "failed to allocate HVF pmem shadow memory for range {range}: {source}"
        ))
    })?;
    let Some(region) = memory.regions().first() else {
        return Err(BackendError::Hypervisor(format!(
            "HVF pmem shadow memory has no region for range {range}"
        ))
        .into());
    };

    validate_pmem_shadow_size(range, region.host_size(), device.mapping().host_size())?;
    copy_pmem_backing_to_shadow(device, &mut memory)?;

    Ok(memory)
}

fn validate_pmem_shadow_size(
    range: GuestMemoryRange,
    shadow_size: usize,
    pmem_size: usize,
) -> Result<(), HvfGuestMemoryMappingError> {
    if shadow_size == pmem_size {
        return Ok(());
    }

    Err(BackendError::Hypervisor(format!(
        "HVF pmem shadow memory for range {range} has size {shadow_size}, expected {pmem_size}"
    ))
    .into())
}

fn copy_pmem_backing_to_shadow(
    device: &PreparedPmemDevice,
    memory: &mut GuestMemory,
) -> Result<(), HvfGuestMemoryMappingError> {
    let range = device.guest_range();
    let mut file = device.backing().file().try_clone().map_err(|source| {
        BackendError::Hypervisor(format!(
            "failed to clone HVF pmem backing for range {range}: {source}"
        ))
    })?;
    file.seek(SeekFrom::Start(0)).map_err(|source| {
        BackendError::Hypervisor(format!(
            "failed to seek HVF pmem backing for range {range}: {source}"
        ))
    })?;

    let mut buffer = [0_u8; PMEM_SHADOW_COPY_BUFFER_SIZE];
    let mut copied = 0;
    let file_len = device.mapping().file_len();

    while copied < file_len {
        let read_len = pmem_shadow_read_len(file_len - copied)?;
        let Some(chunk) = buffer.get_mut(..read_len) else {
            return Err(BackendError::Hypervisor(format!(
                "HVF pmem shadow copy chunk of {read_len} bytes is larger than the copy buffer"
            ))
            .into());
        };

        file.read_exact(chunk).map_err(|source| {
            BackendError::Hypervisor(format!(
                "failed to read HVF pmem backing into shadow for range {range}: {source}"
            ))
        })?;

        let destination = range.start().checked_add(copied).ok_or_else(|| {
            BackendError::Hypervisor(format!(
                "HVF pmem shadow copy offset {copied} overflows guest address space"
            ))
        })?;
        memory.write_slice(chunk, destination).map_err(|source| {
            BackendError::Hypervisor(format!(
                "failed to write HVF pmem shadow memory at {destination}: {source}"
            ))
        })?;

        let read_len = u64::try_from(read_len).map_err(|_| {
            BackendError::Hypervisor(format!(
                "HVF pmem shadow copy chunk length {read_len} does not fit the guest address space"
            ))
        })?;
        copied = copied.checked_add(read_len).ok_or_else(|| {
            BackendError::Hypervisor(format!(
                "HVF pmem shadow copy offset {copied} overflows guest address space"
            ))
        })?;
    }

    Ok(())
}

fn pmem_shadow_read_len(remaining: u64) -> Result<usize, HvfGuestMemoryMappingError> {
    usize::try_from(remaining.min(PMEM_SHADOW_COPY_BUFFER_SIZE as u64)).map_err(|_| {
        BackendError::Hypervisor(format!(
            "HVF pmem shadow copy remaining length {remaining} does not fit this host"
        ))
        .into()
    })
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

            if let Some(mut mapping) = self.guest_memory.take()
                && mapping.unmap_all().is_err()
            {
                mapping_after_failed_unmap = Some(mapping);
            }
            let vm_destroyed = crate::ffi::destroy_vm().is_ok();
            self.vm_created = false;
            self.clear_vm_owned_state();

            if vm_destroyed && let Some(mapping) = mapping_after_failed_unmap {
                mapping.release_after_vm_destroy();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use bangbang_runtime::BackendError;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::pmem::{
        PmemConfig, PmemConfigInput, PmemConfigs, PreparedPmemDevice, PreparedPmemDevices,
        VIRTIO_PMEM_ALIGNMENT,
    };

    use super::HvfBackend;
    use crate::memory::{
        HvfMappedGuestMemoryRegion, HvfMemoryMapRequest, HvfMemoryMapper, HvfMemoryPermissions,
        host_page_size,
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

    #[test]
    fn supported_target_matches_compile_target() {
        assert_eq!(
            HvfBackend::is_supported_target(),
            cfg!(all(target_os = "macos", target_arch = "aarch64"))
        );
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
    fn create_gic_before_vm_reports_state_error_without_calling_creator() {
        let creator = Arc::new(RecordingGicCreator::with_metadata(gic_metadata()));
        let mut backend = HvfBackend::new_with_gic_creator(creator.clone());

        assert_eq!(
            backend.create_gic_with_configured_creator(),
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
            backend.create_gic_with_configured_creator(),
            Ok(&gic_metadata())
        );
        assert_eq!(
            backend.create_gic_with_configured_creator(),
            Err(crate::gic::HvfGicError::InvalidState(
                super::GIC_ALREADY_CREATED_MESSAGE
            ))
        );
        assert_eq!(creator.create_count(), 1);
        assert_eq!(backend.gic_metadata(), Some(&gic_metadata()));
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
            backend.create_gic_with_configured_creator(),
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
            backend.create_gic_with_configured_creator(),
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
        backend.gic = Some(gic_metadata());
        backend.vcpu_topology_started = true;

        backend.clear_vm_owned_state();

        assert_eq!(backend.gic_metadata(), None);
        assert!(!backend.vcpu_topology_started);
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
    fn pmem_mapping_before_vm_checks_state_before_shadow_copy() {
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
    fn duplicate_pmem_mapping_checks_state_before_shadow_copy() {
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
    fn pmem_shadow_memory_copies_file_bytes_and_zero_fills_padding() {
        let payload = [0x11, 0x22, 0x33, 0x44, 0x55];
        let backing = TempPmemFile::with_bytes("shadow-copy", &payload)
            .expect("pmem backing file should be created");
        let layout =
            GuestMemoryLayout::new(vec![range(0, page_size())]).expect("layout should be valid");
        let mut configs = PmemConfigs::new();
        configs.upsert(pmem_config(PmemConfigInput::new(
            "pmem0",
            backing.path_text(),
        )));
        let devices =
            PreparedPmemDevices::from_configs(&configs, &layout).expect("pmem should prepare");
        let device = &devices.as_slice()[0];
        let shadow =
            super::pmem_shadow_memory(device).expect("pmem shadow memory should be created");
        let mut copied = [0; 5];

        shadow
            .read_slice(&mut copied, device.guest_range().start())
            .expect("shadow payload should be readable");

        assert_eq!(copied, payload);

        let padding_offset =
            u64::try_from(payload.len()).expect("payload length should fit guest address space");
        let padding_address = device
            .guest_range()
            .start()
            .checked_add(padding_offset)
            .expect("padding address should fit guest address space");
        let mut padding = [0xff];

        shadow
            .read_slice(&mut padding, padding_address)
            .expect("shadow padding should be readable");

        assert_eq!(padding, [0]);
    }

    #[test]
    fn pmem_shadow_memory_reports_truncated_backing_without_path_leak() {
        let payload = [0x11, 0x22, 0x33, 0x44, 0x55];
        let backing = TempPmemFile::with_bytes("shadow-truncated", &payload)
            .expect("pmem backing file should be created");
        let layout =
            GuestMemoryLayout::new(vec![range(0, page_size())]).expect("layout should be valid");
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

        let err = super::pmem_host_memory_mappings(devices.as_slice())
            .expect_err("truncated pmem backing should fail shadow copy");
        let message = err.to_string();

        assert!(message.contains("pmem device `pmem0`"));
        assert!(message.contains(&devices.as_slice()[0].guest_range().to_string()));
        assert!(message.contains("failed to read HVF pmem backing into shadow"));
        assert!(!message.contains(&backing.path_text()));
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
    }

    impl HvfMemoryMapper for RecordingMapper {
        fn map_region(
            &self,
            request: HvfMemoryMapRequest,
            permissions: HvfMemoryPermissions,
        ) -> Result<(), BackendError> {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .maps
                .push((request, permissions));
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
    }

    #[derive(Debug, Default)]
    struct RecordingMapperState {
        maps: Vec<(HvfMemoryMapRequest, HvfMemoryPermissions)>,
        unmaps: Vec<HvfMappedGuestMemoryRegion>,
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
        create_count: Mutex<usize>,
    }

    impl RecordingGicCreator {
        fn with_metadata(metadata: crate::gic::HvfGicMetadata) -> Self {
            Self {
                result: Ok(metadata),
                create_count: Mutex::new(0),
            }
        }

        fn with_error(error: crate::gic::HvfGicError) -> Self {
            Self {
                result: Err(error),
                create_count: Mutex::new(0),
            }
        }

        fn create_count(&self) -> usize {
            *self
                .create_count
                .lock()
                .expect("create count lock should not be poisoned")
        }
    }

    impl crate::gic::HvfGicCreator for RecordingGicCreator {
        fn create_gic(&self) -> Result<crate::gic::HvfGicMetadata, crate::gic::HvfGicError> {
            *self
                .create_count
                .lock()
                .expect("create count lock should not be poisoned") += 1;
            self.result.clone()
        }
    }
}
