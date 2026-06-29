//! Internal HVF arm64 boot-session preparation.

use std::collections::TryReserveError;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};

use bangbang_runtime::block::BlockMmioLayout;
use bangbang_runtime::fdt::Arm64FdtError;
use bangbang_runtime::interrupt::{
    DeviceInterruptKind, DeviceInterruptTriggerError, GuestInterruptLine, InterruptSink,
};
use bangbang_runtime::memory::{GuestAddress, GuestMemory};
use bangbang_runtime::mmio::{MmioDispatcher, MmioRegionId};
use bangbang_runtime::serial::SharedSerialOutputBuffer;
use bangbang_runtime::startup::{
    Arm64BootBlockNotificationDispatch, Arm64BootBlockNotificationDispatchError,
    Arm64BootBlockNotificationDispatches, Arm64BootResourceConfig, Arm64BootResourceError,
    Arm64BootResources, Arm64BootRuntimeResources,
    Arm64BootSerialDeviceConfig as RuntimeArm64BootSerialDeviceConfig,
};
use bangbang_runtime::{BackendError, VmBackend, VmmController};

use crate::backend::HvfBackend;
use crate::gic::{
    HvfGicError, HvfGicInterruptLineAllocator, HvfGicMetadata, HvfGicSpiSignalError,
    HvfGicSpiSignaler, HvfInterruptLineAllocationError,
};
use crate::memory::{HvfGuestMemoryMappingError, HvfMemoryPermissions};
use crate::runner::{
    HvfVcpuRunCancelHandle, HvfVcpuRunStepOutcome, HvfVcpuRunner, HvfVcpuRunnerError,
};
use crate::vcpu::HvfArm64BootRegisters;

const SINGLE_VCPU_COUNT: u8 = 1;

#[derive(Debug, Clone)]
pub struct HvfArm64BootSessionConfig {
    pub block_mmio_layout: BlockMmioLayout,
    pub serial_device: Option<HvfArm64BootSerialDeviceConfig>,
}

impl HvfArm64BootSessionConfig {
    pub const fn new(block_mmio_layout: BlockMmioLayout) -> Self {
        Self {
            block_mmio_layout,
            serial_device: None,
        }
    }

    pub fn with_serial_device(mut self, serial_device: HvfArm64BootSerialDeviceConfig) -> Self {
        self.serial_device = Some(serial_device);
        self
    }
}

#[derive(Debug, Clone)]
pub struct HvfArm64BootSerialDeviceConfig {
    pub region_id: MmioRegionId,
    pub address: GuestAddress,
    pub output: SharedSerialOutputBuffer,
}

impl HvfArm64BootSerialDeviceConfig {
    pub fn new(
        region_id: MmioRegionId,
        address: GuestAddress,
        output: SharedSerialOutputBuffer,
    ) -> Self {
        Self {
            region_id,
            address,
            output,
        }
    }

    fn into_runtime(
        self,
        interrupt_line: GuestInterruptLine,
    ) -> RuntimeArm64BootSerialDeviceConfig {
        RuntimeArm64BootSerialDeviceConfig::new(
            self.region_id,
            self.address,
            interrupt_line,
            self.output,
        )
    }
}

#[derive(Debug)]
pub struct HvfArm64BootSession<'vm> {
    runner: HvfVcpuRunner<'vm>,
    backend: &'vm mut HvfBackend,
    mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
    runtime_resources: Arm64BootRuntimeResources,
    gic: HvfGicMetadata,
    primary_mpidr: u64,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    boot_registers: HvfArm64BootRegisters,
}

impl HvfArm64BootSession<'_> {
    pub fn shutdown(&mut self) -> Result<(), HvfArm64BootSessionShutdownError> {
        let runner_result = self.runner.shutdown();
        let destroy_result = <HvfBackend as VmBackend>::destroy_vm(self.backend);

        match (runner_result, destroy_result) {
            (Err(source), _) => Err(HvfArm64BootSessionShutdownError::Runner { source }),
            (Ok(()), Err(source)) => Err(HvfArm64BootSessionShutdownError::DestroyVm { source }),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    pub const fn gic_metadata(&self) -> HvfGicMetadata {
        self.gic
    }

    pub const fn primary_mpidr(&self) -> u64 {
        self.primary_mpidr
    }

    pub fn runtime_resources(&self) -> &Arm64BootRuntimeResources {
        &self.runtime_resources
    }

    /// Return a cloned handle to the runner-compatible MMIO dispatcher.
    ///
    /// The dispatcher is local to this boot session. It is shared only so
    /// vCPU-runner commands can dispatch MMIO on the runner thread. Keep cloned
    /// handles scoped to runner commands so dispatcher-owned device resources
    /// are released with the session.
    pub fn mmio_dispatcher(&self) -> Arc<Mutex<MmioDispatcher>> {
        Arc::clone(&self.mmio_dispatcher)
    }

    /// Borrow the guest memory mapped for this prepared boot session.
    ///
    /// This is internal startup plumbing for later runner-loop work; it does not
    /// make public `InstanceStart` succeed.
    pub fn guest_memory(&self) -> Result<&GuestMemory, HvfGuestMemoryMappingError> {
        self.backend.mapped_guest_memory()
    }

    /// Mutably borrow the guest memory mapped for this prepared boot session.
    ///
    /// The HVF backend remains the mapping owner, so shutdown and drop still
    /// unmap the memory through the backend.
    pub fn guest_memory_mut(&mut self) -> Result<&mut GuestMemory, HvfGuestMemoryMappingError> {
        self.backend.mapped_guest_memory_mut()
    }

    pub fn block_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.block_interrupt_lines
    }

    pub const fn serial_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.serial_interrupt_line
    }

    pub const fn boot_registers(&self) -> HvfArm64BootRegisters {
        self.boot_registers
    }

    /// Run the boot session's primary vCPU once with runner-thread MMIO handling.
    ///
    /// This is internal startup plumbing for later runner-loop work. It does not
    /// dispatch boot block notifications or make public `InstanceStart` succeed.
    pub fn run_once_and_handle_mmio(&self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        run_boot_session_vcpu_step(&self.runner, &self.mmio_dispatcher)
    }

    /// Return a handle that can request cancellation of an in-flight vCPU run step.
    ///
    /// This is internal startup plumbing for later runner-loop work. It does not
    /// shut down the boot session or make public `InstanceStart` succeed.
    pub fn run_cancel_handle(&self) -> HvfVcpuRunCancelHandle {
        self.runner.run_cancel_handle()
    }

    pub fn dispatch_block_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>
    {
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootBlockNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootBlockNotificationDispatchError::MmioDispatcher { source }
                })?;

            self.runtime_resources
                .dispatch_block_queue_notifications(memory, &mut mmio_dispatcher)
                .map_err(|source| {
                    HvfArm64BootBlockNotificationDispatchError::DispatchNotifications { source }
                })?
        };

        if !dispatches.needs_queue_interrupt() {
            return collect_block_notification_dispatches(dispatches);
        }

        let signaler = HvfGicSpiSignaler::from_metadata(&self.gic).map_err(|source| {
            HvfArm64BootBlockNotificationDispatchError::CreateSignalSink { source }
        })?;

        signal_block_queue_interrupts(dispatches, &signaler)
    }
}

