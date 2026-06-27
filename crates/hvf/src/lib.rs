//! Hypervisor.framework backend.

mod backend;
mod exit;
mod ffi;
mod gic;
mod memory;
mod runner;
mod vcpu;

pub use backend::HvfBackend;
pub use exit::{HvfExceptionExit, HvfVcpuExit};
pub use gic::{
    HvfGicError, HvfGicInterruptRange, HvfGicMetadata, HvfGicMsiMetadata, HvfGicRedistributor,
    HvfGicRegion, HvfGicTimerInterrupts,
};
pub use memory::{HvfGuestMemoryMappingError, HvfGuestMemoryUnmapFailure, HvfMemoryPermissions};
pub use runner::{HvfVcpuRunner, HvfVcpuRunnerError};
pub use vcpu::{HvfRegister, HvfSystemRegister, HvfVcpu};
