//! Hypervisor.framework backend.

mod backend;
mod cache;
mod coordinator;
mod cpu_template;
mod dirty;
mod exit;
mod ffi;
mod gic;
mod memory;
mod mmio;
mod psci;
mod runner;
mod session_vcpu;
mod sme;
mod snapshot;
mod snapshot_bundle;
mod snapshot_restore;
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
pub use cpu_template::{HvfArm64CpuTemplateError, HvfArm64CpuTemplateVcpuError};
pub use dirty::{
    HvfDirtyWriteEpochResetError, HvfDirtyWriteFaultError, HvfDirtyWriteProtectionFailure,
    HvfDirtyWriteTracker, HvfDirtyWriteTrackerQueryError, HvfDirtyWriteTrackerStartError,
    HvfDirtyWriteTrackerStopError,
};
pub use exit::{
    HvfExceptionExit, HvfHvcDecodeError, HvfHvcExit, HvfMmioAccess, HvfMmioAccessSize,
    HvfMmioDecodeError, HvfMmioDirection, HvfMmioRegister, HvfMmioRegisterWidth,
    HvfMmioResolveError, HvfResolvedMmioAccess, HvfResolvedVcpuExit, HvfSys64DecodeError,
    HvfSys64Direction, HvfSys64Exit, HvfSys64Register, HvfVcpuExit, HvfVcpuExitResolveError,
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
pub use memory::{HvfGuestMemoryMappingError, HvfGuestMemoryUnmapFailure, HvfMemoryPermissions};
pub use mmio::{HvfMmioCompletionError, HvfMmioDispatchError};
pub use runner::{
    HvfArm64SnapshotV1Capture, HvfArm64SnapshotV1CaptureStage,
    HvfArm64SnapshotV1CompatibilityError, HvfArm64SnapshotV1Restore,
    HvfArm64SnapshotV1RestoreStage, HvfVcpuMpidrAffinityStage, HvfVcpuRetainedVtimerWaitOutcome,
    HvfVcpuRetainedVtimerWaitStage, HvfVcpuRunCancelHandle, HvfVcpuRunStepOutcome, HvfVcpuRunner,
    HvfVcpuRunnerError,
};
pub use session_vcpu::HvfArm64BootVcpuError;
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
    PrepareHvfSnapshotV1LoadError, PreparedHvfSnapshotV1Load, PreparedHvfSnapshotV1Memory,
    PreparedHvfSnapshotV1State,
};
pub use startup::{
    HvfArm64BootBalloonDeviceConfig, HvfArm64BootBlockNotificationDispatch,
    HvfArm64BootBlockNotificationDispatchError, HvfArm64BootBlockNotificationDispatches,
    HvfArm64BootEntropyDeviceConfig, HvfArm64BootInterruptLinePurpose,
    HvfArm64BootLimiterRetrySnapshotError, HvfArm64BootLimiterRetryWakeupQuiescenceError,
    HvfArm64BootLimiterRetryWakeupQuiescenceGuard, HvfArm64BootMemoryHotplugDeviceConfig,
    HvfArm64BootMmioDispatcherError, HvfArm64BootNetworkNotificationDispatch,
    HvfArm64BootNetworkNotificationDispatchError, HvfArm64BootNetworkNotificationDispatches,
    HvfArm64BootPciBalloonDeviceUpdater, HvfArm64BootPciBlockDeviceUpdater,
    HvfArm64BootPciDataDeviceDiagnostics, HvfArm64BootPciDataDeviceKind, HvfArm64BootPciDataError,
    HvfArm64BootPciNetworkDeviceUpdater, HvfArm64BootPciPmemDeviceUpdater,
    HvfArm64BootPciValidationDiagnostics, HvfArm64BootPciValidationError,
    HvfArm64BootPciValidationTeardownError, HvfArm64BootPciValidationTeardownEvidence,
    HvfArm64BootRunLoopControl, HvfArm64BootRunLoopError, HvfArm64BootRunLoopOutcome,
    HvfArm64BootRunLoopStopToken, HvfArm64BootSerialDeviceConfig, HvfArm64BootSession,
    HvfArm64BootSessionConfig, HvfArm64BootSessionError, HvfArm64BootSessionShutdownError,
    HvfArm64BootSnapshotV1CaptureStage, HvfArm64BootSnapshotV1DeviceCaptureError,
    HvfArm64BootSnapshotV1StateCaptureError, HvfArm64BootTimerDeviceConfig,
    HvfArm64BootVmGenIdRestoreError, HvfArm64BootVsockNotificationDispatch,
    HvfArm64BootVsockNotificationDispatchError, HvfArm64BootVsockNotificationDispatches,
    OwnedHvfArm64BootSession, RestoredHvfArm64BootSession,
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