impl Drop for HvfArm64BootSession<'_> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Debug)]
pub struct HvfArm64BootBlockNotificationDispatches {
    devices: Vec<HvfArm64BootBlockNotificationDispatch>,
}

impl HvfArm64BootBlockNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootBlockNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootBlockNotificationDispatch] {
        &self.devices
    }

    pub fn len(&self) -> usize {
        self.devices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn has_signal_failure(&self) -> bool {
        self.devices
            .iter()
            .any(|device| device.signal_error().is_some())
    }
}

#[derive(Debug)]
pub struct HvfArm64BootBlockNotificationDispatch {
    dispatch: Arm64BootBlockNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootBlockNotificationDispatch {
    fn new(
        dispatch: Arm64BootBlockNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootBlockNotificationDispatch {
        &self.dispatch
    }

    pub const fn signal_error(&self) -> Option<&DeviceInterruptTriggerError> {
        self.signal_error.as_ref()
    }

    pub fn queue_interrupt_signaled(&self) -> bool {
        self.dispatch.needs_queue_interrupt() && self.signal_error.is_none()
    }
}

#[derive(Debug)]
pub enum HvfArm64BootBlockNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootBlockNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

impl fmt::Display for HvfArm64BootBlockNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for block notifications: {source}"
                )
            }
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::DispatchNotifications { source } => {
                write!(
                    f,
                    "failed to dispatch boot block queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot block interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot block notification results: {source}"
                )
            }
        }
    }
}

impl std::error::Error for HvfArm64BootBlockNotificationDispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::MapGuestMemory { source } => Some(source),
            Self::MmioDispatcher { source } => Some(source),
            Self::DispatchNotifications { source } => Some(source),
            Self::CreateSignalSink { source } => Some(source),
            Self::ResultAllocation { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootMmioDispatcherError {
    Busy,
    Poisoned,
}

impl fmt::Display for HvfArm64BootMmioDispatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("HVF boot-session MMIO dispatcher lock is busy"),
            Self::Poisoned => f.write_str("HVF boot-session MMIO dispatcher lock is poisoned"),
        }
    }
}

impl std::error::Error for HvfArm64BootMmioDispatcherError {}

trait BootSessionRunStepRunner {
    fn run_once_and_handle_mmio(
        &self,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
    ) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>;
}

impl BootSessionRunStepRunner for HvfVcpuRunner<'_> {
    fn run_once_and_handle_mmio(
        &self,
        dispatcher: Arc<Mutex<MmioDispatcher>>,
    ) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        HvfVcpuRunner::run_once_and_handle_mmio(self, dispatcher)
    }
}

fn run_boot_session_vcpu_step(
    runner: &impl BootSessionRunStepRunner,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
    runner.run_once_and_handle_mmio(Arc::clone(dispatcher))
}

fn lock_boot_mmio_dispatcher(
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
) -> Result<MutexGuard<'_, MmioDispatcher>, HvfArm64BootMmioDispatcherError> {
    dispatcher.try_lock().map_err(|source| match source {
        TryLockError::WouldBlock => HvfArm64BootMmioDispatcherError::Busy,
        TryLockError::Poisoned(_) => HvfArm64BootMmioDispatcherError::Poisoned,
    })
}

fn collect_block_notification_dispatches(
    dispatches: Arm64BootBlockNotificationDispatches,
) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootBlockNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootBlockNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootBlockNotificationDispatches::new(devices))
}

fn signal_block_queue_interrupts(
    dispatches: Arm64BootBlockNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootBlockNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootBlockNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootBlockNotificationDispatches::new(devices))
}

