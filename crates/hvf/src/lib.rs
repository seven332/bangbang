//! Hypervisor.framework backend.

mod backend;
mod exit;
mod ffi;
mod gic;
mod memory;
mod runner;
mod vcpu;

pub use backend::HvfBackend;
pub use exit::{
    HvfExceptionExit, HvfMmioAccess, HvfMmioAccessSize, HvfMmioDecodeError, HvfMmioDirection,
    HvfMmioRegister, HvfMmioRegisterWidth, HvfVcpuExit,
};
pub use gic::{
    HvfGicError, HvfGicInterruptRange, HvfGicMetadata, HvfGicMsiMetadata, HvfGicRedistributor,
    HvfGicRegion, HvfGicTimerInterrupts,
};
pub use memory::{HvfGuestMemoryMappingError, HvfGuestMemoryUnmapFailure, HvfMemoryPermissions};
pub use runner::{HvfVcpuRunner, HvfVcpuRunnerError};
pub use vcpu::{
    ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfRegister, HvfSystemRegister, HvfVcpu,
};
