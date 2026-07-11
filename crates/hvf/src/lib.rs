//! Hypervisor.framework backend.

mod backend;
mod exit;
mod ffi;
mod gic;
mod memory;
mod mmio;
mod psci;
mod runner;
mod sme;
mod startup;
mod vcpu;
mod vcpu_config;

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
pub use sme::HvfArm64SmeConfiguration;
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
    ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfArm64VcpuBreakpointRegisterState,
    HvfArm64VcpuCacheSelectionRegisterState, HvfArm64VcpuCoreSystemRegisterState,
    HvfArm64VcpuDebugControlRegisterState, HvfArm64VcpuDebugTrapState,
    HvfArm64VcpuExceptionRegisterState, HvfArm64VcpuExecutionControlRegisterState,
    HvfArm64VcpuGeneralRegisterState, HvfArm64VcpuIdentificationRegisterState,
    HvfArm64VcpuPendingInterruptState, HvfArm64VcpuPhysicalTimerState,
    HvfArm64VcpuPointerAuthenticationKeyState, HvfArm64VcpuSimdFpState, HvfArm64VcpuSmePstate,
    HvfArm64VcpuSmeSystemRegisterState, HvfArm64VcpuSveSmeIdentificationRegisterState,
    HvfArm64VcpuSystemContextRegisterState, HvfArm64VcpuThreadContextRegisterState,
    HvfArm64VcpuTranslationRegisterState, HvfArm64VcpuVirtualTimerState,
    HvfArm64VcpuWatchpointRegisterState, HvfInterruptType, HvfRegister, HvfSimdFpRegister,
    HvfSystemRegister, HvfVcpu,
};
pub use vcpu_config::HvfArm64VcpuCacheConfiguration;
