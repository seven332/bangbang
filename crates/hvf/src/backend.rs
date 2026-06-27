use std::sync::Arc;

use bangbang_runtime::memory::GuestMemory;
use bangbang_runtime::{BackendError, VmBackend};

use crate::gic::{HvfGicCreator, HvfGicError, HvfGicMetadata, RealHvfGicCreator};
use crate::memory::{
    HvfGuestMemoryMapping, HvfGuestMemoryMappingError, HvfMemoryMapper, HvfMemoryPermissions,
    RealHvfMemoryMapper,
};
use crate::runner::{HvfVcpuRunner, HvfVcpuRunnerError};
use crate::vcpu::HvfVcpu;

const VM_NOT_CREATED_FOR_MEMORY_MESSAGE: &str = "VM must be created before mapping guest memory";
const GUEST_MEMORY_ALREADY_MAPPED_MESSAGE: &str = "guest memory is already mapped";
const VM_NOT_CREATED_FOR_GIC_MESSAGE: &str = "VM must be created before creating a GIC";
const GIC_ALREADY_CREATED_MESSAGE: &str = "GIC is already created";
const VCPU_TOPOLOGY_ALREADY_STARTED_MESSAGE: &str = "GIC must be created before creating vCPUs";

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

    pub fn unmap_guest_memory(&mut self) -> Result<(), HvfGuestMemoryMappingError> {
        if let Some(mapping) = self.guest_memory.as_mut() {
            mapping.unmap_all()?;
        }

        self.guest_memory = None;
        Ok(())
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
        if !Self::is_supported_target() {
            return Err(BackendError::Unsupported(crate::ffi::UNSUPPORTED_TARGET_MESSAGE).into());
        }

        if !self.vm_created {
            return Err(BackendError::InvalidState(
                "VM must be created before starting a vCPU runner",
            )
            .into());
        }

        let runner = HvfVcpuRunner::new()?;
        self.vcpu_topology_started = true;
        Ok(runner)
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

        match HvfGuestMemoryMapping::map_with_mapper(
            memory,
            permissions,
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
    use std::sync::{Arc, Mutex};

    use bangbang_runtime::BackendError;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
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
                    maps: 0,
                    unmaps: 0,
                    fail_unmap,
                }),
            }
        }

        fn map_count(&self) -> usize {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .maps
        }

        fn unmap_count(&self) -> usize {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .unmaps
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
            _: HvfMemoryMapRequest,
            _: HvfMemoryPermissions,
        ) -> Result<(), BackendError> {
            self.state
                .lock()
                .expect("state lock should not be poisoned")
                .maps += 1;
            Ok(())
        }

        fn unmap_region(&self, _: HvfMappedGuestMemoryRegion) -> Result<(), BackendError> {
            let mut state = self
                .state
                .lock()
                .expect("state lock should not be poisoned");
            state.unmaps += 1;

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
        maps: usize,
        unmaps: usize,
        fail_unmap: bool,
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