fn signal_queue_interrupt(
    line: GuestInterruptLine,
    signaler: &dyn InterruptSink,
) -> Result<(), DeviceInterruptTriggerError> {
    signaler
        .signal(line)
        .map_err(|source| DeviceInterruptTriggerError::Signal {
            line,
            kind: DeviceInterruptKind::Queue,
            source,
        })
}

#[derive(Debug)]
pub enum HvfArm64BootSessionError {
    BackendAlreadyInitialized,
    UnsupportedVcpuCount {
        vcpu_count: u8,
    },
    CreateVm {
        source: BackendError,
    },
    CreateGic {
        source: HvfGicError,
    },
    TimerMetadata {
        source: Arm64FdtError,
    },
    InterruptLineStorage {
        source: TryReserveError,
    },
    AllocateInterruptLine {
        purpose: HvfArm64BootInterruptLinePurpose,
        source: HvfInterruptLineAllocationError,
    },
    StartRunner {
        source: HvfVcpuRunnerError,
    },
    ReadMpidr {
        source: HvfVcpuRunnerError,
    },
    AssembleResources {
        source: Arm64BootResourceError,
    },
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    ConfigureBootRegisters {
        source: HvfVcpuRunnerError,
    },
}

impl fmt::Display for HvfArm64BootSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BackendAlreadyInitialized => {
                f.write_str("HVF arm64 boot session requires a backend without an existing VM")
            }
            Self::UnsupportedVcpuCount { vcpu_count } => write!(
                f,
                "HVF arm64 boot session supports exactly {SINGLE_VCPU_COUNT} vCPU, got {vcpu_count}"
            ),
            Self::CreateVm { source } => write!(f, "failed to create HVF VM: {source}"),
            Self::CreateGic { source } => write!(f, "failed to create HVF GIC: {source}"),
            Self::TimerMetadata { source } => {
                write!(
                    f,
                    "failed to convert HVF timer metadata for arm64 FDT: {source}"
                )
            }
            Self::InterruptLineStorage { source } => {
                write!(
                    f,
                    "failed to allocate HVF interrupt-line metadata: {source}"
                )
            }
            Self::AllocateInterruptLine { purpose, source } => {
                write!(
                    f,
                    "failed to allocate HVF SPI interrupt line for {purpose}: {source}"
                )
            }
            Self::StartRunner { source } => {
                write!(f, "failed to start HVF vCPU runner: {source}")
            }
            Self::ReadMpidr { source } => {
                write!(f, "failed to read primary vCPU MPIDR_EL1: {source}")
            }
            Self::AssembleResources { source } => {
                write!(f, "failed to assemble arm64 boot resources: {source}")
            }
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to map arm64 boot guest memory into HVF: {source}"
                )
            }
            Self::ConfigureBootRegisters { source } => {
                write!(
                    f,
                    "failed to configure primary HVF boot registers: {source}"
                )
            }
        }
    }
}

impl std::error::Error for HvfArm64BootSessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CreateVm { source } => Some(source),
            Self::CreateGic { source } => Some(source),
            Self::TimerMetadata { source } => Some(source),
            Self::InterruptLineStorage { source } => Some(source),
            Self::AllocateInterruptLine { source, .. } => Some(source),
            Self::StartRunner { source } => Some(source),
            Self::ReadMpidr { source } => Some(source),
            Self::AssembleResources { source } => Some(source),
            Self::MapGuestMemory { source } => Some(source),
            Self::ConfigureBootRegisters { source } => Some(source),
            Self::BackendAlreadyInitialized | Self::UnsupportedVcpuCount { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootInterruptLinePurpose {
    BlockDevice,
    SerialDevice,
}

impl fmt::Display for HvfArm64BootInterruptLinePurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockDevice => f.write_str("block device"),
            Self::SerialDevice => f.write_str("serial device"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64BootSessionShutdownError {
    Runner { source: HvfVcpuRunnerError },
    DestroyVm { source: BackendError },
}

impl fmt::Display for HvfArm64BootSessionShutdownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runner { source } => {
                write!(f, "failed to shut down HVF boot-session runner: {source}")
            }
            Self::DestroyVm { source } => {
                write!(f, "failed to destroy HVF boot-session VM: {source}")
            }
        }
    }
}

impl std::error::Error for HvfArm64BootSessionShutdownError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Runner { source } => Some(source),
            Self::DestroyVm { source } => Some(source),
        }
    }
}

#[derive(Debug)]
struct PreparedHvfArm64BootSession<'vm> {
    runner: HvfVcpuRunner<'vm>,
    mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
    runtime_resources: Arm64BootRuntimeResources,
    gic: HvfGicMetadata,
    primary_mpidr: u64,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    boot_registers: HvfArm64BootRegisters,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HvfArm64BootInterruptLines {
    block: Vec<GuestInterruptLine>,
    serial: Option<GuestInterruptLine>,
}

impl HvfBackend {
    pub fn prepare_arm64_boot_session<'vm>(
        &'vm mut self,
        controller: &VmmController,
        config: HvfArm64BootSessionConfig,
    ) -> Result<HvfArm64BootSession<'vm>, HvfArm64BootSessionError> {
        if self.has_created_vm() {
            return Err(HvfArm64BootSessionError::BackendAlreadyInitialized);
        }

        let prepared = match prepare_arm64_boot_session_parts(self, controller, config) {
            Ok(prepared) => prepared,
            Err(err) => {
                let _ = <Self as VmBackend>::destroy_vm(self);
                return Err(err);
            }
        };

        Ok(HvfArm64BootSession {
            runner: prepared.runner,
            backend: self,
            mmio_dispatcher: prepared.mmio_dispatcher,
            runtime_resources: prepared.runtime_resources,
            gic: prepared.gic,
            primary_mpidr: prepared.primary_mpidr,
            block_interrupt_lines: prepared.block_interrupt_lines,
            serial_interrupt_line: prepared.serial_interrupt_line,
            boot_registers: prepared.boot_registers,
        })
    }
}

