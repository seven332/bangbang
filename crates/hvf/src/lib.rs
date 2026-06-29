//! Hypervisor.framework backend.

mod backend;
mod exit;
mod ffi;
mod gic;
mod memory;
mod mmio;
mod runner;
mod startup;
mod vcpu;

pub use backend::HvfBackend;
pub use exit::{
    HvfExceptionExit, HvfMmioAccess, HvfMmioAccessSize, HvfMmioDecodeError, HvfMmioDirection,
    HvfMmioRegister, HvfMmioRegisterWidth, HvfMmioResolveError, HvfResolvedMmioAccess,
    HvfResolvedVcpuExit, HvfVcpuExit, HvfVcpuExitResolveError,
};
pub use gic::{
    HvfGicError, HvfGicInterruptLineAllocator, HvfGicInterruptRange, HvfGicMetadata,
    HvfGicMsiMetadata, HvfGicRedistributor, HvfGicRegion, HvfGicSpiSignalError, HvfGicSpiSignaler,
    HvfGicTimerInterrupts, HvfInterruptLineAllocationError,
};
pub use memory::{HvfGuestMemoryMappingError, HvfGuestMemoryUnmapFailure, HvfMemoryPermissions};
pub use mmio::{HvfMmioCompletionError, HvfMmioDispatchError};
pub use runner::{
    HvfVcpuRunCancelHandle, HvfVcpuRunStepOutcome, HvfVcpuRunner, HvfVcpuRunnerError,
};
pub use startup::{
    HvfArm64BootBlockNotificationDispatch, HvfArm64BootBlockNotificationDispatchError,
    HvfArm64BootBlockNotificationDispatches, HvfArm64BootInterruptLinePurpose,
    HvfArm64BootMmioDispatcherError, HvfArm64BootRunLoopControl, HvfArm64BootRunLoopError,
    HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopStopToken, HvfArm64BootSerialDeviceConfig,
    HvfArm64BootSession, HvfArm64BootSessionConfig, HvfArm64BootSessionError,
    HvfArm64BootSessionShutdownError,
};
pub use vcpu::{
    ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfRegister, HvfSystemRegister, HvfVcpu,
};
