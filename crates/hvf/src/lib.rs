//! Hypervisor.framework backend.

mod backend;
mod exit;
mod ffi;
mod gic;
mod memory;
mod mmio;
mod psci;
mod runner;
mod startup;
mod vcpu;

pub use backend::HvfBackend;
pub use exit::{
    HvfExceptionExit, HvfHvcDecodeError, HvfHvcExit, HvfMmioAccess, HvfMmioAccessSize,
    HvfMmioDecodeError, HvfMmioDirection, HvfMmioRegister, HvfMmioRegisterWidth,
    HvfMmioResolveError, HvfResolvedMmioAccess, HvfResolvedVcpuExit, HvfSys64DecodeError,
    HvfSys64Direction, HvfSys64Exit, HvfSys64Register, HvfVcpuExit, HvfVcpuExitResolveError,
};
pub use gic::{
    HvfArm64GicIccRegisterState, HvfGicDeviceState, HvfGicError, HvfGicInterruptLineAllocator,
    HvfGicInterruptRange, HvfGicMetadata, HvfGicMsiMetadata, HvfGicRedistributor, HvfGicRegion,
    HvfGicSpiSignalError, HvfGicSpiSignaler, HvfGicTimerInterrupts,
    HvfInterruptLineAllocationError,
};
pub use memory::{HvfGuestMemoryMappingError, HvfGuestMemoryUnmapFailure, HvfMemoryPermissions};
pub use mmio::{HvfMmioCompletionError, HvfMmioDispatchError};
pub use runner::{
    HvfVcpuRunCancelHandle, HvfVcpuRunStepOutcome, HvfVcpuRunner, HvfVcpuRunnerError,
};
pub use startup::{
    HvfArm64BootBalloonDeviceConfig, HvfArm64BootBlockNotificationDispatch,
    HvfArm64BootBlockNotificationDispatchError, HvfArm64BootBlockNotificationDispatches,
    HvfArm64BootEntropyDeviceConfig, HvfArm64BootInterruptLinePurpose,
    HvfArm64BootLimiterRetryWakeupQuiescenceError, HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
    HvfArm64BootMemoryHotplugDeviceConfig, HvfArm64BootMmioDispatcherError,
    HvfArm64BootNetworkNotificationDispatch, HvfArm64BootNetworkNotificationDispatchError,
    HvfArm64BootNetworkNotificationDispatches, HvfArm64BootRunLoopControl,
    HvfArm64BootRunLoopError, HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopStopToken,
    HvfArm64BootSerialDeviceConfig, HvfArm64BootSession, HvfArm64BootSessionConfig,
    HvfArm64BootSessionError, HvfArm64BootSessionShutdownError, HvfArm64BootTimerDeviceConfig,
    HvfArm64BootVsockNotificationDispatch, HvfArm64BootVsockNotificationDispatchError,
    HvfArm64BootVsockNotificationDispatches, OwnedHvfArm64BootSession,
};
pub use vcpu::{
    ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfArm64VcpuCoreSystemRegisterState,
    HvfArm64VcpuExceptionRegisterState, HvfArm64VcpuExecutionControlRegisterState,
    HvfArm64VcpuGeneralRegisterState, HvfArm64VcpuPendingInterruptState, HvfArm64VcpuSimdFpState,
    HvfArm64VcpuThreadContextRegisterState, HvfArm64VcpuTranslationRegisterState,
    HvfArm64VcpuVirtualTimerState, HvfInterruptType, HvfRegister, HvfSimdFpRegister,
    HvfSystemRegister, HvfVcpu,
};