fn prepare_arm64_boot_session_parts<'vm>(
    backend: &mut HvfBackend,
    controller: &VmmController,
    config: HvfArm64BootSessionConfig,
) -> Result<PreparedHvfArm64BootSession<'vm>, HvfArm64BootSessionError> {
    validate_single_vcpu(controller)?;

    <HvfBackend as VmBackend>::create_vm(backend)
        .map_err(|source| HvfArm64BootSessionError::CreateVm { source })?;
    let gic = *backend
        .create_gic()
        .map_err(|source| HvfArm64BootSessionError::CreateGic { source })?;
    let timer = gic
        .arm64_fdt_timer_interrupts()
        .map_err(|source| HvfArm64BootSessionError::TimerMetadata { source })?;
    let interrupt_lines = allocate_interrupt_lines(
        &gic,
        controller.drive_configs().len(),
        config.serial_device.is_some(),
    )?;

    let runner = backend
        .start_session_vcpu_runner()
        .map_err(|source| HvfArm64BootSessionError::StartRunner { source })?;
    let primary_mpidr = runner
        .mpidr_el1()
        .map_err(|source| HvfArm64BootSessionError::ReadMpidr { source })?;
    let runtime_serial = config
        .serial_device
        .zip(interrupt_lines.serial)
        .map(|(serial, interrupt_line)| serial.into_runtime(interrupt_line));
    let resources = Arm64BootResources::assemble_from_controller(
        controller,
        Arm64BootResourceConfig {
            vcpu_mpidrs: &[primary_mpidr],
            gic: gic.arm64_fdt_gic(),
            timer,
            serial_device: runtime_serial,
            block_mmio_layout: config.block_mmio_layout,
            block_interrupt_lines: &interrupt_lines.block,
        },
    )
    .map_err(|source| HvfArm64BootSessionError::AssembleResources { source })?;
    let parts = resources.into_parts();

    backend
        .map_guest_memory(parts.memory, HvfMemoryPermissions::GUEST_RAM)
        .map_err(|source| HvfArm64BootSessionError::MapGuestMemory { source })?;
    let boot_registers = HvfArm64BootRegisters {
        kernel_entry: parts.runtime.loaded_boot_source.kernel.entry_address,
        fdt_address: parts.runtime.fdt.address,
    };
    runner
        .configure_arm64_boot_registers(boot_registers)
        .map_err(|source| HvfArm64BootSessionError::ConfigureBootRegisters { source })?;

    Ok(PreparedHvfArm64BootSession {
        runner,
        mmio_dispatcher: Arc::new(Mutex::new(parts.mmio_dispatcher)),
        runtime_resources: parts.runtime,
        gic,
        primary_mpidr,
        block_interrupt_lines: interrupt_lines.block,
        serial_interrupt_line: interrupt_lines.serial,
        boot_registers,
    })
}

fn validate_single_vcpu(controller: &VmmController) -> Result<(), HvfArm64BootSessionError> {
    let vcpu_count = controller.machine_config().vcpu_count();
    if vcpu_count == SINGLE_VCPU_COUNT {
        Ok(())
    } else {
        Err(HvfArm64BootSessionError::UnsupportedVcpuCount { vcpu_count })
    }
}

