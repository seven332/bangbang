//! Hypervisor.framework backend.

mod backend;
mod cache;
mod coordinator;
mod cpu_template;
mod dirty;
mod exit;
mod ffi;
mod gic;
mod lazy_guest_fault;
mod lazy_host_fault;
mod lazy_pager;
mod mach_lazy;
mod memory;
mod mmio;
mod optional_state;
mod paused_topology;
mod psci;
mod pvtime;
mod runner;
mod session_vcpu;
mod sme;
mod snapshot;
mod snapshot_bundle;
mod snapshot_restore;
mod snapshot_v2;
mod snapshot_v2_balloon_platform;
mod snapshot_v2_entropy_platform;
mod snapshot_v2_multi_block_platform;
mod snapshot_v2_platform;
mod snapshot_v2_storage_platform;
mod startup;
mod topology;
mod vcpu;
mod vcpu_config;

pub use backend::HvfBackend;
pub use cache::{HostFactReadError, HvfArm64CacheTopologyError};
pub use coordinator::{
    HvfVcpuCoordinatorWork, HvfVcpuRunBarrierReport, HvfVcpuRunBarrierWaiter, HvfVcpuRunControl,
    HvfVcpuRunControlReason, HvfVcpuRunCoordinator, HvfVcpuRunCoordinatorError, HvfVcpuRunEvent,
    HvfVcpuRunMemberOutcome, HvfVcpuRunMemberResult, HvfVcpuRunTerminalReport,
};
pub use cpu_template::{
    HVF_ARM64_CPU_TEMPLATE_APPLICATION_MAX_ENTRIES, HvfArm64CpuTemplateApplicationEntry,
    HvfArm64CpuTemplateApplicationState, HvfArm64CpuTemplateApplicationStateError,
    HvfArm64CpuTemplateError, HvfArm64CpuTemplateRegisterTag, HvfArm64CpuTemplateValue,
    HvfArm64CpuTemplateValueWidth, HvfArm64CpuTemplateVcpuError,
};
pub use dirty::{
    HvfDirtyWriteEpochResetError, HvfDirtyWriteFaultError, HvfDirtyWriteProtectionFailure,
    HvfDirtyWriteTracker, HvfDirtyWriteTrackerQueryError, HvfDirtyWriteTrackerStartError,
    HvfDirtyWriteTrackerStopError,
};
pub use exit::{
    HvfExceptionExit, HvfHvcDecodeError, HvfHvcExit, HvfLazyGuestAccess, HvfLazyGuestFault,
    HvfMmioAccess, HvfMmioAccessSize, HvfMmioDecodeError, HvfMmioDirection, HvfMmioRegister,
    HvfMmioRegisterWidth, HvfMmioResolveError, HvfResolvedMmioAccess, HvfResolvedVcpuExit,
    HvfSys64DecodeError, HvfSys64Direction, HvfSys64Exit, HvfSys64Register, HvfVcpuExit,
    HvfVcpuExitResolveError,
};
pub use gic::{
    HvfArm64GicIccRegister, HvfArm64GicIccRegisterRestoreError,
    HvfArm64GicIccRegisterRestoreOperation, HvfArm64GicIccRegisterState, HvfGicDeviceState,
    HvfGicError, HvfGicInterruptLineAllocator, HvfGicInterruptRange, HvfGicMetadata,
    HvfGicMsiConfiguration, HvfGicMsiDeviceInterruptResourceError,
    HvfGicMsiDeviceInterruptResources, HvfGicMsiInterrupt, HvfGicMsiInterruptAllocationError,
    HvfGicMsiInterruptAllocator, HvfGicMsiInterruptReleaseError, HvfGicMsiMetadata, HvfGicMsiRoute,
    HvfGicMsiSignalError, HvfGicMsiSignaler, HvfGicRedistributor, HvfGicRegion,
    HvfGicSpiSignalError, HvfGicSpiSignaler, HvfGicTimerInterrupts,
    HvfInterruptLineAllocationError,
};
pub use lazy_guest_fault::{
    HvfHandledLazyGuestFault, HvfLazyGuestFaultError, HvfLazyGuestResolutionFailure,
};
pub use lazy_host_fault::{
    HVF_LAZY_HOST_FAULT_TERMINAL_EXIT_CODE, HvfLazyGuestMemoryConsumer, HvfLazyHostFaultBridge,
    HvfLazyHostFaultError, HvfLazyHostFaultShutdown, HvfLazyHostFaultStage, HvfLazyPageContents,
    HvfLazyPageRemoval, HvfLazyPageRemovalRequest, HvfLazyPageRequest, HvfLazyPageResolution,
    HvfLazyPageResolver, HvfLazyPageSource, HvfLazyPageSourceError,
};
pub use lazy_pager::{HvfLazyPager, HvfLazyPagerError};
pub use memory::{
    HvfGuestMemoryMappingError, HvfGuestMemoryUnmapFailure, HvfMemoryPermissions,
    HvfVirtioMemMappingCaptureError, HvfVirtioMemMappingCaptureState,
};
pub use mmio::{HvfMmioCompletionError, HvfMmioDispatchError};
pub use optional_state::{
    HvfArm64DebugRegisterRestoreState, HvfArm64OptionalStateValue,
    HvfArm64ReviewedOptionalStateBuildError, HvfArm64ReviewedOptionalStateRestore,
    HvfArm64ReviewedOptionalStateRestoreError, HvfArm64ReviewedOptionalStateRestoreFamily,
    HvfArm64ReviewedOptionalStateRestoreRejection, HvfArm64ReviewedOptionalStateRestoreStage,
    HvfArm64SmeRestoreState, HvfArm64SmeRestoreStateInput,
};
pub use paused_topology::{
    HvfArm64CpuSuspendConvention, HvfArm64StableCpuSuspendState,
    HvfArm64StablePausedTopologyBuildError, HvfArm64StablePausedTopologyMember,
    HvfArm64StablePausedTopologyState, HvfArm64StableVcpuDisposition,
};
pub use pvtime::{
    HvfArm64PvTimeAccountingError, HvfArm64PvTimeAccountingStage, HvfArm64PvTimeCaptureState,
    HvfArm64PvTimeContentionProbe, HvfArm64PvTimeMeasurementError, HvfArm64PvTimeVcpuCaptureState,
    is_hvf_arm64_pvtime_measurement_available,
};
pub use runner::{
    HvfArm64SnapshotV1Capture, HvfArm64SnapshotV1CaptureStage,
    HvfArm64SnapshotV1CompatibilityError, HvfArm64SnapshotV1Restore,
    HvfArm64SnapshotV1RestoreStage, HvfArm64SnapshotV2VcpuCapture,
    HvfArm64SnapshotV2VcpuCaptureStage, HvfArm64SnapshotV2VcpuRestore,
    HvfArm64SnapshotV2VcpuRestoreStage, HvfVcpuMpidrAffinityStage,
    HvfVcpuRetainedVtimerWaitOutcome, HvfVcpuRetainedVtimerWaitStage, HvfVcpuRunCancelHandle,
    HvfVcpuRunStepOutcome, HvfVcpuRunner, HvfVcpuRunnerError,
};
pub use session_vcpu::{
    HvfArm64BootVcpuError, HvfArm64BootVcpuSession, HvfArm64SnapshotV2TopologyCaptureError,
    HvfArm64StablePausedTopologyCaptureError, HvfArm64StablePausedTopologyCleanupFailure,
    HvfArm64StablePausedTopologyCleanupStage, HvfArm64StablePausedTopologyImportError,
};
pub use sme::HvfArm64SmeConfiguration;
pub use snapshot::{
    HvfArm64SnapshotOptionalStateRejection, HvfArm64SnapshotTimerPolicyError,
    HvfArm64SnapshotTimerRestoreError, HvfArm64SnapshotTimerRestoreOperation,
    HvfArm64SnapshotTimerState, normalize_arm64_snapshot_timer_state,
    validate_native_v1_arm64_snapshot_optional_state,
};
pub use snapshot_bundle::{
    HVF_SNAPSHOT_V1_GIC_DEVICE_STATE_MAX_BYTES, HvfSnapshotV1Bundle, HvfSnapshotV1BundleError,
    HvfSnapshotV1CompatibilityState, HvfSnapshotV1DecodeError, HvfSnapshotV1EncodeError,
    HvfSnapshotV1InterruptState, HvfSnapshotV1State, HvfSnapshotV1VcpuState,
    decode_hvf_snapshot_v1_state, encode_hvf_snapshot_v1_state,
};
pub use snapshot_restore::{
    HvfSnapshotV1PlatformError, HvfSnapshotV1RestoreCleanup, HvfSnapshotV1RestoreDisposition,
    HvfSnapshotV1RestoreError, HvfSnapshotV1RestoreFailure, HvfSnapshotV1RestoreStage,
    PrepareHvfSnapshotV1LoadError, PreparedHvfSnapshotV1LazyLoad, PreparedHvfSnapshotV1LazyState,
    PreparedHvfSnapshotV1Load, PreparedHvfSnapshotV1Memory, PreparedHvfSnapshotV1RuntimeRef,
    PreparedHvfSnapshotV1State,
};
pub use snapshot_v2::{
    HVF_SNAPSHOT_V2_GIC_DEVICE_STATE_MAX_BYTES, HVF_SNAPSHOT_V2_MAX_BOOT_ARGUMENT_BYTES,
    HVF_SNAPSHOT_V2_MAX_PATH_BYTES, HVF_SNAPSHOT_V2_MAX_SME_SVL_BYTES, HvfSnapshotV2BalloonState,
    HvfSnapshotV2BootState, HvfSnapshotV2BuildError, HvfSnapshotV2DecodeError,
    HvfSnapshotV2EncodeError, HvfSnapshotV2EntropyState, HvfSnapshotV2FdtState,
    HvfSnapshotV2GlobalState, HvfSnapshotV2MachineState,
    HvfSnapshotV2MemoryHotplugCaptureBuildError, HvfSnapshotV2MemoryHotplugCaptureState,
    HvfSnapshotV2MemoryHotplugPlatformState, HvfSnapshotV2MemoryHotplugState,
    HvfSnapshotV2MultiBlockState, HvfSnapshotV2NativePath, HvfSnapshotV2PlatformState,
    HvfSnapshotV2SerialState, HvfSnapshotV2State, HvfSnapshotV2StorageState,
    HvfSnapshotV2VcpuState, decode_hvf_snapshot_v2_balloon_state,
    decode_hvf_snapshot_v2_entropy_state, decode_hvf_snapshot_v2_multi_block_state,
    decode_hvf_snapshot_v2_platform_state, decode_hvf_snapshot_v2_serial_state,
    decode_hvf_snapshot_v2_state, decode_hvf_snapshot_v2_storage_state,
    encode_hvf_snapshot_v2_balloon_state, encode_hvf_snapshot_v2_entropy_state,
    encode_hvf_snapshot_v2_memory_hotplug_state, encode_hvf_snapshot_v2_multi_block_state,
    encode_hvf_snapshot_v2_platform_state, encode_hvf_snapshot_v2_serial_state,
    encode_hvf_snapshot_v2_state, encode_hvf_snapshot_v2_storage_state,
};
pub use snapshot_v2_balloon_platform::{
    HvfSnapshotV2BalloonEntropyMmioEndpointPlan, HvfSnapshotV2BalloonMmioEndpointPlan,
    HvfSnapshotV2BalloonMmioPlatformPlan, HvfSnapshotV2BalloonMmioProcessConfig,
    HvfSnapshotV2BalloonPciEndpointPlan, HvfSnapshotV2BalloonPciPlatformPlan,
    HvfSnapshotV2BalloonPreparedProduct, HvfSnapshotV2BalloonProductKind,
    PrepareHvfSnapshotV2BalloonPlatformPlanError,
    prepare_hvf_snapshot_v2_balloon_mmio_platform_plan,
    prepare_hvf_snapshot_v2_balloon_pci_platform_plan,
};
pub use snapshot_v2_entropy_platform::{
    HvfSnapshotV2EntropyPciEndpointPlan, HvfSnapshotV2StorageEntropyPciPlatformPlan,
    PrepareHvfSnapshotV2EntropyPciPlatformPlanError,
    prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan,
    prepare_hvf_snapshot_v2_storage_entropy_pci_platform_plan,
};
pub use snapshot_v2_multi_block_platform::{
    HvfSnapshotV2MultiBlockMmioRecordPlan, HvfSnapshotV2MultiBlockPciPlan,
    HvfSnapshotV2MultiBlockPciRecordPlan, HvfSnapshotV2MultiBlockPlatformPlan,
    HvfSnapshotV2MultiBlockProcessConfig, HvfSnapshotV2MultiBlockRetryPlan,
    HvfSnapshotV2MultiBlockTransportPlan, PrepareHvfSnapshotV2MultiBlockPlatformPlanError,
    prepare_hvf_snapshot_v2_multi_block_platform_plan,
};
pub use snapshot_v2_platform::{
    HvfSnapshotV2DefaultProcessShell, HvfSnapshotV2PlatformCleanupFailure,
    HvfSnapshotV2PlatformCleanupStage, HvfSnapshotV2PlatformRestoreError,
    HvfSnapshotV2PlatformRestoreFailure, HvfSnapshotV2PlatformRestoreStage,
    HvfSnapshotV2PlatformShutdownError, HvfSnapshotV2ProcessFdtMismatch,
    HvfSnapshotV2RestoredSerialShell, HvfSnapshotV2RootProcessConfig,
    HvfSnapshotV2RootResourcePlan, HvfSnapshotV2RootTransportPlan,
    HvfSnapshotV2SerialOnlyProcessConfig, PrepareHvfSnapshotV2RootPlanError,
    PreparedHvfSnapshotV2RootPlan, RestoredHvfSnapshotV2Platform,
    prepare_hvf_snapshot_v2_root_plan, restore_hvf_snapshot_v2_platform,
    restore_hvf_snapshot_v2_process_platform, restore_hvf_snapshot_v2_serial_only_process_platform,
};
pub use snapshot_v2_storage_platform::{
    HvfSnapshotV2StorageMmioPlatformPlan, HvfSnapshotV2StorageMmioProcessConfig,
    HvfSnapshotV2StorageMmioRecordPlan, HvfSnapshotV2StoragePciHostPlan,
    HvfSnapshotV2StoragePciPlatformPlan, HvfSnapshotV2StoragePciRecordPlan,
    HvfSnapshotV2StorageRetryPlan, PrepareHvfSnapshotV2StorageMmioPlatformPlanError,
    PrepareHvfSnapshotV2StoragePciPlatformPlanError,
    prepare_hvf_snapshot_v2_storage_entropy_mmio_platform_plan,
    prepare_hvf_snapshot_v2_storage_mmio_platform_plan,
    prepare_hvf_snapshot_v2_storage_pci_platform_plan,
};
pub use startup::{
    HvfArm64BootBalloonCaptureError, HvfArm64BootBalloonCaptureState,
    HvfArm64BootBalloonDeviceConfig, HvfArm64BootBalloonTransportState,
    HvfArm64BootBlockNotificationDispatch, HvfArm64BootBlockNotificationDispatchError,
    HvfArm64BootBlockNotificationDispatches, HvfArm64BootEntropyCaptureError,
    HvfArm64BootEntropyCaptureState, HvfArm64BootEntropyDeviceConfig,
    HvfArm64BootEntropyTransportState, HvfArm64BootInterruptLinePurpose,
    HvfArm64BootLimiterRetrySnapshotError, HvfArm64BootLimiterRetryWakeupQuiescenceError,
    HvfArm64BootLimiterRetryWakeupQuiescenceGuard, HvfArm64BootMemoryHotplugCaptureError,
    HvfArm64BootMemoryHotplugCaptureState, HvfArm64BootMemoryHotplugDeviceConfig,
    HvfArm64BootMemoryHotplugSnapshotV2CaptureError, HvfArm64BootMemoryHotplugTransportState,
    HvfArm64BootMmioDispatcherError, HvfArm64BootNetworkCaptureConfig,
    HvfArm64BootNetworkCaptureError, HvfArm64BootNetworkCaptureState,
    HvfArm64BootNetworkDeviceOrigin, HvfArm64BootNetworkInterfaceCaptureState,
    HvfArm64BootNetworkNotificationDispatch, HvfArm64BootNetworkNotificationDispatchError,
    HvfArm64BootNetworkNotificationDispatches, HvfArm64BootNetworkTransportCaptureState,
    HvfArm64BootPciBalloonDeviceUpdater, HvfArm64BootPciBlockDeviceUpdater,
    HvfArm64BootPciDataDeviceDiagnostics, HvfArm64BootPciDataDeviceKind, HvfArm64BootPciDataError,
    HvfArm64BootPciNetworkDeviceUpdater, HvfArm64BootPciPmemDeviceUpdater,
    HvfArm64BootPciValidationDiagnostics, HvfArm64BootPciValidationError,
    HvfArm64BootPciValidationTeardownError, HvfArm64BootPciValidationTeardownEvidence,
    HvfArm64BootRunLoopControl, HvfArm64BootRunLoopError, HvfArm64BootRunLoopOutcome,
    HvfArm64BootRunLoopStopToken, HvfArm64BootSerialCaptureError, HvfArm64BootSerialDeviceConfig,
    HvfArm64BootSerialInputDispatchError, HvfArm64BootSession, HvfArm64BootSessionConfig,
    HvfArm64BootSessionError, HvfArm64BootSessionShutdownError, HvfArm64BootSnapshotV1CaptureStage,
    HvfArm64BootSnapshotV1DeviceCaptureError, HvfArm64BootSnapshotV1StateCaptureError,
    HvfArm64BootSnapshotV2CaptureError, HvfArm64BootSnapshotV2CaptureInput,
    HvfArm64BootSnapshotV2CaptureStage, HvfArm64BootStorageCaptureError,
    HvfArm64BootStorageCaptureErrorKind, HvfArm64BootStorageCaptureStage,
    HvfArm64BootTimeIdentityRestoreError, HvfArm64BootTimerDeviceConfig,
    HvfArm64BootVmClockRestoreError, HvfArm64BootVmGenIdRestoreError,
    HvfArm64BootVsockCaptureDisposition, HvfArm64BootVsockCaptureError,
    HvfArm64BootVsockCaptureErrorKind, HvfArm64BootVsockCaptureStage,
    HvfArm64BootVsockCaptureState, HvfArm64BootVsockNotificationDispatch,
    HvfArm64BootVsockNotificationDispatchError, HvfArm64BootVsockNotificationDispatches,
    HvfArm64BootVsockTransportState, HvfSnapshotV2BalloonMmioRestoreError,
    HvfSnapshotV2BalloonMmioRestoreFailure, HvfSnapshotV2BalloonMmioRestoreFault,
    HvfSnapshotV2BalloonMmioRestoreStage, HvfSnapshotV2BalloonPciRestoreError,
    HvfSnapshotV2BalloonPciRestoreFailure, HvfSnapshotV2BalloonPciRestoreFault,
    HvfSnapshotV2BalloonPciRestoreStage, HvfSnapshotV2EntropyMmioRestoreError,
    HvfSnapshotV2EntropyMmioRestoreFailure, HvfSnapshotV2EntropyMmioRestoreStage,
    HvfSnapshotV2EntropyPciRestoreError, HvfSnapshotV2EntropyPciRestoreFailure,
    HvfSnapshotV2EntropyPciRestoreStage, HvfSnapshotV2MultiBlockMmioRestoreCleanupFailure,
    HvfSnapshotV2MultiBlockMmioRestoreError, HvfSnapshotV2MultiBlockMmioRestoreFailure,
    HvfSnapshotV2MultiBlockMmioRestoreStage, HvfSnapshotV2MultiBlockPciRestoreCleanupFailure,
    HvfSnapshotV2MultiBlockPciRestoreError, HvfSnapshotV2MultiBlockPciRestoreFailure,
    HvfSnapshotV2MultiBlockPciRestoreStage, HvfSnapshotV2RootRestoreCleanupFailure,
    HvfSnapshotV2RootRestoreError, HvfSnapshotV2RootRestoreFailure, HvfSnapshotV2RootRestoreStage,
    HvfSnapshotV2SerialOnlyRestoreError, HvfSnapshotV2StorageMmioRestoreCleanupFailure,
    HvfSnapshotV2StorageMmioRestoreError, HvfSnapshotV2StorageMmioRestoreFailure,
    HvfSnapshotV2StorageMmioRestoreStage, HvfSnapshotV2StoragePciRestoreCleanupFailure,
    HvfSnapshotV2StoragePciRestoreError, HvfSnapshotV2StoragePciRestoreFailure,
    HvfSnapshotV2StoragePciRestoreStage, OwnedHvfArm64BootSession,
    PreparedHvfArm64BootPciNetworkRemoval, RestoredHvfArm64BootSession,
    RestoredHvfSnapshotV2BalloonMmioOwners, RestoredHvfSnapshotV2BalloonPciOwners,
    RestoredHvfSnapshotV2EntropyMmioOwners, RestoredHvfSnapshotV2EntropyPciOwners,
    RestoredHvfSnapshotV2MultiBlockMmioOwners, RestoredHvfSnapshotV2MultiBlockPciOwners,
    RestoredHvfSnapshotV2StorageMmioOwners, RestoredHvfSnapshotV2StoragePciOwners,
};
pub use topology::{
    HvfVcpuTopology, HvfVcpuTopologyAllocation, HvfVcpuTopologyCreateStage, HvfVcpuTopologyError,
    HvfVcpuTopologyMemberFailure, HvfVcpuTopologyOperation,
};
pub use vcpu::{
    ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfArm64VcpuBreakpointRegisterState,
    HvfArm64VcpuCacheSelectionRegisterState, HvfArm64VcpuCoreSystemRegisterState,
    HvfArm64VcpuDebugControlRegisterState, HvfArm64VcpuDebugTrapRestoreError,
    HvfArm64VcpuDebugTrapRestoreOperation, HvfArm64VcpuDebugTrapState,
    HvfArm64VcpuExceptionRegisterState, HvfArm64VcpuExecutionControlRegisterState,
    HvfArm64VcpuGeneralRegisterRestoreError, HvfArm64VcpuGeneralRegisterState,
    HvfArm64VcpuIdentificationRegisterState, HvfArm64VcpuPendingInterruptRestoreError,
    HvfArm64VcpuPendingInterruptState, HvfArm64VcpuPhysicalTimerState,
    HvfArm64VcpuPointerAuthenticationKeyState, HvfArm64VcpuSimdFpRestoreError,
    HvfArm64VcpuSimdFpRestoreRegister, HvfArm64VcpuSimdFpState,
    HvfArm64VcpuSmePRegisterCaptureError, HvfArm64VcpuSmePRegisterState, HvfArm64VcpuSmePstate,
    HvfArm64VcpuSmeSystemRegisterState, HvfArm64VcpuSmeZRegisterCaptureError,
    HvfArm64VcpuSmeZRegisterState, HvfArm64VcpuSmeZaRegisterCaptureError,
    HvfArm64VcpuSmeZaRegisterState, HvfArm64VcpuSmeZt0RegisterCaptureError,
    HvfArm64VcpuSmeZt0RegisterState, HvfArm64VcpuSveSmeIdentificationRegisterState,
    HvfArm64VcpuSystemContextRegisterState, HvfArm64VcpuSystemRegisterRestoreError,
    HvfArm64VcpuThreadContextRegisterState, HvfArm64VcpuTranslationRegisterState,
    HvfArm64VcpuVirtualTimerState, HvfArm64VcpuWatchpointRegisterState, HvfInterruptType,
    HvfRegister, HvfSimdFpRegister, HvfSystemRegister, HvfVcpu,
};
pub use vcpu_config::{
    HvfArm64VcpuCacheConfiguration, HvfArm64VcpuCacheGeometry, HvfArm64VcpuCacheManifest,
};