fn allocate_interrupt_lines(
    gic: &HvfGicMetadata,
    block_device_count: usize,
    serial_configured: bool,
) -> Result<HvfArm64BootInterruptLines, HvfArm64BootSessionError> {
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(gic).map_err(|source| {
        HvfArm64BootSessionError::AllocateInterruptLine {
            purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
            source,
        }
    })?;
    let mut block = Vec::new();
    block
        .try_reserve_exact(block_device_count)
        .map_err(|source| HvfArm64BootSessionError::InterruptLineStorage { source })?;

    for _ in 0..block_device_count {
        block.push(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
                source,
            }
        })?);
    }

    let serial = if serial_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::SerialDevice,
                source,
            }
        })?)
    } else {
        None
    };

    Ok(HvfArm64BootInterruptLines { block, serial })
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{
        BlockMmioLayout, DriveConfigInput, VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
        VIRTIO_BLOCK_REQUEST_TYPE_FLUSH, VIRTIO_BLOCK_REQUEST_TYPE_IN, VIRTIO_BLOCK_SECTOR_SIZE,
        VIRTIO_BLOCK_STATUS_OK, VIRTIO_BLOCK_STATUS_SIZE,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::fdt::{Arm64FdtGic, Arm64FdtRegion, Arm64FdtTimerInterrupts};
    use bangbang_runtime::interrupt::{
        DeviceInterruptKind, GuestInterruptLine, InterruptSignalError, InterruptSink,
    };
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory};
    use bangbang_runtime::mmio::{
        MmioAccess, MmioAccessBytes, MmioDispatchOutcome, MmioDispatcher, MmioHandler,
        MmioHandlerError, MmioOperation, MmioRegionId,
    };
    use bangbang_runtime::serial::SharedSerialOutputBuffer;
    use bangbang_runtime::startup::{
        Arm64BootBlockNotificationDispatches, Arm64BootResourceConfig, Arm64BootResources,
        Arm64BootRuntimeResources,
    };
    use bangbang_runtime::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK, VirtioMmioRegister,
    };
    use bangbang_runtime::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
    };

    use super::{
        HvfArm64BootBlockNotificationDispatchError, HvfArm64BootInterruptLinePurpose,
        HvfArm64BootMmioDispatcherError, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSessionError, allocate_interrupt_lines, collect_block_notification_dispatches,
        lock_boot_mmio_dispatcher, run_boot_session_vcpu_step, signal_block_queue_interrupts,
        validate_single_vcpu,
    };
    use crate::gic::{HvfGicInterruptRange, HvfGicMetadata, HvfGicRedistributor, HvfGicRegion};
    use crate::runner::{HvfVcpuRunStepOutcome, HvfVcpuRunnerError};

    static NEXT_TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    const TEST_MEMORY_MIB: u64 = 8;
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;
    const TEST_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
    const TEST_QUEUE_SIZE: u16 = 4;
    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8040_0000);
    const TEST_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8041_0000);
    const TEST_USED_RING: GuestAddress = GuestAddress::new(0x8042_0000);
    const HEADER_ADDR: GuestAddress = GuestAddress::new(0x8043_0000);
    const DATA_ADDR: GuestAddress = GuestAddress::new(0x8044_0000);
    const STATUS_ADDR: GuestAddress = GuestAddress::new(0x8045_0000);
    const TEST_AVAILABLE_RING_IDX_OFFSET: u64 = 2;
    const TEST_AVAILABLE_RING_RING_OFFSET: u64 = 4;
    const TEST_AVAILABLE_RING_ENTRY_SIZE: u64 = 2;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;

    struct TempFile {
        path: PathBuf,
    }

    impl TempFile {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    #[derive(Debug, Clone)]
    struct RecordingSink {
        lines: Arc<Mutex<Vec<GuestInterruptLine>>>,
        result: Result<(), InterruptSignalError>,
    }

    impl RecordingSink {
        fn successful() -> (Arc<Mutex<Vec<GuestInterruptLine>>>, Arc<dyn InterruptSink>) {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::new(Self {
                lines: Arc::clone(&lines),
                result: Ok(()),
            });

            (lines, sink)
        }

        fn failing(
            message: &'static str,
        ) -> (Arc<Mutex<Vec<GuestInterruptLine>>>, Arc<dyn InterruptSink>) {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let sink = Arc::new(Self {
                lines: Arc::clone(&lines),
                result: Err(InterruptSignalError::new(message)),
            });

            (lines, sink)
        }
    }

    impl InterruptSink for RecordingSink {
        fn signal(&self, line: GuestInterruptLine) -> Result<(), InterruptSignalError> {
            self.lines
                .lock()
                .expect("recording sink lock should not be poisoned")
                .push(line);
            self.result.clone()
        }
    }

    #[derive(Debug)]
    struct WrongBlockHandler;

    impl MmioHandler for WrongBlockHandler {
        fn read(&mut self, _access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
            Err(MmioHandlerError::new("wrong handler read"))
        }

        fn write(
            &mut self,
            _access: MmioAccess,
            _data: MmioAccessBytes,
        ) -> Result<(), MmioHandlerError> {
            Err(MmioHandlerError::new("wrong handler write"))
        }
    }

    type RecordedRunStepDispatchers = Arc<Mutex<Vec<Arc<Mutex<MmioDispatcher>>>>>;

    #[derive(Debug)]
    struct RecordingBootSessionRunStepRunner {
        result: Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>,
        dispatchers: RecordedRunStepDispatchers,
    }

    impl RecordingBootSessionRunStepRunner {
        fn new(
            result: Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>,
        ) -> (Self, RecordedRunStepDispatchers) {
            let dispatchers = Arc::new(Mutex::new(Vec::new()));

            (
                Self {
                    result,
                    dispatchers: Arc::clone(&dispatchers),
                },
                dispatchers,
            )
        }
    }

    impl super::BootSessionRunStepRunner for RecordingBootSessionRunStepRunner {
        fn run_once_and_handle_mmio(
            &self,
            dispatcher: Arc<Mutex<MmioDispatcher>>,
        ) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
            self.dispatchers
                .lock()
                .expect("recorded dispatcher list should lock")
                .push(dispatcher);

            self.result.clone()
        }
    }

    fn recorded_run_step_dispatchers(
        dispatchers: &RecordedRunStepDispatchers,
    ) -> Vec<Arc<Mutex<MmioDispatcher>>> {
        dispatchers
            .lock()
            .expect("recorded dispatcher list should lock")
            .clone()
    }

    fn gic_with_spi_range(base: u32, count: u32) -> HvfGicMetadata {
        HvfGicMetadata {
            distributor: HvfGicRegion {
                base: 0x3ffe_0000,
                size: 0x1_0000,
            },
            redistributor: HvfGicRedistributor {
                region: HvfGicRegion {
                    base: 0x3ffc_0000,
                    size: 0x2_0000,
                },
                single_redistributor_size: 0x2_0000,
            },
            spi_interrupt_range: HvfGicInterruptRange { base, count },
            timer_interrupts: crate::gic::HvfGicTimerInterrupts {
                el1_virtual_timer_intid: 27,
                el1_physical_timer_intid: 30,
            },
            msi: None,
        }
    }

    fn controller_with_vcpus(vcpu_count: u8) -> bangbang_runtime::VmmController {
        let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(
                vcpu_count, 128,
            )))
            .expect("machine config should be stored");
        controller
    }

    fn line_values(lines: &[bangbang_runtime::interrupt::GuestInterruptLine]) -> Vec<u32> {
        lines.iter().map(|line| line.raw_value()).collect()
    }

    fn line(value: u32) -> GuestInterruptLine {
        GuestInterruptLine::new(value).expect("test interrupt line should be valid")
    }

    fn recorded_lines(lines: &Arc<Mutex<Vec<GuestInterruptLine>>>) -> Vec<u32> {
        lines
            .lock()
            .expect("recorded interrupt lines should be readable")
            .iter()
            .map(|line| line.raw_value())
            .collect()
    }

    fn temp_path(name: &str) -> PathBuf {
        let id = NEXT_TEST_FILE_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "bangbang-hvf-startup-{name}-{}-{id}",
            std::process::id()
        ))
    }

    fn temp_file(name: &str, bytes: &[u8]) -> TempFile {
        let path = temp_path(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("test file should be created");
        file.write_all(bytes)
            .expect("test file bytes should be written");
        TempFile { path }
    }

    fn arm64_image() -> Vec<u8> {
        let mut bytes = vec![0xaa; ARM64_IMAGE_HEADER_SIZE];
        write_u64_le(&mut bytes, ARM64_IMAGE_TEXT_OFFSET_OFFSET, 0);
        write_u64_le(
            &mut bytes,
            ARM64_IMAGE_SIZE_OFFSET,
            ARM64_IMAGE_HEADER_SIZE as u64,
        );
        write_u32_le(&mut bytes, ARM64_IMAGE_MAGIC_OFFSET, ARM64_IMAGE_MAGIC);
        bytes
    }

    fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) {
        let end = offset + std::mem::size_of::<u64>();
        bytes[offset..end].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) {
        let end = offset + std::mem::size_of::<u32>();
        bytes[offset..end].copy_from_slice(&value.to_le_bytes());
    }

    fn controller_with_kernel(kernel: &Path) -> bangbang_runtime::VmmController {
        let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(
                1,
                TEST_MEMORY_MIB,
            )))
            .expect("machine config should be stored");
        controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                kernel.to_path_buf(),
            )))
            .expect("boot source should be stored");
        controller
    }

    fn add_drive(
        controller: &mut bangbang_runtime::VmmController,
        id: &str,
        path: &Path,
        is_root_device: bool,
    ) {
        controller
            .handle_action(VmmAction::PutDrive(DriveConfigInput::new(
                id,
                id,
                path.to_path_buf(),
                is_root_device,
            )))
            .expect("drive config should be stored");
    }

    fn valid_boot_resource_config(lines: &[GuestInterruptLine]) -> Arm64BootResourceConfig<'_> {
        Arm64BootResourceConfig {
            vcpu_mpidrs: &[0],
            gic: valid_fdt_gic(),
            timer: Arm64FdtTimerInterrupts::firecracker_default(),
            serial_device: None,
            block_mmio_layout: BlockMmioLayout::new(TEST_BLOCK_MMIO_BASE, MmioRegionId::new(1)),
            block_interrupt_lines: lines,
        }
    }

    fn valid_fdt_gic() -> Arm64FdtGic {
        Arm64FdtGic {
            distributor: Arm64FdtRegion {
                base: 0x3ffc_0000,
                size: 0x1_0000,
            },
            redistributor: Arm64FdtRegion {
                base: 0x3ffd_0000,
                size: 0x2_0000,
            },
            compatibility: "arm,gic-v3",
            interrupt_cells: 3,
            maintenance_irq: 9,
            msi: None,
        }
    }

    fn boot_runtime_without_drives() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-no-drives", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_boot_resource_config(&[]),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_drives(
        drives: &[(&str, &[u8], bool)],
    ) -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-drives", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        let mut blocks = Vec::new();
        blocks
            .try_reserve_exact(drives.len())
            .expect("test block files should reserve");

        for (id, bytes, is_root_device) in drives {
            let block = temp_file(id, bytes);
            add_drive(&mut controller, id, block.path(), *is_root_device);
            blocks.push(block);
        }

        let interrupt_lines: Vec<_> = (0..drives.len())
            .map(|index| {
                line(32 + u32::try_from(index).expect("test block device index should fit in u32"))
            })
            .collect();
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_boot_resource_config(&interrupt_lines),
        )
        .expect("boot resources should assemble");
        drop(blocks);
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn dispatch_boot_block_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootBlockNotificationDispatches {
        runtime
            .dispatch_block_queue_notifications(memory, mmio_dispatcher)
            .expect("block notification dispatch result should allocate")
    }

    fn write_boot_block_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime.block_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("block MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("block MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_block_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime.block_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("block MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("block MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn configure_boot_block_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        device_ring: GuestAddress,
    ) {
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(TEST_DESCRIPTOR_TABLE),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(TEST_AVAILABLE_RING),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueReady,
            1,
        );
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_block_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        write_boot_block_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNotify,
            0,
        );
    }

    fn guest_address_low(address: GuestAddress) -> u32 {
        u32::try_from(address.raw_value()).expect("test address should fit in low queue register")
    }

    #[derive(Debug, Clone, Copy)]
    struct TestDescriptor {
        address: GuestAddress,
        len: u32,
        flags: u16,
        next: u16,
    }

    impl TestDescriptor {
        const fn readable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_NEXT, index),
                None => (0, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }

        const fn writable(address: GuestAddress, len: u32, next: Option<u16>) -> Self {
            let (flags, next_index) = match next {
                Some(index) => (VIRTQUEUE_DESC_F_WRITE | VIRTQUEUE_DESC_F_NEXT, index),
                None => (VIRTQUEUE_DESC_F_WRITE, 0),
            };
            Self {
                address,
                len,
                flags,
                next: next_index,
            }
        }
    }

    fn write_queued_read_request(memory: &mut GuestMemory) {
        write_request_header(memory, HEADER_ADDR, VIRTIO_BLOCK_REQUEST_TYPE_IN, 0);
        write_descriptor(
            memory,
            0,
            TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as u32, Some(2)),
        );
        write_descriptor(memory, 2, TestDescriptor::writable(STATUS_ADDR, 1, None));
        write_available_heads(memory, &[0]);
    }

    fn write_partially_invalid_queued_flush_request(memory: &mut GuestMemory) {
        write_request_header(memory, HEADER_ADDR, VIRTIO_BLOCK_REQUEST_TYPE_FLUSH, 0);
        write_descriptor(
            memory,
            0,
            TestDescriptor::readable(HEADER_ADDR, VIRTIO_BLOCK_REQUEST_HEADER_SIZE, Some(1)),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE, None),
        );
        write_available_heads(memory, &[0, TEST_QUEUE_SIZE]);
    }

    fn write_request_header(
        memory: &mut GuestMemory,
        address: GuestAddress,
        request_type: u32,
        sector: u64,
    ) {
        let mut bytes = [0; VIRTIO_BLOCK_REQUEST_HEADER_SIZE as usize];
        let (request_type_bytes, tail) = bytes.split_at_mut(4);
        let (_reserved, sector_bytes) = tail.split_at_mut(4);
        request_type_bytes.copy_from_slice(&request_type.to_le_bytes());
        sector_bytes.copy_from_slice(&sector.to_le_bytes());
        memory
            .write_slice(&bytes, address)
            .expect("request header should write");
    }

    fn write_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("descriptor should write");
    }

    fn write_guest_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 field should write");
    }

    fn read_guest_bytes(memory: &GuestMemory, address: GuestAddress, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest bytes should read");
        bytes
    }

    fn available_ring_idx_address() -> GuestAddress {
        TEST_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("available idx address should not overflow")
    }

    fn available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("available entry address should not overflow")
    }

    fn write_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    #[test]
    fn block_notification_signal_dispatch_accepts_empty_devices() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result = collect_block_notification_dispatches(dispatches)
            .expect("empty dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
    }

    #[test]
    fn block_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(device.dispatch().device().registration.drive_id(), "rootfs");
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn block_notification_signal_dispatch_signals_queued_request() {
        let payload = vec![0x74; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", payload.as_slice(), true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            payload
        );
        assert_eq!(
            read_guest_bytes(&memory, STATUS_ADDR, 1),
            [VIRTIO_BLOCK_STATUS_OK]
        );
    }

    #[test]
    fn block_notification_signal_dispatch_keeps_multiple_devices_independent() {
        let first_payload = [0x11; 512];
        let second_payload = vec![0x22; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_drives(&[
            ("rootfs", first_payload.as_slice(), true),
            ("data", second_payload.as_slice(), false),
        ]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("multi-device dispatch should collect");

        assert_eq!(result.len(), 2);
        let first = &result.as_slice()[0];
        let second = &result.as_slice()[1];
        assert_eq!(first.dispatch().device().registration.drive_id(), "rootfs");
        assert_eq!(second.dispatch().device().registration.drive_id(), "data");
        assert!(!first.queue_interrupt_signaled());
        assert!(second.queue_interrupt_signaled());
        assert_eq!(recorded_lines(&lines), vec![33]);
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                1,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, VIRTIO_BLOCK_SECTOR_SIZE as usize),
            second_payload
        );
    }

    #[test]
    fn block_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_partially_invalid_queued_flush_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.dispatch().outcome().dispatch_error().is_some());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_guest_bytes(&memory, STATUS_ADDR, VIRTIO_BLOCK_STATUS_SIZE as usize),
            [VIRTIO_BLOCK_STATUS_OK]
        );
    }

    #[test]
    fn block_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let payload = vec![0x74; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", payload.as_slice(), true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::failing("injected signal failure");

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_boot_block_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn block_notification_signal_dispatch_preserves_missing_handler_without_signal() {
        let (mut memory, mut runtime, _) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn block_notification_signal_dispatch_preserves_wrong_handler_without_signal() {
        let (mut memory, mut runtime, _) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        let region = runtime.block_devices[0].registration.region();
        let mut mmio_dispatcher = MmioDispatcher::new();
        mmio_dispatcher
            .insert_region(region.id(), region.range().start(), region.range().size())
            .expect("replacement region should insert");
        mmio_dispatcher
            .register_handler(region.id(), WrongBlockHandler)
            .expect("wrong handler should register");
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("wrong handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn boot_session_run_step_delegates_with_session_dispatcher() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let (runner, recorded_dispatchers) =
            RecordingBootSessionRunStepRunner::new(Ok(HvfVcpuRunStepOutcome::Canceled));

        let result = run_boot_session_vcpu_step(&runner, &dispatcher);

        assert_eq!(result, Ok(HvfVcpuRunStepOutcome::Canceled));
        let recorded = recorded_run_step_dispatchers(&recorded_dispatchers);
        assert_eq!(recorded.len(), 1);
        let recorded_dispatcher = recorded.first().expect("one dispatcher should be recorded");
        assert!(Arc::ptr_eq(recorded_dispatcher, &dispatcher));
        assert!(
            dispatcher
                .try_lock()
                .expect("delegated run step should not keep dispatcher locked")
                .regions()
                .is_empty()
        );
    }

    #[test]
    fn boot_session_run_step_preserves_runner_error() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let source = HvfVcpuRunnerError::InvalidState("fake boot-session run step failed");
        let (runner, recorded_dispatchers) =
            RecordingBootSessionRunStepRunner::new(Err(source.clone()));

        let err = run_boot_session_vcpu_step(&runner, &dispatcher)
            .expect_err("runner error should be returned");

        assert_eq!(err, source);
        let recorded = recorded_run_step_dispatchers(&recorded_dispatchers);
        assert_eq!(recorded.len(), 1);
        let recorded_dispatcher = recorded.first().expect("one dispatcher should be recorded");
        assert!(Arc::ptr_eq(recorded_dispatcher, &dispatcher));
    }

    #[test]
    fn boot_mmio_dispatcher_lock_accepts_available_dispatcher() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));

        let guard =
            lock_boot_mmio_dispatcher(&dispatcher).expect("available boot dispatcher should lock");

        assert!(guard.regions().is_empty());
    }

    #[test]
    fn boot_mmio_dispatcher_lock_reports_busy_dispatcher() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let _held = dispatcher
            .lock()
            .expect("test dispatcher lock should be acquired");

        let err = lock_boot_mmio_dispatcher(&dispatcher)
            .expect_err("already-held dispatcher lock should be busy");

        assert_eq!(err, HvfArm64BootMmioDispatcherError::Busy);
    }

    #[test]
    fn boot_mmio_dispatcher_lock_reports_poisoned_dispatcher() {
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let poisoned_dispatcher = Arc::clone(&dispatcher);
        let thread = thread::spawn(move || {
            let _held = poisoned_dispatcher
                .lock()
                .expect("test dispatcher lock should be acquired");
            panic!("poison test dispatcher lock");
        });

        assert!(thread.join().is_err());
        let err = lock_boot_mmio_dispatcher(&dispatcher)
            .expect_err("poisoned dispatcher lock should fail");

        assert_eq!(err, HvfArm64BootMmioDispatcherError::Poisoned);
    }

    #[test]
    fn displays_block_notification_dispatch_errors() {
        let err = HvfArm64BootBlockNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for block notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootBlockNotificationDispatchError::MmioDispatcher {
            source: HvfArm64BootMmioDispatcherError::Busy,
        };
        assert_eq!(
            err.to_string(),
            "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("HVF boot-session MMIO dispatcher lock is busy".to_string())
        );
    }

    #[test]
    fn session_config_stores_serial_device() {
        let serial = HvfArm64BootSerialDeviceConfig::new(
            MmioRegionId::new(7),
            GuestAddress::new(0x4000_0000),
            SharedSerialOutputBuffer::default(),
        );

        let config = HvfArm64BootSessionConfig::new(BlockMmioLayout::new(
            GuestAddress::new(0x5000_0000),
            MmioRegionId::new(1),
        ))
        .with_serial_device(serial);

        assert!(config.serial_device.is_some());
    }

    #[test]
    fn single_vcpu_validation_accepts_default_controller() {
        let controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");

        assert!(validate_single_vcpu(&controller).is_ok());
    }

    #[test]
    fn single_vcpu_validation_rejects_multi_vcpu_controller() {
        let controller = controller_with_vcpus(2);

        assert!(matches!(
            validate_single_vcpu(&controller),
            Err(HvfArm64BootSessionError::UnsupportedVcpuCount { vcpu_count: 2 })
        ));
    }

    #[test]
    fn interrupt_lines_allocate_blocks_before_serial() {
        let lines = allocate_interrupt_lines(&gic_with_spi_range(32, 4), 2, true)
            .expect("interrupt lines should allocate");

        assert_eq!(line_values(&lines.block), vec![32, 33]);
        assert_eq!(lines.serial.map(|line| line.raw_value()), Some(34));
    }

    #[test]
    fn interrupt_lines_allocate_none_for_absent_serial() {
        let lines = allocate_interrupt_lines(&gic_with_spi_range(40, 2), 2, false)
            .expect("interrupt lines should allocate");

        assert_eq!(line_values(&lines.block), vec![40, 41]);
        assert_eq!(lines.serial, None);
    }

    #[test]
    fn interrupt_lines_report_serial_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(&gic_with_spi_range(32, 1), 1, true)
            .expect_err("serial allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::SerialDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_reject_invalid_gic_range() {
        let err = allocate_interrupt_lines(&gic_with_spi_range(31, 1), 0, false)
            .expect_err("invalid SPI range should fail");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
                ..
            }
        ));
    }

    #[test]
    fn instance_start_remains_unsupported() {
        let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                "/tmp/vmlinux",
            )))
            .expect("boot source config should be stored");

        let err = controller
            .handle_action(VmmAction::InstanceStart)
            .expect_err("instance start must remain unsupported");

        assert_eq!(
            err.to_string(),
            "The requested operation is not supported: InstanceStart"
        );
    }
}
