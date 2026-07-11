//! Internal HVF arm64 boot-session preparation.

use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, Write as _};
use std::num::NonZeroUsize;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use bangbang_runtime::balloon::{
    BalloonMmioLayout, BalloonUpdateError, VirtioBalloonDeviceNotificationError,
};
use bangbang_runtime::block::BlockMmioLayout;
use bangbang_runtime::boot_timer::{
    BootTimerMmioLayout, BootTimerMmioRegistrationError, register_boot_timer_mmio,
};
use bangbang_runtime::entropy::{EntropyMmioLayout, VirtioRngOsEntropySource};
use bangbang_runtime::fdt::Arm64FdtError;
use bangbang_runtime::interrupt::{
    DeviceInterruptKind, DeviceInterruptTriggerError, GuestInterruptLine, InterruptSink,
};
use bangbang_runtime::memory::{GuestAddress, GuestMemory};
use bangbang_runtime::memory_hotplug::{
    MemoryHotplugConfig, MemoryHotplugSizeUpdate, MemoryHotplugStatus, MemoryHotplugStatusError,
    MemoryHotplugUpdateError, VirtioMemMmioLayout, VirtioMemMutationExecutor,
};
use bangbang_runtime::metrics::{
    SharedBalloonDeviceMetrics, SharedBlockDeviceMetricsRegistry, SharedEntropyDeviceMetrics,
    SharedNetworkInterfaceMetricsRegistry, SharedPmemDeviceMetricsRegistry, SharedRtcDeviceMetrics,
    SharedVsockDeviceMetrics,
};
use bangbang_runtime::mmio::{MmioDispatcher, MmioRegionId};
use bangbang_runtime::network::NetworkMmioLayout;
use bangbang_runtime::pmem::{PmemMmioLayout, VirtioPmemFlushStatus};
use bangbang_runtime::rtc::RtcMmioLayout;
use bangbang_runtime::serial::SharedSerialOutput;
use bangbang_runtime::startup::{
    Arm64BootBalloonNotificationDispatch, Arm64BootBalloonNotificationDispatchError,
    Arm64BootBalloonNotificationDispatches, Arm64BootBlockNotificationDispatch,
    Arm64BootBlockNotificationDispatchError, Arm64BootBlockNotificationDispatches,
    Arm64BootEntropyDeviceConfig as RuntimeArm64BootEntropyDeviceConfig,
    Arm64BootEntropyNotificationDispatch, Arm64BootEntropyNotificationDispatchError,
    Arm64BootEntropyNotificationDispatches, Arm64BootEntropySourceProvider,
    Arm64BootMemoryHotplugDeviceConfig as RuntimeArm64BootMemoryHotplugDeviceConfig,
    Arm64BootMemoryHotplugNotificationDispatch, Arm64BootMemoryHotplugNotificationDispatchError,
    Arm64BootMemoryHotplugNotificationDispatches, Arm64BootNetworkNotificationDispatch,
    Arm64BootNetworkNotificationDispatchError, Arm64BootNetworkNotificationDispatches,
    Arm64BootNetworkPacketIoProvider, Arm64BootPmemNotificationDispatch,
    Arm64BootPmemNotificationDispatchError, Arm64BootPmemNotificationDispatches,
    Arm64BootResourceConfig, Arm64BootResourceError, Arm64BootResourceParts, Arm64BootResources,
    Arm64BootRtcDeviceConfig as RuntimeArm64BootRtcDeviceConfig, Arm64BootRuntimeResources,
    Arm64BootSerialDeviceConfig as RuntimeArm64BootSerialDeviceConfig,
    Arm64BootVsockNotificationDispatch, Arm64BootVsockNotificationDispatchError,
    Arm64BootVsockNotificationDispatches, Arm64BootVsockWakeupFdsError,
    memory_hotplug_status_for_device, update_memory_hotplug_config_for_device,
};
use bangbang_runtime::vsock::VsockMmioLayout;
use bangbang_runtime::{BackendError, VmBackend, VmmController};

use crate::backend::HvfBackend;
use crate::gic::{
    HvfArm64GicIccRegisterState, HvfGicDeviceState, HvfGicError, HvfGicInterruptLineAllocator,
    HvfGicMetadata, HvfGicSpiSignalError, HvfGicSpiSignaler, HvfInterruptLineAllocationError,
};
use crate::memory::{HvfGuestMemoryMappingError, HvfMemoryPermissions};
use crate::runner::{
    HvfVcpuRunCancelHandle, HvfVcpuRunStepOutcome, HvfVcpuRunner, HvfVcpuRunnerError,
};
use crate::vcpu::{
    HvfArm64BootRegisters, HvfArm64VcpuBreakpointRegisterState,
    HvfArm64VcpuCacheSelectionRegisterState, HvfArm64VcpuCoreSystemRegisterState,
    HvfArm64VcpuDebugControlRegisterState, HvfArm64VcpuDebugTrapState,
    HvfArm64VcpuExceptionRegisterState, HvfArm64VcpuExecutionControlRegisterState,
    HvfArm64VcpuGeneralRegisterState, HvfArm64VcpuIdentificationRegisterState,
    HvfArm64VcpuPendingInterruptState, HvfArm64VcpuPhysicalTimerState,
    HvfArm64VcpuPointerAuthenticationKeyState, HvfArm64VcpuSimdFpState,
    HvfArm64VcpuSveSmeIdentificationRegisterState, HvfArm64VcpuThreadContextRegisterState,
    HvfArm64VcpuTranslationRegisterState, HvfArm64VcpuVirtualTimerState,
    HvfArm64VcpuWatchpointRegisterState,
};

const SINGLE_VCPU_COUNT: u8 = 1;
const BLOCK_RETRY_WAKEUP_SCHEDULER_THREAD_NAME: &str = "bangbang-hvf-block-retry-wakeup";
const ENTROPY_RETRY_WAKEUP_SCHEDULER_THREAD_NAME: &str = "bangbang-hvf-entropy-retry-wakeup";
const VSOCK_WAKEUP_MONITOR_THREAD_NAME: &str = "bangbang-hvf-vsock-wakeup";
const VSOCK_WAKEUP_MONITOR_STOP_BYTE: [u8; 1] = [0];
const POLL_FOREVER: libc::c_int = -1;

#[derive(Debug, Clone)]
pub struct HvfArm64BootSessionConfig {
    pub block_mmio_layout: BlockMmioLayout,
    pub pmem_mmio_layout: PmemMmioLayout,
    pub network_mmio_layout: NetworkMmioLayout,
    pub vsock_mmio_layout: VsockMmioLayout,
    pub rtc_mmio_layout: RtcMmioLayout,
    pub balloon_device: Option<HvfArm64BootBalloonDeviceConfig>,
    pub boot_timer_device: Option<HvfArm64BootTimerDeviceConfig>,
    pub entropy_device: Option<HvfArm64BootEntropyDeviceConfig>,
    pub memory_hotplug_device: Option<HvfArm64BootMemoryHotplugDeviceConfig>,
    pub serial_device: Option<HvfArm64BootSerialDeviceConfig>,
}

impl HvfArm64BootSessionConfig {
    pub const fn new(
        block_mmio_layout: BlockMmioLayout,
        pmem_mmio_layout: PmemMmioLayout,
        network_mmio_layout: NetworkMmioLayout,
        vsock_mmio_layout: VsockMmioLayout,
        rtc_mmio_layout: RtcMmioLayout,
    ) -> Self {
        Self {
            block_mmio_layout,
            pmem_mmio_layout,
            network_mmio_layout,
            vsock_mmio_layout,
            rtc_mmio_layout,
            balloon_device: None,
            boot_timer_device: None,
            entropy_device: None,
            memory_hotplug_device: None,
            serial_device: None,
        }
    }

    pub const fn with_balloon_device(
        mut self,
        balloon_device: HvfArm64BootBalloonDeviceConfig,
    ) -> Self {
        self.balloon_device = Some(balloon_device);
        self
    }

    pub const fn with_entropy_device(
        mut self,
        entropy_device: HvfArm64BootEntropyDeviceConfig,
    ) -> Self {
        self.entropy_device = Some(entropy_device);
        self
    }

    pub const fn with_memory_hotplug_device(
        mut self,
        memory_hotplug_device: HvfArm64BootMemoryHotplugDeviceConfig,
    ) -> Self {
        self.memory_hotplug_device = Some(memory_hotplug_device);
        self
    }

    pub const fn with_boot_timer_device(
        mut self,
        boot_timer_device: HvfArm64BootTimerDeviceConfig,
    ) -> Self {
        self.boot_timer_device = Some(boot_timer_device);
        self
    }

    pub fn with_serial_device(mut self, serial_device: HvfArm64BootSerialDeviceConfig) -> Self {
        self.serial_device = Some(serial_device);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootBalloonDeviceConfig {
    pub mmio_layout: BalloonMmioLayout,
}

impl HvfArm64BootBalloonDeviceConfig {
    pub const fn new(mmio_layout: BalloonMmioLayout) -> Self {
        Self { mmio_layout }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootEntropyDeviceConfig {
    pub mmio_layout: EntropyMmioLayout,
}

impl HvfArm64BootEntropyDeviceConfig {
    pub const fn new(mmio_layout: EntropyMmioLayout) -> Self {
        Self { mmio_layout }
    }

    const fn into_runtime(
        self,
        interrupt_line: GuestInterruptLine,
    ) -> RuntimeArm64BootEntropyDeviceConfig {
        RuntimeArm64BootEntropyDeviceConfig::new(self.mmio_layout, interrupt_line)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootMemoryHotplugDeviceConfig {
    pub mmio_layout: VirtioMemMmioLayout,
}

impl HvfArm64BootMemoryHotplugDeviceConfig {
    pub const fn new(mmio_layout: VirtioMemMmioLayout) -> Self {
        Self { mmio_layout }
    }

    const fn into_runtime(
        self,
        interrupt_line: GuestInterruptLine,
    ) -> RuntimeArm64BootMemoryHotplugDeviceConfig {
        RuntimeArm64BootMemoryHotplugDeviceConfig::new(self.mmio_layout, interrupt_line)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvfArm64BootTimerDeviceConfig {
    pub mmio_layout: BootTimerMmioLayout,
}

impl HvfArm64BootTimerDeviceConfig {
    pub const fn new(mmio_layout: BootTimerMmioLayout) -> Self {
        Self { mmio_layout }
    }
}

#[derive(Debug, Clone)]
pub struct HvfArm64BootSerialDeviceConfig {
    pub region_id: MmioRegionId,
    pub address: GuestAddress,
    pub output: SharedSerialOutput,
}

impl HvfArm64BootSerialDeviceConfig {
    pub fn new(region_id: MmioRegionId, address: GuestAddress, output: SharedSerialOutput) -> Self {
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
    control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    run_loop_wakeup: HvfArm64BootRunLoopWakeupToken,
    block_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    block_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    entropy_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_source: VirtioRngOsEntropySource,
    block_device_metrics: SharedBlockDeviceMetricsRegistry,
    pmem_device_metrics: SharedPmemDeviceMetricsRegistry,
    balloon_device_metrics: SharedBalloonDeviceMetrics,
    network_interface_metrics: SharedNetworkInterfaceMetricsRegistry,
    vsock_device_metrics: SharedVsockDeviceMetrics,
    entropy_device_metrics: SharedEntropyDeviceMetrics,
    gic: HvfGicMetadata,
    primary_mpidr: u64,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    pmem_interrupt_lines: Vec<GuestInterruptLine>,
    network_interrupt_lines: Vec<GuestInterruptLine>,
    vsock_interrupt_line: Option<GuestInterruptLine>,
    balloon_interrupt_line: Option<GuestInterruptLine>,
    entropy_interrupt_line: Option<GuestInterruptLine>,
    memory_hotplug_interrupt_line: Option<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    vmgenid_interrupt_line: GuestInterruptLine,
    vmclock_interrupt_line: GuestInterruptLine,
    boot_registers: HvfArm64BootRegisters,
}

#[derive(Debug)]
pub struct OwnedHvfArm64BootSession {
    runner: HvfVcpuRunner<'static>,
    backend: HvfBackend,
    mmio_dispatcher: Arc<Mutex<MmioDispatcher>>,
    runtime_resources: Arm64BootRuntimeResources,
    control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    run_loop_wakeup: HvfArm64BootRunLoopWakeupToken,
    block_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    block_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    entropy_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_source: VirtioRngOsEntropySource,
    block_device_metrics: SharedBlockDeviceMetricsRegistry,
    pmem_device_metrics: SharedPmemDeviceMetricsRegistry,
    balloon_device_metrics: SharedBalloonDeviceMetrics,
    network_interface_metrics: SharedNetworkInterfaceMetricsRegistry,
    vsock_device_metrics: SharedVsockDeviceMetrics,
    entropy_device_metrics: SharedEntropyDeviceMetrics,
    gic: HvfGicMetadata,
    primary_mpidr: u64,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    pmem_interrupt_lines: Vec<GuestInterruptLine>,
    network_interrupt_lines: Vec<GuestInterruptLine>,
    vsock_interrupt_line: Option<GuestInterruptLine>,
    balloon_interrupt_line: Option<GuestInterruptLine>,
    entropy_interrupt_line: Option<GuestInterruptLine>,
    memory_hotplug_interrupt_line: Option<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    vmgenid_interrupt_line: GuestInterruptLine,
    vmclock_interrupt_line: GuestInterruptLine,
    boot_registers: HvfArm64BootRegisters,
}

#[derive(Debug, Clone, Default)]
pub struct HvfArm64BootRunLoopStopToken {
    stop_requested: Arc<AtomicBool>,
}

impl HvfArm64BootRunLoopStopToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Relaxed);
    }

    pub fn is_stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Default)]
struct HvfArm64BootRunLoopWakeupToken {
    wakeup_requested: Arc<AtomicBool>,
}

impl HvfArm64BootRunLoopWakeupToken {
    fn request_wakeup(&self) {
        self.wakeup_requested.store(true, Ordering::Relaxed);
    }

    fn take_wakeup_request(&self) -> bool {
        self.wakeup_requested.swap(false, Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Default)]
struct HvfArm64BootLimiterRetryWakeupToken {
    wakeup_requested: Arc<AtomicBool>,
}

impl HvfArm64BootLimiterRetryWakeupToken {
    fn request_wakeup(&self) {
        self.wakeup_requested.store(true, Ordering::Relaxed);
    }

    fn take_wakeup_request(&self) -> bool {
        self.wakeup_requested.swap(false, Ordering::Relaxed)
    }
}

#[derive(Debug)]
struct HvfArm64BootLimiterRetryWakeupScheduler {
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
    thread: Option<JoinHandle<()>>,
}

impl HvfArm64BootLimiterRetryWakeupScheduler {
    fn inactive() -> Self {
        Self {
            shared: Arc::new(HvfArm64BootLimiterRetryWakeupSchedulerShared::default()),
            thread: None,
        }
    }

    fn start(
        thread_name: &'static str,
        cancel_handle: HvfVcpuRunCancelHandle,
        wakeup_token: HvfArm64BootLimiterRetryWakeupToken,
    ) -> Result<Self, io::Error> {
        Self::start_with_cancellation(thread_name, wakeup_token, move || cancel_handle.cancel())
    }

    fn start_with_cancellation<R>(
        thread_name: &'static str,
        wakeup_token: HvfArm64BootLimiterRetryWakeupToken,
        cancel_vcpu_run: impl Fn() -> R + Send + 'static,
    ) -> Result<Self, io::Error> {
        let shared = Arc::new(HvfArm64BootLimiterRetryWakeupSchedulerShared::default());
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(thread_name.to_owned())
            .spawn(move || {
                run_limiter_retry_wakeup_scheduler(thread_shared, wakeup_token, cancel_vcpu_run);
            })?;

        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    fn schedule_after(&self, retry_after: Option<Duration>) {
        if self.thread.is_none() {
            return;
        }

        let deadline = retry_after.map(limiter_retry_deadline_after);
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        if matches!(
            state.status,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped
        ) {
            return;
        }
        state.deadline = deadline;
        if retry_after.is_none() {
            state.deferred_publication = false;
        }
        self.shared.condvar.notify_one();
    }

    fn quiesce(
        &self,
    ) -> Result<
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard,
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError,
    > {
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        match state.status {
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running => {
                state.status = HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced;
            }
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced => {
                return Err(
                    HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::AlreadyQuiesced,
                );
            }
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped => {
                return Err(HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped);
            }
        }
        self.shared.condvar.notify_all();

        while state.publication_in_flight {
            state = wait_limiter_retry_wakeup_state(&self.shared, state);
            if matches!(
                state.status,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped
            ) {
                return Err(HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped);
            }
        }

        Ok(HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard {
            shared: Arc::clone(&self.shared),
        })
    }

    fn stop(&mut self) {
        {
            let mut state = lock_limiter_retry_wakeup_state(&self.shared);
            state.status = HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped;
            state.deadline = None;
            state.deferred_publication = false;
        }
        self.shared.condvar.notify_all();

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HvfArm64BootLimiterRetryWakeupScheduler {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Debug, Default)]
struct HvfArm64BootLimiterRetryWakeupSchedulerShared {
    state: Mutex<HvfArm64BootLimiterRetryWakeupSchedulerState>,
    condvar: Condvar,
}

#[derive(Debug, Default)]
struct HvfArm64BootLimiterRetryWakeupSchedulerState {
    deadline: Option<Instant>,
    status: HvfArm64BootLimiterRetryWakeupSchedulerStatus,
    publication_in_flight: bool,
    deferred_publication: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum HvfArm64BootLimiterRetryWakeupSchedulerStatus {
    #[default]
    Running,
    Quiesced,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError {
    AlreadyQuiesced,
    Stopped,
}

#[derive(Debug)]
struct HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard {
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
}

impl HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard {
    fn defer_publication(&self) {
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        if matches!(
            state.status,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced
        ) {
            state.deferred_publication = true;
        }
    }
}

impl Drop for HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard {
    fn drop(&mut self) {
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        if matches!(
            state.status,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced
        ) {
            state.status = HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running;
            self.shared.condvar.notify_all();
        }
    }
}

/// Holds block and entropy limiter retry wakeup schedulers quiesced.
///
/// Dropping the guard resumes each scheduler that has not stopped. Active
/// scheduler threads then publish any retry wakeup that became due while the
/// guard was held.
#[derive(Debug)]
#[must_use = "dropping the guard resumes limiter retry wakeup publication"]
pub struct HvfArm64BootLimiterRetryWakeupQuiescenceGuard {
    _block: HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard,
    _entropy: HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceGuard,
}

/// Describes why a boot session could not quiesce its limiter retry wakeups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootLimiterRetryWakeupQuiescenceError {
    BlockAlreadyQuiesced,
    BlockStopped,
    EntropyAlreadyQuiesced,
    EntropyStopped,
}

impl fmt::Display for HvfArm64BootLimiterRetryWakeupQuiescenceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockAlreadyQuiesced => {
                f.write_str("block limiter retry wakeups are already quiesced")
            }
            Self::BlockStopped => f.write_str("block limiter retry wakeup scheduler is stopped"),
            Self::EntropyAlreadyQuiesced => {
                f.write_str("entropy limiter retry wakeups are already quiesced")
            }
            Self::EntropyStopped => {
                f.write_str("entropy limiter retry wakeup scheduler is stopped")
            }
        }
    }
}

impl std::error::Error for HvfArm64BootLimiterRetryWakeupQuiescenceError {}

fn quiesce_limiter_retry_wakeups(
    block_retry_wakeup: &HvfArm64BootLimiterRetryWakeupToken,
    block_retry_wakeup_scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_retry_wakeup: &HvfArm64BootLimiterRetryWakeupToken,
    entropy_retry_wakeup_scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
) -> Result<
    HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
    HvfArm64BootLimiterRetryWakeupQuiescenceError,
> {
    let block = block_retry_wakeup_scheduler
        .quiesce()
        .map_err(|err| match err {
            HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::AlreadyQuiesced => {
                HvfArm64BootLimiterRetryWakeupQuiescenceError::BlockAlreadyQuiesced
            }
            HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped => {
                HvfArm64BootLimiterRetryWakeupQuiescenceError::BlockStopped
            }
        })?;
    let entropy = entropy_retry_wakeup_scheduler
        .quiesce()
        .map_err(|err| match err {
            HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::AlreadyQuiesced => {
                HvfArm64BootLimiterRetryWakeupQuiescenceError::EntropyAlreadyQuiesced
            }
            HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped => {
                HvfArm64BootLimiterRetryWakeupQuiescenceError::EntropyStopped
            }
        })?;

    if block_retry_wakeup.take_wakeup_request() {
        block.defer_publication();
    }
    if entropy_retry_wakeup.take_wakeup_request() {
        entropy.defer_publication();
    }

    Ok(HvfArm64BootLimiterRetryWakeupQuiescenceGuard {
        _block: block,
        _entropy: entropy,
    })
}

#[derive(Debug)]
struct HvfArm64BootLimiterRetryWakeupPublicationGuard {
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
}

impl Drop for HvfArm64BootLimiterRetryWakeupPublicationGuard {
    fn drop(&mut self) {
        let mut state = lock_limiter_retry_wakeup_state(&self.shared);
        state.publication_in_flight = false;
        self.shared.condvar.notify_all();
    }
}

fn limiter_retry_deadline_after(delay: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(delay).unwrap_or(now)
}

fn lock_limiter_retry_wakeup_state(
    shared: &HvfArm64BootLimiterRetryWakeupSchedulerShared,
) -> MutexGuard<'_, HvfArm64BootLimiterRetryWakeupSchedulerState> {
    match shared.state.lock() {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_limiter_retry_wakeup_state<'a>(
    shared: &'a HvfArm64BootLimiterRetryWakeupSchedulerShared,
    state: MutexGuard<'a, HvfArm64BootLimiterRetryWakeupSchedulerState>,
) -> MutexGuard<'a, HvfArm64BootLimiterRetryWakeupSchedulerState> {
    match shared.condvar.wait(state) {
        Ok(state) => state,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_limiter_retry_wakeup_state_timeout<'a>(
    shared: &'a HvfArm64BootLimiterRetryWakeupSchedulerShared,
    state: MutexGuard<'a, HvfArm64BootLimiterRetryWakeupSchedulerState>,
    timeout: Duration,
) -> MutexGuard<'a, HvfArm64BootLimiterRetryWakeupSchedulerState> {
    match shared.condvar.wait_timeout(state, timeout) {
        Ok((state, _)) => state,
        Err(poisoned) => poisoned.into_inner().0,
    }
}

fn wait_to_publish_limiter_retry_wakeup(
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
) -> bool {
    let mut state = lock_limiter_retry_wakeup_state(&shared);
    loop {
        match state.status {
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running => {}
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced => {
                state = wait_limiter_retry_wakeup_state(&shared, state);
                continue;
            }
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped => return false,
        }

        let now = Instant::now();
        if state.deferred_publication {
            state.deferred_publication = false;
            if state.deadline.is_some_and(|deadline| deadline <= now) {
                state.deadline = None;
            }
            state.publication_in_flight = true;
            return true;
        }

        let Some(deadline) = state.deadline else {
            state = wait_limiter_retry_wakeup_state(&shared, state);
            continue;
        };

        if deadline <= now {
            state.deadline = None;
            state.publication_in_flight = true;
            return true;
        }

        state = wait_limiter_retry_wakeup_state_timeout(
            &shared,
            state,
            deadline.saturating_duration_since(now),
        );
    }
}

fn run_limiter_retry_wakeup_scheduler<R>(
    shared: Arc<HvfArm64BootLimiterRetryWakeupSchedulerShared>,
    wakeup_token: HvfArm64BootLimiterRetryWakeupToken,
    cancel_vcpu_run: impl Fn() -> R,
) {
    while wait_to_publish_limiter_retry_wakeup(Arc::clone(&shared)) {
        let _publication = HvfArm64BootLimiterRetryWakeupPublicationGuard {
            shared: Arc::clone(&shared),
        };
        wakeup_token.request_wakeup();
        let _cancel_result = cancel_vcpu_run();
    }
}

#[derive(Debug, Clone, Default)]
struct HvfArm64BootRunLoopControlWakeupToken {
    wakeup_requested: Arc<AtomicBool>,
}

impl HvfArm64BootRunLoopControlWakeupToken {
    fn request_wakeup(&self) {
        self.wakeup_requested.store(true, Ordering::Relaxed);
    }

    fn take_wakeup_request(&self) -> bool {
        self.wakeup_requested.swap(false, Ordering::Relaxed)
    }
}

#[derive(Debug, Clone)]
pub struct HvfArm64BootRunLoopControl {
    stop_token: HvfArm64BootRunLoopStopToken,
    control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    cancel_handle: HvfVcpuRunCancelHandle,
}

impl HvfArm64BootRunLoopControl {
    fn new(
        cancel_handle: HvfVcpuRunCancelHandle,
        control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    ) -> Self {
        Self {
            stop_token: HvfArm64BootRunLoopStopToken::new(),
            control_wakeup,
            cancel_handle,
        }
    }

    pub fn stop_token(&self) -> HvfArm64BootRunLoopStopToken {
        self.stop_token.clone()
    }

    pub fn request_stop(&self) -> Result<(), HvfVcpuRunnerError> {
        self.stop_token.request_stop();
        self.cancel_handle.cancel()
    }

    /// Wake the boot run loop without requesting guest shutdown.
    ///
    /// This is runner-command plumbing for future runtime device updates. It
    /// lets the process worker regain control while keeping stop semantics
    /// separate from ordinary command dispatch.
    pub fn request_wakeup(&self) -> Result<(), HvfVcpuRunnerError> {
        self.control_wakeup.request_wakeup();
        if let Err(source) = self.cancel_handle.cancel() {
            let _ = self.control_wakeup.take_wakeup_request();
            return Err(source);
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HvfArm64BootRunLoopOutcome {
    StepLimitReached { steps: usize },
    Wakeup { steps: usize },
    Stopped { steps: usize },
    Canceled { steps: usize },
    GuestShutdown { steps: usize },
    GuestReset { steps: usize },
    Unknown { steps: usize, reason: u32 },
}

#[derive(Debug)]
pub enum HvfArm64BootRunLoopError {
    StartVsockWakeupMonitor {
        steps_completed: usize,
        source: Box<HvfArm64BootRunLoopWakeupMonitorError>,
    },
    RunStep {
        steps_completed: usize,
        source: Box<HvfVcpuRunnerError>,
    },
    StopVsockWakeupMonitor {
        steps_completed: usize,
        source: Box<HvfArm64BootRunLoopWakeupMonitorError>,
    },
    DispatchBlockNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootBlockNotificationDispatchError>,
    },
    DispatchPmemNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootPmemNotificationDispatchError>,
    },
    DispatchNetworkNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootNetworkNotificationDispatchError>,
    },
    DispatchVsockNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootVsockNotificationDispatchError>,
    },
    DispatchBalloonNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootBalloonNotificationDispatchError>,
    },
    DispatchMemoryHotplugNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootMemoryHotplugNotificationDispatchError>,
    },
    DispatchEntropyNotifications {
        steps_completed: usize,
        source: Box<HvfArm64BootEntropyNotificationDispatchError>,
    },
    HandleVirtualTimer {
        steps_completed: usize,
        source: Box<HvfVcpuRunnerError>,
    },
}

impl fmt::Display for HvfArm64BootRunLoopError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StartVsockWakeupMonitor {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to start HVF boot-session vsock wakeup monitor after {steps_completed} completed steps: {source}"
            ),
            Self::RunStep {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to run HVF boot-session vCPU step after {steps_completed} completed steps: {source}"
            ),
            Self::StopVsockWakeupMonitor {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to stop HVF boot-session vsock wakeup monitor after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchBlockNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session block notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchPmemNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session pmem notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchNetworkNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session network notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchVsockNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session vsock notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchBalloonNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session balloon notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchMemoryHotplugNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session memory-hotplug notifications after {steps_completed} completed steps: {source}"
            ),
            Self::DispatchEntropyNotifications {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to dispatch HVF boot-session entropy notifications after {steps_completed} completed steps: {source}"
            ),
            Self::HandleVirtualTimer {
                steps_completed,
                source,
            } => write!(
                f,
                "failed to handle HVF boot-session virtual timer after {steps_completed} completed steps: {source}"
            ),
        }
    }
}

impl std::error::Error for HvfArm64BootRunLoopError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::StartVsockWakeupMonitor { source, .. } => Some(source.as_ref()),
            Self::RunStep { source, .. } => Some(source.as_ref()),
            Self::StopVsockWakeupMonitor { source, .. } => Some(source.as_ref()),
            Self::DispatchBlockNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchPmemNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchNetworkNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchVsockNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchBalloonNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchMemoryHotplugNotifications { source, .. } => Some(source.as_ref()),
            Self::DispatchEntropyNotifications { source, .. } => Some(source.as_ref()),
            Self::HandleVirtualTimer { source, .. } => Some(source.as_ref()),
        }
    }
}

#[derive(Debug)]
pub enum HvfArm64BootRunLoopWakeupMonitorError {
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    CollectVsockWakeupFds {
        source: Arm64BootVsockWakeupFdsError,
    },
    PollFdAllocation {
        source: TryReserveError,
    },
    TooManyPollFds {
        count: usize,
    },
    CreateStopPipe {
        source: io::ErrorKind,
    },
    ThreadSpawn {
        source: io::Error,
    },
    StopSignal {
        source: io::ErrorKind,
    },
    ThreadPanicked,
}

impl fmt::Display for HvfArm64BootRunLoopWakeupMonitorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MmioDispatcher { source } => {
                write!(
                    f,
                    "failed to lock HVF boot-session MMIO dispatcher: {source}"
                )
            }
            Self::CollectVsockWakeupFds { source } => {
                write!(f, "failed to collect boot vsock wakeup fds: {source}")
            }
            Self::PollFdAllocation { source } => {
                write!(
                    f,
                    "failed to allocate boot vsock wakeup poll fd list: {source}"
                )
            }
            Self::TooManyPollFds { count } => {
                write!(f, "too many boot vsock wakeup poll fds: {count}")
            }
            Self::CreateStopPipe { source } => {
                write!(f, "failed to create boot vsock wakeup stop pipe: {source}")
            }
            Self::ThreadSpawn { source } => {
                write!(f, "failed to spawn boot vsock wakeup monitor: {source}")
            }
            Self::StopSignal { source } => {
                write!(
                    f,
                    "failed to signal boot vsock wakeup monitor stop: {source}"
                )
            }
            Self::ThreadPanicked => f.write_str("boot vsock wakeup monitor thread panicked"),
        }
    }
}

impl std::error::Error for HvfArm64BootRunLoopWakeupMonitorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::CollectVsockWakeupFds { source } => Some(source),
            Self::PollFdAllocation { source } => Some(source),
            Self::ThreadSpawn { source } => Some(source),
            Self::MmioDispatcher { .. }
            | Self::TooManyPollFds { .. }
            | Self::CreateStopPipe { .. }
            | Self::StopSignal { .. }
            | Self::ThreadPanicked => None,
        }
    }
}

impl HvfArm64BootSession<'_> {
    /// Quiesce block and entropy limiter retry wakeup publication.
    pub fn quiesce_limiter_retry_wakeups(
        &self,
    ) -> Result<
        HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        HvfArm64BootLimiterRetryWakeupQuiescenceError,
    > {
        quiesce_limiter_retry_wakeups(
            &self.block_retry_wakeup,
            &self.block_retry_wakeup_scheduler,
            &self.entropy_retry_wakeup,
            &self.entropy_retry_wakeup_scheduler,
        )
    }

    pub fn shutdown(&mut self) -> Result<(), HvfArm64BootSessionShutdownError> {
        self.block_retry_wakeup_scheduler.stop();
        self.entropy_retry_wakeup_scheduler.stop();
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

    pub fn shared_balloon_device_metrics(&self) -> SharedBalloonDeviceMetrics {
        self.balloon_device_metrics.clone()
    }

    pub fn shared_block_device_metrics(&self) -> SharedBlockDeviceMetricsRegistry {
        self.block_device_metrics.clone()
    }

    pub fn shared_pmem_device_metrics(&self) -> SharedPmemDeviceMetricsRegistry {
        self.pmem_device_metrics.clone()
    }

    pub fn shared_network_interface_metrics(&self) -> SharedNetworkInterfaceMetricsRegistry {
        self.network_interface_metrics.clone()
    }

    pub fn shared_vsock_device_metrics(&self) -> SharedVsockDeviceMetrics {
        self.vsock_device_metrics.clone()
    }

    pub fn shared_entropy_device_metrics(&self) -> SharedEntropyDeviceMetrics {
        self.entropy_device_metrics.clone()
    }

    pub fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
        self.runtime_resources
            .rtc_device
            .as_ref()
            .map(|device| device.metrics.clone())
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
    /// This is startup preparation plumbing; it does not enter a continuous
    /// guest run loop or prove guest boot.
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

    pub fn pmem_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.pmem_interrupt_lines
    }

    pub fn network_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.network_interrupt_lines
    }

    pub const fn vsock_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.vsock_interrupt_line
    }

    pub const fn balloon_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.balloon_interrupt_line
    }

    pub const fn entropy_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.entropy_interrupt_line
    }

    pub const fn memory_hotplug_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.memory_hotplug_interrupt_line
    }

    pub const fn serial_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.serial_interrupt_line
    }

    pub const fn vmgenid_interrupt_line(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt_line
    }

    pub const fn vmclock_interrupt_line(&self) -> GuestInterruptLine {
        self.vmclock_interrupt_line
    }

    pub const fn boot_registers(&self) -> HvfArm64BootRegisters {
        self.boot_registers
    }

    /// Capture X0-X30, PC, and CPSR on the primary vCPU's owner thread.
    ///
    /// This is runner plumbing for later lease-owned orchestration. It does not
    /// establish snapshot readiness or return complete restorable vCPU state.
    pub fn capture_arm64_general_register_state(
        &self,
    ) -> Result<HvfArm64VcpuGeneralRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_general_register_state()
    }

    /// Capture raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// This is runner plumbing for later lease-owned orchestration. It omits
    /// the wider system-register, SIMD/FP, interrupt, and restore inventories.
    pub fn capture_arm64_core_system_register_state(
        &self,
    ) -> Result<HvfArm64VcpuCoreSystemRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_core_system_register_state()
    }

    /// Capture raw EL1 exception-register state on the primary owner thread.
    ///
    /// This subset omits vector-table memory, semantic validation,
    /// persistence, and a safe restore sequence.
    pub fn capture_arm64_exception_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExceptionRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_exception_register_state()
    }

    /// Capture raw EL1 ACTLR and CPACR controls on the primary owner thread.
    ///
    /// Complete capture requires macOS 15 and omits feature validation,
    /// persistence, and a safe restore sequence.
    pub fn capture_arm64_execution_control_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExecutionControlRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_execution_control_register_state()
    }

    /// Capture raw EL1 CSSELR cache-size selection state on the primary owner
    /// thread.
    ///
    /// This getter-only value is not cache topology and omits feature metadata,
    /// selector validation, synchronization, maintenance, and restore policy.
    pub fn capture_arm64_cache_selection_register_state(
        &self,
    ) -> Result<HvfArm64VcpuCacheSelectionRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_cache_selection_register_state()
    }

    /// Capture every implemented raw EL1 hardware-breakpoint pair on the
    /// primary owner thread.
    ///
    /// Values can contain sensitive guest addresses or identities. This
    /// getter-only subset omits watchpoints, HVF trap state, persistence,
    /// breakpoint activation, and a safe restore sequence.
    pub fn capture_arm64_breakpoint_register_state(
        &self,
    ) -> Result<HvfArm64VcpuBreakpointRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_breakpoint_register_state()
    }

    /// Capture every implemented raw EL1 hardware-watchpoint pair on the
    /// primary owner thread.
    ///
    /// Values can contain sensitive guest data addresses. This getter-only
    /// subset omits HVF trap state, persistence, watchpoint activation, and a
    /// safe restore sequence.
    pub fn capture_arm64_watchpoint_register_state(
        &self,
    ) -> Result<HvfArm64VcpuWatchpointRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_watchpoint_register_state()
    }

    /// Capture raw EL1 MDCCINT and MDSCR debug controls on the primary owner
    /// thread.
    ///
    /// This getter-only subset excludes the separately captured breakpoint,
    /// watchpoint, and HVF trap state, feature validation, persistence, debug
    /// activation, and a safe restore sequence.
    pub fn capture_arm64_debug_control_register_state(
        &self,
    ) -> Result<HvfArm64VcpuDebugControlRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_debug_control_register_state()
    }

    /// Capture Hypervisor.framework debug-trap policy on the primary owner thread.
    ///
    /// This getter-only host-policy value excludes setters, guest debug-register
    /// contents, validation, persistence, debug activation, and safe restore.
    pub fn capture_arm64_debug_trap_state(
        &self,
    ) -> Result<HvfArm64VcpuDebugTrapState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_debug_trap_state()
    }

    /// Capture guest-visible arm64 identification registers on the primary
    /// owner thread.
    ///
    /// These raw virtual-CPU/HVF compatibility inputs define no destination
    /// policy, persistence format, or mutable restore state.
    pub fn capture_arm64_identification_register_state(
        &self,
    ) -> Result<HvfArm64VcpuIdentificationRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_identification_register_state()
    }

    /// Capture optional SVE/SME identification metadata on the primary owner thread.
    ///
    /// These macOS 15.2+ ZFR0/SMFR0 compatibility inputs exclude feature masks,
    /// destination policy, SVE/SME execution state, persistence, and restore.
    pub fn capture_arm64_sve_sme_identification_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSveSmeIdentificationRegisterState, HvfVcpuRunnerError> {
        self.runner
            .capture_arm64_sve_sme_identification_register_state()
    }

    /// Capture raw EL1 translation-register state on the primary owner thread.
    ///
    /// This subset omits table memory, feature validation, TLB/cache
    /// maintenance, persistence, and a safe restore sequence.
    pub fn capture_arm64_translation_register_state(
        &self,
    ) -> Result<HvfArm64VcpuTranslationRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_translation_register_state()
    }

    /// Capture the five raw EL1 pointer-authentication keys on the primary
    /// owner thread.
    ///
    /// Debug formatting is redacted. Feature validation, persistence
    /// protection, and restore ordering remain outside this getter-only value.
    pub fn capture_arm64_pointer_authentication_key_state(
        &self,
    ) -> Result<HvfArm64VcpuPointerAuthenticationKeyState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_pointer_authentication_key_state()
    }

    /// Capture raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// These sensitive software thread-ID values can contain guest pointers.
    /// This subset omits TPIDR2_EL0, wider system state, and restore policy.
    pub fn capture_arm64_thread_context_register_state(
        &self,
    ) -> Result<HvfArm64VcpuThreadContextRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_thread_context_register_state()
    }

    /// Capture raw Q0-Q31, FPCR, and FPSR values on the primary vCPU owner
    /// thread.
    ///
    /// This baseline SIMD/FP view omits SVE/SME state, restore policy, and the
    /// wider snapshot inventory.
    pub fn capture_arm64_simd_fp_state(
        &self,
    ) -> Result<HvfArm64VcpuSimdFpState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_simd_fp_state()
    }

    /// Capture raw EL1 physical-timer state on the primary owner thread.
    ///
    /// CNTP capture requires macOS 15 and a GIC created before the vCPU. The
    /// absolute compare value has no portable restore-time adjustment policy.
    pub fn capture_arm64_physical_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuPhysicalTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_physical_timer_state()
    }

    /// Capture raw virtual-timer mask, offset, control, and compare state on the
    /// primary vCPU owner thread.
    ///
    /// This subset omits pending interrupt and GIC state, and the host-time
    /// adjustment required by a restore policy. The control status bit is
    /// derived and may change as virtual time advances.
    pub fn capture_arm64_virtual_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuVirtualTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_virtual_timer_state()
    }

    /// Capture CPU-level IRQ/FIQ pending state on the primary vCPU owner thread.
    ///
    /// HVF clears these per-run injection levels after a vCPU run returns.
    /// This subset excludes GIC/device state and snapshot restore policy.
    pub fn capture_arm64_pending_interrupt_state(
        &self,
    ) -> Result<HvfArm64VcpuPendingInterruptState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_pending_interrupt_state()
    }

    /// Capture opaque, versioned GIC device state while the primary runner is
    /// stopped.
    ///
    /// The bytes exclude vCPU-affine GIC CPU registers and are not yet a
    /// bangbang snapshot schema or validated restore input.
    pub fn capture_gic_device_state(&self) -> Result<HvfGicDeviceState, HvfVcpuRunnerError> {
        self.runner.capture_gic_device_state()
    }

    /// Capture raw EL1 GIC ICC registers on the primary vCPU owner thread.
    ///
    /// This per-vCPU value complements the opaque GIC device blob but is not a
    /// restored or persisted snapshot. EL2 ICC, ICH, and ICV state are omitted.
    pub fn capture_arm64_gic_icc_register_state(
        &self,
    ) -> Result<HvfArm64GicIccRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_gic_icc_register_state()
    }

    /// Run the boot session's primary vCPU once with runner-thread MMIO handling.
    ///
    /// This is runner-loop plumbing. It does not dispatch boot block or
    /// virtio-net TX notifications or enter a continuous guest run loop.
    pub fn run_once_and_handle_mmio(&self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        run_boot_session_vcpu_step(&self.runner, &self.mmio_dispatcher)
    }

    /// Return a handle that can request cancellation of an in-flight vCPU run step.
    ///
    /// This is runner-loop plumbing. It does not shut down the boot session or
    /// enter a continuous guest run loop.
    pub fn run_cancel_handle(&self) -> HvfVcpuRunCancelHandle {
        self.runner.run_cancel_handle()
    }

    /// Return a control handle for the bounded internal boot-session run loop.
    ///
    /// Stop and non-stop wakeup requests use the existing runner cancellation
    /// boundary. This remains internal runner-loop plumbing and does not start
    /// an unbounded guest loop.
    pub fn run_loop_control(&self) -> HvfArm64BootRunLoopControl {
        HvfArm64BootRunLoopControl::new(self.run_cancel_handle(), self.control_wakeup.clone())
    }

    /// Run bounded vCPU steps and dispatch boot block and virtio-net TX
    /// notifications plus virtio-vsock TX notifications between steps.
    ///
    /// The step limit keeps this scaffold deterministic until a later scheduler
    /// owns the continuous guest loop and timer/device policy.
    pub fn run_loop(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        run_boot_session_loop(self, stop_token, max_steps)
    }

    /// Run bounded vCPU steps and report each raw step outcome to `observe_step`.
    ///
    /// This keeps diagnostics at the same boundary as the internal boot loop:
    /// observers see the step that was returned by HVF before the loop performs
    /// follow-up timer or block/network-notification handling.
    pub fn run_loop_with_observer(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
        observe_step: impl FnMut(&HvfVcpuRunStepOutcome),
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        run_boot_session_loop_with_observer(self, stop_token, max_steps, observe_step)
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

        record_block_runtime_dispatch_metrics(&self.block_device_metrics, dispatches.as_slice());
        let result = if !dispatches.needs_queue_interrupt() {
            collect_block_notification_dispatches(dispatches)
        } else {
            match HvfGicSpiSignaler::from_metadata(&self.gic) {
                Ok(signaler) => signal_block_queue_interrupts(dispatches, &signaler),
                Err(source) => {
                    Err(HvfArm64BootBlockNotificationDispatchError::CreateSignalSink { source })
                }
            }
        };
        match &result {
            Ok(dispatches) => record_block_signal_metrics(&self.block_device_metrics, dispatches),
            Err(_) => self.block_device_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_pmem_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        dispatch_pmem_queue_notifications_and_signal_interrupts(
            self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.pmem_device_metrics,
        )
    }

    pub fn dispatch_network_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_network_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_network_runtime_dispatch_metrics(
            &self.network_interface_metrics,
            dispatches.as_slice(),
        );
        let result = collect_or_signal_network_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_network_signal_metrics(&self.network_interface_metrics, dispatches);
            }
            Err(_) => self.network_interface_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_network_queue_notifications_with_packet_io_and_signal_interrupts(
        &mut self,
        packet_io: &mut impl Arm64BootNetworkPacketIoProvider,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_network_runtime_notifications_with_packet_io(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
                packet_io,
            )?
        };

        record_network_runtime_dispatch_metrics(
            &self.network_interface_metrics,
            dispatches.as_slice(),
        );
        let result = collect_or_signal_network_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_network_signal_metrics(&self.network_interface_metrics, dispatches);
            }
            Err(_) => self.network_interface_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_vsock_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootVsockNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootVsockNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_vsock_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_vsock_runtime_dispatch_metrics(&self.vsock_device_metrics, dispatches.as_slice());
        let result = collect_or_signal_vsock_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => record_vsock_signal_metrics(&self.vsock_device_metrics, dispatches),
            Err(_) => self.vsock_device_metrics.record_muxer_event_failure(),
        }
        result
    }

    pub fn dispatch_balloon_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootBalloonNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootBalloonNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_balloon_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_balloon_runtime_dispatch_metrics(
            &self.balloon_device_metrics,
            dispatches.as_slice(),
            true,
        );
        let result = collect_or_signal_balloon_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_balloon_signal_metrics(&self.balloon_device_metrics, dispatches)
            }
            Err(_) => self.balloon_device_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_memory_hotplug_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        let dispatches = {
            let (memory, mut mutation_executor) = self
                .backend
                .mapped_guest_memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MapGuestMemory { source }
                })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_memory_hotplug_runtime_notifications_with_executor(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
                &mut mutation_executor,
            )?
        };

        collect_or_signal_memory_hotplug_queue_interrupts(dispatches, &self.gic)
    }

    pub fn update_memory_hotplug_requested_size_and_signal_interrupt(
        &mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        update_memory_hotplug_requested_size_and_signal_interrupt(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            &self.gic,
            update,
        )
    }

    pub fn memory_hotplug_status(
        &mut self,
        config: MemoryHotplugConfig,
        requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        memory_hotplug_status(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            config,
            requested_size_mib,
        )
    }

    pub fn trigger_balloon_statistics_update_and_signal_interrupts(
        &mut self,
    ) -> Result<(), BalloonUpdateError> {
        let result = (|| {
            let dispatches = {
                let memory = self
                    .backend
                    .mapped_guest_memory_mut()
                    .map_err(balloon_update_error_from_display)?;
                let mut mmio_dispatcher = lock_boot_mmio_dispatcher(&self.mmio_dispatcher)
                    .map_err(balloon_update_error_from_display)?;

                self.runtime_resources
                    .trigger_balloon_statistics_update(memory, &mut mmio_dispatcher)
                    .map_err(balloon_update_error_from_display)?
            };

            record_balloon_runtime_dispatch_metrics(
                &self.balloon_device_metrics,
                dispatches.as_slice(),
                false,
            );
            let dispatches = collect_or_signal_balloon_queue_interrupts(dispatches, &self.gic)
                .map_err(balloon_update_error_from_display)?;
            record_balloon_signal_metrics(&self.balloon_device_metrics, &dispatches);

            balloon_update_result_from_hvf_dispatches(&dispatches)
        })();
        if result.is_err() {
            self.balloon_device_metrics
                .record_statistics_update_failure();
        }

        result
    }

    pub fn dispatch_entropy_queue_notifications_and_signal_interrupts(
        &mut self,
        entropy_source: &mut impl Arm64BootEntropySourceProvider,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.entropy_device_metrics,
            entropy_source,
        )
    }
}

impl Drop for HvfArm64BootSession<'_> {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

impl OwnedHvfArm64BootSession {
    pub fn new(
        controller: &VmmController,
        config: HvfArm64BootSessionConfig,
    ) -> Result<Self, HvfArm64BootSessionError> {
        let mut backend = HvfBackend::new();
        let prepared: PreparedHvfArm64BootSession<'static> =
            match prepare_arm64_boot_session_parts(&mut backend, controller, config) {
                Ok(prepared) => prepared,
                Err(err) => {
                    let _ = <HvfBackend as VmBackend>::destroy_vm(&mut backend);
                    return Err(err);
                }
            };

        Ok(Self {
            runner: prepared.runner,
            backend,
            mmio_dispatcher: prepared.mmio_dispatcher,
            runtime_resources: prepared.runtime_resources,
            control_wakeup: prepared.control_wakeup,
            run_loop_wakeup: prepared.run_loop_wakeup,
            block_retry_wakeup: prepared.block_retry_wakeup,
            block_retry_wakeup_scheduler: prepared.block_retry_wakeup_scheduler,
            entropy_retry_wakeup: prepared.entropy_retry_wakeup,
            entropy_retry_wakeup_scheduler: prepared.entropy_retry_wakeup_scheduler,
            entropy_source: VirtioRngOsEntropySource::new(),
            block_device_metrics: prepared.block_device_metrics,
            pmem_device_metrics: prepared.pmem_device_metrics,
            balloon_device_metrics: prepared.balloon_device_metrics,
            network_interface_metrics: prepared.network_interface_metrics,
            vsock_device_metrics: prepared.vsock_device_metrics,
            entropy_device_metrics: prepared.entropy_device_metrics,
            gic: prepared.gic,
            primary_mpidr: prepared.primary_mpidr,
            block_interrupt_lines: prepared.block_interrupt_lines,
            pmem_interrupt_lines: prepared.pmem_interrupt_lines,
            network_interrupt_lines: prepared.network_interrupt_lines,
            vsock_interrupt_line: prepared.vsock_interrupt_line,
            balloon_interrupt_line: prepared.balloon_interrupt_line,
            entropy_interrupt_line: prepared.entropy_interrupt_line,
            memory_hotplug_interrupt_line: prepared.memory_hotplug_interrupt_line,
            serial_interrupt_line: prepared.serial_interrupt_line,
            vmgenid_interrupt_line: prepared.vmgenid_interrupt_line,
            vmclock_interrupt_line: prepared.vmclock_interrupt_line,
            boot_registers: prepared.boot_registers,
        })
    }

    /// Quiesce block and entropy limiter retry wakeup publication.
    pub fn quiesce_limiter_retry_wakeups(
        &self,
    ) -> Result<
        HvfArm64BootLimiterRetryWakeupQuiescenceGuard,
        HvfArm64BootLimiterRetryWakeupQuiescenceError,
    > {
        quiesce_limiter_retry_wakeups(
            &self.block_retry_wakeup,
            &self.block_retry_wakeup_scheduler,
            &self.entropy_retry_wakeup,
            &self.entropy_retry_wakeup_scheduler,
        )
    }

    pub fn shutdown(&mut self) -> Result<(), HvfArm64BootSessionShutdownError> {
        self.block_retry_wakeup_scheduler.stop();
        self.entropy_retry_wakeup_scheduler.stop();
        let runner_result = self.runner.shutdown();
        let destroy_result = <HvfBackend as VmBackend>::destroy_vm(&mut self.backend);

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

    pub fn shared_balloon_device_metrics(&self) -> SharedBalloonDeviceMetrics {
        self.balloon_device_metrics.clone()
    }

    pub fn shared_block_device_metrics(&self) -> SharedBlockDeviceMetricsRegistry {
        self.block_device_metrics.clone()
    }

    pub fn shared_pmem_device_metrics(&self) -> SharedPmemDeviceMetricsRegistry {
        self.pmem_device_metrics.clone()
    }

    pub fn shared_network_interface_metrics(&self) -> SharedNetworkInterfaceMetricsRegistry {
        self.network_interface_metrics.clone()
    }

    pub fn shared_vsock_device_metrics(&self) -> SharedVsockDeviceMetrics {
        self.vsock_device_metrics.clone()
    }

    pub fn shared_entropy_device_metrics(&self) -> SharedEntropyDeviceMetrics {
        self.entropy_device_metrics.clone()
    }

    pub fn shared_rtc_device_metrics(&self) -> Option<SharedRtcDeviceMetrics> {
        self.runtime_resources
            .rtc_device
            .as_ref()
            .map(|device| device.metrics.clone())
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
    /// This is startup preparation plumbing; it does not enter a continuous
    /// guest run loop or prove guest boot.
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

    pub fn pmem_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.pmem_interrupt_lines
    }

    pub fn network_interrupt_lines(&self) -> &[GuestInterruptLine] {
        &self.network_interrupt_lines
    }

    pub const fn vsock_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.vsock_interrupt_line
    }

    pub const fn balloon_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.balloon_interrupt_line
    }

    pub const fn entropy_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.entropy_interrupt_line
    }

    pub const fn memory_hotplug_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.memory_hotplug_interrupt_line
    }

    pub const fn serial_interrupt_line(&self) -> Option<GuestInterruptLine> {
        self.serial_interrupt_line
    }

    pub const fn vmgenid_interrupt_line(&self) -> GuestInterruptLine {
        self.vmgenid_interrupt_line
    }

    pub const fn vmclock_interrupt_line(&self) -> GuestInterruptLine {
        self.vmclock_interrupt_line
    }

    pub const fn boot_registers(&self) -> HvfArm64BootRegisters {
        self.boot_registers
    }

    /// Capture X0-X30, PC, and CPSR on the primary vCPU's owner thread.
    ///
    /// This is runner plumbing for later lease-owned orchestration. It does not
    /// establish snapshot readiness or return complete restorable vCPU state.
    pub fn capture_arm64_general_register_state(
        &self,
    ) -> Result<HvfArm64VcpuGeneralRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_general_register_state()
    }

    /// Capture raw SP_EL0, SP_EL1, ELR_EL1, and SPSR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// This is runner plumbing for later lease-owned orchestration. It omits
    /// the wider system-register, SIMD/FP, interrupt, and restore inventories.
    pub fn capture_arm64_core_system_register_state(
        &self,
    ) -> Result<HvfArm64VcpuCoreSystemRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_core_system_register_state()
    }

    /// Capture raw EL1 exception-register state on the primary owner thread.
    ///
    /// This subset omits vector-table memory, semantic validation,
    /// persistence, and a safe restore sequence.
    pub fn capture_arm64_exception_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExceptionRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_exception_register_state()
    }

    /// Capture raw EL1 ACTLR and CPACR controls on the primary owner thread.
    ///
    /// Complete capture requires macOS 15 and omits feature validation,
    /// persistence, and a safe restore sequence.
    pub fn capture_arm64_execution_control_register_state(
        &self,
    ) -> Result<HvfArm64VcpuExecutionControlRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_execution_control_register_state()
    }

    /// Capture raw EL1 CSSELR cache-size selection state on the primary owner
    /// thread.
    ///
    /// This getter-only value is not cache topology and omits feature metadata,
    /// selector validation, synchronization, maintenance, and restore policy.
    pub fn capture_arm64_cache_selection_register_state(
        &self,
    ) -> Result<HvfArm64VcpuCacheSelectionRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_cache_selection_register_state()
    }

    /// Capture every implemented raw EL1 hardware-breakpoint pair on the
    /// primary owner thread.
    ///
    /// Values can contain sensitive guest addresses or identities. This
    /// getter-only subset omits watchpoints, HVF trap state, persistence,
    /// breakpoint activation, and a safe restore sequence.
    pub fn capture_arm64_breakpoint_register_state(
        &self,
    ) -> Result<HvfArm64VcpuBreakpointRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_breakpoint_register_state()
    }

    /// Capture every implemented raw EL1 hardware-watchpoint pair on the
    /// primary owner thread.
    ///
    /// Values can contain sensitive guest data addresses. This getter-only
    /// subset omits HVF trap state, persistence, watchpoint activation, and a
    /// safe restore sequence.
    pub fn capture_arm64_watchpoint_register_state(
        &self,
    ) -> Result<HvfArm64VcpuWatchpointRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_watchpoint_register_state()
    }

    /// Capture raw EL1 MDCCINT and MDSCR debug controls on the primary owner
    /// thread.
    ///
    /// This getter-only subset excludes the separately captured breakpoint,
    /// watchpoint, and HVF trap state, feature validation, persistence, debug
    /// activation, and a safe restore sequence.
    pub fn capture_arm64_debug_control_register_state(
        &self,
    ) -> Result<HvfArm64VcpuDebugControlRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_debug_control_register_state()
    }

    /// Capture Hypervisor.framework debug-trap policy on the primary owner thread.
    ///
    /// This getter-only host-policy value excludes setters, guest debug-register
    /// contents, validation, persistence, debug activation, and safe restore.
    pub fn capture_arm64_debug_trap_state(
        &self,
    ) -> Result<HvfArm64VcpuDebugTrapState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_debug_trap_state()
    }

    /// Capture guest-visible arm64 identification registers on the primary
    /// owner thread.
    ///
    /// These raw virtual-CPU/HVF compatibility inputs define no destination
    /// policy, persistence format, or mutable restore state.
    pub fn capture_arm64_identification_register_state(
        &self,
    ) -> Result<HvfArm64VcpuIdentificationRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_identification_register_state()
    }

    /// Capture optional SVE/SME identification metadata on the primary owner thread.
    ///
    /// These macOS 15.2+ ZFR0/SMFR0 compatibility inputs exclude feature masks,
    /// destination policy, SVE/SME execution state, persistence, and restore.
    pub fn capture_arm64_sve_sme_identification_register_state(
        &self,
    ) -> Result<HvfArm64VcpuSveSmeIdentificationRegisterState, HvfVcpuRunnerError> {
        self.runner
            .capture_arm64_sve_sme_identification_register_state()
    }

    /// Capture raw EL1 translation-register state on the primary owner thread.
    ///
    /// This subset omits table memory, feature validation, TLB/cache
    /// maintenance, persistence, and a safe restore sequence.
    pub fn capture_arm64_translation_register_state(
        &self,
    ) -> Result<HvfArm64VcpuTranslationRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_translation_register_state()
    }

    /// Capture the five raw EL1 pointer-authentication keys on the primary
    /// owner thread.
    ///
    /// Debug formatting is redacted. Feature validation, persistence
    /// protection, and restore ordering remain outside this getter-only value.
    pub fn capture_arm64_pointer_authentication_key_state(
        &self,
    ) -> Result<HvfArm64VcpuPointerAuthenticationKeyState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_pointer_authentication_key_state()
    }

    /// Capture raw TPIDR_EL0, TPIDRRO_EL0, and TPIDR_EL1 values on the primary
    /// vCPU owner thread.
    ///
    /// These sensitive software thread-ID values can contain guest pointers.
    /// This subset omits TPIDR2_EL0, wider system state, and restore policy.
    pub fn capture_arm64_thread_context_register_state(
        &self,
    ) -> Result<HvfArm64VcpuThreadContextRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_thread_context_register_state()
    }

    /// Capture raw Q0-Q31, FPCR, and FPSR values on the primary vCPU owner
    /// thread.
    ///
    /// This baseline SIMD/FP view omits SVE/SME state, restore policy, and the
    /// wider snapshot inventory.
    pub fn capture_arm64_simd_fp_state(
        &self,
    ) -> Result<HvfArm64VcpuSimdFpState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_simd_fp_state()
    }

    /// Capture raw EL1 physical-timer state on the primary owner thread.
    ///
    /// CNTP capture requires macOS 15 and a GIC created before the vCPU. The
    /// absolute compare value has no portable restore-time adjustment policy.
    pub fn capture_arm64_physical_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuPhysicalTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_physical_timer_state()
    }

    /// Capture raw virtual-timer mask, offset, control, and compare state on the
    /// primary vCPU owner thread.
    ///
    /// This subset omits pending interrupt and GIC state, and the host-time
    /// adjustment required by a restore policy. The control status bit is
    /// derived and may change as virtual time advances.
    pub fn capture_arm64_virtual_timer_state(
        &self,
    ) -> Result<HvfArm64VcpuVirtualTimerState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_virtual_timer_state()
    }

    /// Capture CPU-level IRQ/FIQ pending state on the primary vCPU owner thread.
    ///
    /// HVF clears these per-run injection levels after a vCPU run returns.
    /// This subset excludes GIC/device state and snapshot restore policy.
    pub fn capture_arm64_pending_interrupt_state(
        &self,
    ) -> Result<HvfArm64VcpuPendingInterruptState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_pending_interrupt_state()
    }

    /// Capture opaque, versioned GIC device state while the primary runner is
    /// stopped.
    ///
    /// The bytes exclude vCPU-affine GIC CPU registers and are not yet a
    /// bangbang snapshot schema or validated restore input.
    pub fn capture_gic_device_state(&self) -> Result<HvfGicDeviceState, HvfVcpuRunnerError> {
        self.runner.capture_gic_device_state()
    }

    /// Capture raw EL1 GIC ICC registers on the primary vCPU owner thread.
    ///
    /// This per-vCPU value complements the opaque GIC device blob but is not a
    /// restored or persisted snapshot. EL2 ICC, ICH, and ICV state are omitted.
    pub fn capture_arm64_gic_icc_register_state(
        &self,
    ) -> Result<HvfArm64GicIccRegisterState, HvfVcpuRunnerError> {
        self.runner.capture_arm64_gic_icc_register_state()
    }

    pub fn run_once_and_handle_mmio(&self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        run_boot_session_vcpu_step(&self.runner, &self.mmio_dispatcher)
    }

    pub fn run_cancel_handle(&self) -> HvfVcpuRunCancelHandle {
        self.runner.run_cancel_handle()
    }

    pub fn run_loop_control(&self) -> HvfArm64BootRunLoopControl {
        HvfArm64BootRunLoopControl::new(self.run_cancel_handle(), self.control_wakeup.clone())
    }

    pub fn run_loop(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        run_boot_session_loop(self, stop_token, max_steps)
    }

    pub fn run_loop_with_network_packet_io(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
        packet_io: &mut impl Arm64BootNetworkPacketIoProvider,
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        let mut session = NetworkPacketIoBootSessionRunLoopSession::new(self, packet_io);
        run_boot_session_loop(&mut session, stop_token, max_steps)
    }

    pub fn run_loop_with_observer(
        &mut self,
        stop_token: &HvfArm64BootRunLoopStopToken,
        max_steps: NonZeroUsize,
        observe_step: impl FnMut(&HvfVcpuRunStepOutcome),
    ) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
        run_boot_session_loop_with_observer(self, stop_token, max_steps, observe_step)
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

        record_block_runtime_dispatch_metrics(&self.block_device_metrics, dispatches.as_slice());
        let result = if !dispatches.needs_queue_interrupt() {
            collect_block_notification_dispatches(dispatches)
        } else {
            match HvfGicSpiSignaler::from_metadata(&self.gic) {
                Ok(signaler) => signal_block_queue_interrupts(dispatches, &signaler),
                Err(source) => {
                    Err(HvfArm64BootBlockNotificationDispatchError::CreateSignalSink { source })
                }
            }
        };
        match &result {
            Ok(dispatches) => record_block_signal_metrics(&self.block_device_metrics, dispatches),
            Err(_) => self.block_device_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_pmem_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        dispatch_pmem_queue_notifications_and_signal_interrupts(
            &mut self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.pmem_device_metrics,
        )
    }

    pub fn dispatch_network_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_network_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_network_runtime_dispatch_metrics(
            &self.network_interface_metrics,
            dispatches.as_slice(),
        );
        let result = collect_or_signal_network_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_network_signal_metrics(&self.network_interface_metrics, dispatches);
            }
            Err(_) => self.network_interface_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_network_queue_notifications_with_packet_io_and_signal_interrupts(
        &mut self,
        packet_io: &mut impl Arm64BootNetworkPacketIoProvider,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_network_runtime_notifications_with_packet_io(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
                packet_io,
            )?
        };

        record_network_runtime_dispatch_metrics(
            &self.network_interface_metrics,
            dispatches.as_slice(),
        );
        let result = collect_or_signal_network_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_network_signal_metrics(&self.network_interface_metrics, dispatches);
            }
            Err(_) => self.network_interface_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_vsock_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootVsockNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootVsockNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_vsock_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_vsock_runtime_dispatch_metrics(&self.vsock_device_metrics, dispatches.as_slice());
        let result = collect_or_signal_vsock_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => record_vsock_signal_metrics(&self.vsock_device_metrics, dispatches),
            Err(_) => self.vsock_device_metrics.record_muxer_event_failure(),
        }
        result
    }

    pub fn dispatch_balloon_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        let dispatches = {
            let memory = self.backend.mapped_guest_memory_mut().map_err(|source| {
                HvfArm64BootBalloonNotificationDispatchError::MapGuestMemory { source }
            })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootBalloonNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_balloon_runtime_notifications(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
            )?
        };

        record_balloon_runtime_dispatch_metrics(
            &self.balloon_device_metrics,
            dispatches.as_slice(),
            true,
        );
        let result = collect_or_signal_balloon_queue_interrupts(dispatches, &self.gic);
        match &result {
            Ok(dispatches) => {
                record_balloon_signal_metrics(&self.balloon_device_metrics, dispatches)
            }
            Err(_) => self.balloon_device_metrics.record_event_failure(),
        }
        result
    }

    pub fn dispatch_memory_hotplug_queue_notifications_and_signal_interrupts(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        let dispatches = {
            let (memory, mut mutation_executor) = self
                .backend
                .mapped_guest_memory_and_virtio_mem_executor_mut(HvfMemoryPermissions::GUEST_RAM)
                .map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MapGuestMemory { source }
                })?;
            let mut mmio_dispatcher =
                lock_boot_mmio_dispatcher(&self.mmio_dispatcher).map_err(|source| {
                    HvfArm64BootMemoryHotplugNotificationDispatchError::MmioDispatcher { source }
                })?;

            dispatch_memory_hotplug_runtime_notifications_with_executor(
                memory,
                &mut self.runtime_resources,
                &mut mmio_dispatcher,
                &mut mutation_executor,
            )?
        };

        collect_or_signal_memory_hotplug_queue_interrupts(dispatches, &self.gic)
    }

    pub fn update_memory_hotplug_requested_size_and_signal_interrupt(
        &mut self,
        update: MemoryHotplugSizeUpdate,
    ) -> Result<(), MemoryHotplugUpdateError> {
        update_memory_hotplug_requested_size_and_signal_interrupt(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            &self.gic,
            update,
        )
    }

    pub fn memory_hotplug_status(
        &mut self,
        config: MemoryHotplugConfig,
        requested_size_mib: u64,
    ) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
        memory_hotplug_status(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            config,
            requested_size_mib,
        )
    }

    pub fn trigger_balloon_statistics_update_and_signal_interrupts(
        &mut self,
    ) -> Result<(), BalloonUpdateError> {
        let result = (|| {
            let dispatches = {
                let memory = self
                    .backend
                    .mapped_guest_memory_mut()
                    .map_err(balloon_update_error_from_display)?;
                let mut mmio_dispatcher = lock_boot_mmio_dispatcher(&self.mmio_dispatcher)
                    .map_err(balloon_update_error_from_display)?;

                self.runtime_resources
                    .trigger_balloon_statistics_update(memory, &mut mmio_dispatcher)
                    .map_err(balloon_update_error_from_display)?
            };

            record_balloon_runtime_dispatch_metrics(
                &self.balloon_device_metrics,
                dispatches.as_slice(),
                false,
            );
            let dispatches = collect_or_signal_balloon_queue_interrupts(dispatches, &self.gic)
                .map_err(balloon_update_error_from_display)?;
            record_balloon_signal_metrics(&self.balloon_device_metrics, &dispatches);

            balloon_update_result_from_hvf_dispatches(&dispatches)
        })();
        if result.is_err() {
            self.balloon_device_metrics
                .record_statistics_update_failure();
        }

        result
    }

    pub fn dispatch_entropy_queue_notifications_and_signal_interrupts(
        &mut self,
        entropy_source: &mut impl Arm64BootEntropySourceProvider,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            &mut self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.entropy_device_metrics,
            entropy_source,
        )
    }
}

impl BootSessionRunLoopSession for OwnedHvfArm64BootSession {
    fn start_run_loop_vsock_wakeup_monitor(
        &mut self,
    ) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
        start_run_loop_vsock_wakeup_monitor(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            self.run_cancel_handle(),
            self.run_loop_wakeup.clone(),
        )
    }

    fn take_run_loop_wakeup_request(&mut self) -> bool {
        self.run_loop_wakeup.take_wakeup_request()
    }

    fn take_run_loop_control_wakeup_request(&mut self) -> bool {
        self.control_wakeup.take_wakeup_request()
    }

    fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
        self.block_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_block_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.block_retry_wakeup.take_wakeup_request();
        }
        self.block_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
        self.entropy_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_entropy_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.entropy_retry_wakeup.take_wakeup_request();
        }
        self.entropy_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn run_loop_vcpu_step(&mut self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        self.run_once_and_handle_mmio()
    }

    fn handle_run_loop_virtual_timer(&mut self) -> Result<(), HvfVcpuRunnerError> {
        let intid = self.gic.timer_interrupts.el1_virtual_timer_intid;
        self.runner.set_gic_ppi_pending(intid)
    }

    fn dispatch_run_loop_block_notifications(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>
    {
        self.dispatch_block_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_pmem_notifications(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        self.dispatch_pmem_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_network_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        self.dispatch_network_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_vsock_notifications(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        self.dispatch_vsock_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_balloon_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        self.dispatch_balloon_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_memory_hotplug_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        self.dispatch_memory_hotplug_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_entropy_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            &mut self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.entropy_device_metrics,
            &mut self.entropy_source,
        )
    }
}

struct NetworkPacketIoBootSessionRunLoopSession<'session, 'packet_io, P>
where
    P: Arm64BootNetworkPacketIoProvider,
{
    session: &'session mut OwnedHvfArm64BootSession,
    packet_io: &'packet_io mut P,
}

impl<'session, 'packet_io, P> NetworkPacketIoBootSessionRunLoopSession<'session, 'packet_io, P>
where
    P: Arm64BootNetworkPacketIoProvider,
{
    const fn new(
        session: &'session mut OwnedHvfArm64BootSession,
        packet_io: &'packet_io mut P,
    ) -> Self {
        Self { session, packet_io }
    }
}

impl<P> BootSessionRunLoopSession for NetworkPacketIoBootSessionRunLoopSession<'_, '_, P>
where
    P: Arm64BootNetworkPacketIoProvider,
{
    fn start_run_loop_vsock_wakeup_monitor(
        &mut self,
    ) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
        self.session.start_run_loop_vsock_wakeup_monitor()
    }

    fn take_run_loop_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_wakeup_request()
    }

    fn take_run_loop_control_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_control_wakeup_request()
    }

    fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_block_retry_wakeup_request()
    }

    fn schedule_run_loop_block_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        self.session
            .schedule_run_loop_block_retry_wakeup(retry_after);
    }

    fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
        self.session.take_run_loop_entropy_retry_wakeup_request()
    }

    fn schedule_run_loop_entropy_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        self.session
            .schedule_run_loop_entropy_retry_wakeup(retry_after);
    }

    fn run_loop_vcpu_step(&mut self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        self.session.run_once_and_handle_mmio()
    }

    fn handle_run_loop_virtual_timer(&mut self) -> Result<(), HvfVcpuRunnerError> {
        let intid = self.session.gic.timer_interrupts.el1_virtual_timer_intid;
        self.session.runner.set_gic_ppi_pending(intid)
    }

    fn dispatch_run_loop_block_notifications(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>
    {
        self.session
            .dispatch_block_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_pmem_notifications(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        self.session
            .dispatch_pmem_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_network_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        self.session
            .dispatch_network_queue_notifications_with_packet_io_and_signal_interrupts(
                self.packet_io,
            )
    }

    fn dispatch_run_loop_vsock_notifications(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        self.session
            .dispatch_vsock_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_balloon_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        self.session
            .dispatch_balloon_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_memory_hotplug_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        self.session
            .dispatch_memory_hotplug_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_entropy_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            &mut self.session.backend,
            &self.session.mmio_dispatcher,
            &mut self.session.runtime_resources,
            &self.session.gic,
            &self.session.entropy_device_metrics,
            &mut self.session.entropy_source,
        )
    }
}

impl Drop for OwnedHvfArm64BootSession {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[derive(Debug)]
pub struct HvfArm64BootBlockNotificationDispatches {
    devices: Vec<HvfArm64BootBlockNotificationDispatch>,
    rate_limiter_retry_after: Option<Duration>,
}

impl HvfArm64BootBlockNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootBlockNotificationDispatch>) -> Self {
        let rate_limiter_retry_after = devices
            .iter()
            .filter_map(|device| device.dispatch().rate_limiter_retry_after())
            .min();
        Self {
            devices,
            rate_limiter_retry_after,
        }
    }

    #[cfg(test)]
    fn new_for_test_with_rate_limiter_retry_after(retry_after: Duration) -> Self {
        Self {
            devices: Vec::new(),
            rate_limiter_retry_after: Some(retry_after),
        }
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

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
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
pub struct HvfArm64BootPmemNotificationDispatches {
    devices: Vec<HvfArm64BootPmemNotificationDispatch>,
}

impl HvfArm64BootPmemNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootPmemNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootPmemNotificationDispatch] {
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
pub struct HvfArm64BootPmemNotificationDispatch {
    dispatch: Arm64BootPmemNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootPmemNotificationDispatch {
    fn new(
        dispatch: Arm64BootPmemNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootPmemNotificationDispatch {
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
pub struct HvfArm64BootNetworkNotificationDispatches {
    devices: Vec<HvfArm64BootNetworkNotificationDispatch>,
}

impl HvfArm64BootNetworkNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootNetworkNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootNetworkNotificationDispatch] {
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
pub struct HvfArm64BootNetworkNotificationDispatch {
    dispatch: Arm64BootNetworkNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootNetworkNotificationDispatch {
    fn new(
        dispatch: Arm64BootNetworkNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootNetworkNotificationDispatch {
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
pub struct HvfArm64BootVsockNotificationDispatches {
    devices: Vec<HvfArm64BootVsockNotificationDispatch>,
}

impl HvfArm64BootVsockNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootVsockNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootVsockNotificationDispatch] {
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
pub struct HvfArm64BootVsockNotificationDispatch {
    dispatch: Arm64BootVsockNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootVsockNotificationDispatch {
    fn new(
        dispatch: Arm64BootVsockNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootVsockNotificationDispatch {
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
pub struct HvfArm64BootBalloonNotificationDispatches {
    devices: Vec<HvfArm64BootBalloonNotificationDispatch>,
}

impl HvfArm64BootBalloonNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootBalloonNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootBalloonNotificationDispatch] {
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
pub struct HvfArm64BootBalloonNotificationDispatch {
    dispatch: Arm64BootBalloonNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootBalloonNotificationDispatch {
    fn new(
        dispatch: Arm64BootBalloonNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootBalloonNotificationDispatch {
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
pub struct HvfArm64BootMemoryHotplugNotificationDispatches {
    devices: Vec<HvfArm64BootMemoryHotplugNotificationDispatch>,
}

impl HvfArm64BootMemoryHotplugNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootMemoryHotplugNotificationDispatch>) -> Self {
        Self { devices }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootMemoryHotplugNotificationDispatch] {
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
pub struct HvfArm64BootMemoryHotplugNotificationDispatch {
    dispatch: Arm64BootMemoryHotplugNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootMemoryHotplugNotificationDispatch {
    fn new(
        dispatch: Arm64BootMemoryHotplugNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootMemoryHotplugNotificationDispatch {
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
pub struct HvfArm64BootEntropyNotificationDispatches {
    devices: Vec<HvfArm64BootEntropyNotificationDispatch>,
    rate_limiter_retry_after: Option<Duration>,
}

impl HvfArm64BootEntropyNotificationDispatches {
    fn new(devices: Vec<HvfArm64BootEntropyNotificationDispatch>) -> Self {
        let rate_limiter_retry_after = devices
            .iter()
            .filter_map(|device| device.dispatch().rate_limiter_retry_after())
            .min();
        Self {
            devices,
            rate_limiter_retry_after,
        }
    }

    #[cfg(test)]
    fn new_for_test_with_rate_limiter_retry_after(retry_after: Duration) -> Self {
        Self {
            devices: Vec::new(),
            rate_limiter_retry_after: Some(retry_after),
        }
    }

    pub fn as_slice(&self) -> &[HvfArm64BootEntropyNotificationDispatch] {
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

    pub fn rate_limiter_retry_after(&self) -> Option<Duration> {
        self.rate_limiter_retry_after
    }
}

#[derive(Debug)]
pub struct HvfArm64BootEntropyNotificationDispatch {
    dispatch: Arm64BootEntropyNotificationDispatch,
    signal_error: Option<DeviceInterruptTriggerError>,
}

impl HvfArm64BootEntropyNotificationDispatch {
    fn new(
        dispatch: Arm64BootEntropyNotificationDispatch,
        signal_error: Option<DeviceInterruptTriggerError>,
    ) -> Self {
        Self {
            dispatch,
            signal_error,
        }
    }

    pub const fn dispatch(&self) -> &Arm64BootEntropyNotificationDispatch {
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

#[derive(Debug)]
pub enum HvfArm64BootPmemNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootPmemNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootNetworkNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootNetworkNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootVsockNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootVsockNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootBalloonNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootBalloonNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootMemoryHotplugNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootMemoryHotplugNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

#[derive(Debug)]
pub enum HvfArm64BootEntropyNotificationDispatchError {
    MapGuestMemory {
        source: HvfGuestMemoryMappingError,
    },
    MmioDispatcher {
        source: HvfArm64BootMmioDispatcherError,
    },
    DispatchNotifications {
        source: Arm64BootEntropyNotificationDispatchError,
    },
    CreateSignalSink {
        source: HvfGicSpiSignalError,
    },
    ResultAllocation {
        source: TryReserveError,
    },
}

impl fmt::Display for HvfArm64BootNetworkNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for network notifications: {source}"
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
                    "failed to dispatch boot network queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot network interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot network notification results: {source}"
                )
            }
        }
    }
}

impl fmt::Display for HvfArm64BootPmemNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for pmem notifications: {source}"
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
                    "failed to dispatch boot pmem queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot pmem interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot pmem notification results: {source}"
                )
            }
        }
    }
}

impl fmt::Display for HvfArm64BootVsockNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for vsock notifications: {source}"
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
                    "failed to dispatch boot vsock queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot vsock interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot vsock notification results: {source}"
                )
            }
        }
    }
}

impl fmt::Display for HvfArm64BootBalloonNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for balloon notifications: {source}"
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
                    "failed to dispatch boot balloon queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot balloon interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot balloon notification results: {source}"
                )
            }
        }
    }
}

impl fmt::Display for HvfArm64BootMemoryHotplugNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for memory-hotplug notifications: {source}"
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
                    "failed to dispatch boot memory-hotplug queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot memory-hotplug interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot memory-hotplug notification results: {source}"
                )
            }
        }
    }
}

impl fmt::Display for HvfArm64BootEntropyNotificationDispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MapGuestMemory { source } => {
                write!(
                    f,
                    "failed to borrow HVF boot-session guest memory for entropy notifications: {source}"
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
                    "failed to dispatch boot entropy queue notifications: {source}"
                )
            }
            Self::CreateSignalSink { source } => {
                write!(
                    f,
                    "failed to create HVF boot entropy interrupt signaler: {source}"
                )
            }
            Self::ResultAllocation { source } => {
                write!(
                    f,
                    "failed to allocate HVF boot entropy notification results: {source}"
                )
            }
        }
    }
}

impl std::error::Error for HvfArm64BootEntropyNotificationDispatchError {
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

impl std::error::Error for HvfArm64BootPmemNotificationDispatchError {
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

impl std::error::Error for HvfArm64BootVsockNotificationDispatchError {
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

impl std::error::Error for HvfArm64BootBalloonNotificationDispatchError {
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

impl std::error::Error for HvfArm64BootMemoryHotplugNotificationDispatchError {
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

impl std::error::Error for HvfArm64BootNetworkNotificationDispatchError {
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

fn start_run_loop_vsock_wakeup_monitor(
    runtime_resources: &Arm64BootRuntimeResources,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    cancel_handle: HvfVcpuRunCancelHandle,
    wakeup_token: HvfArm64BootRunLoopWakeupToken,
) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
    if runtime_resources.vsock_device.is_none() {
        return Ok(HvfArm64BootRunLoopWakeupMonitor::inactive());
    }

    let fds = {
        let mut mmio_dispatcher = lock_boot_mmio_dispatcher(dispatcher)
            .map_err(|source| HvfArm64BootRunLoopWakeupMonitorError::MmioDispatcher { source })?;
        runtime_resources
            .vsock_host_read_wakeup_fds(&mut mmio_dispatcher)
            .map_err(
                |source| HvfArm64BootRunLoopWakeupMonitorError::CollectVsockWakeupFds { source },
            )?
    };

    HvfArm64BootRunLoopWakeupMonitor::start(fds, cancel_handle, wakeup_token)
}

trait BootSessionRunLoopSession {
    fn start_run_loop_vsock_wakeup_monitor(
        &mut self,
    ) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
        Ok(HvfArm64BootRunLoopWakeupMonitor::inactive())
    }

    fn take_run_loop_wakeup_request(&mut self) -> bool {
        false
    }

    fn take_run_loop_control_wakeup_request(&mut self) -> bool {
        false
    }

    fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
        false
    }

    fn schedule_run_loop_block_retry_wakeup(&mut self, _retry_after: Option<Duration>) {}

    fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
        false
    }

    fn schedule_run_loop_entropy_retry_wakeup(&mut self, _retry_after: Option<Duration>) {}

    fn run_loop_vcpu_step(&mut self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>;

    fn handle_run_loop_virtual_timer(&mut self) -> Result<(), HvfVcpuRunnerError>;

    fn dispatch_run_loop_block_notifications(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>;

    fn dispatch_run_loop_pmem_notifications(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>;

    fn dispatch_run_loop_network_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    >;

    fn dispatch_run_loop_vsock_notifications(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>;

    fn dispatch_run_loop_balloon_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    >;

    fn dispatch_run_loop_memory_hotplug_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    >;

    fn dispatch_run_loop_entropy_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    >;
}

impl BootSessionRunLoopSession for HvfArm64BootSession<'_> {
    fn start_run_loop_vsock_wakeup_monitor(
        &mut self,
    ) -> Result<HvfArm64BootRunLoopWakeupMonitor, HvfArm64BootRunLoopWakeupMonitorError> {
        start_run_loop_vsock_wakeup_monitor(
            &self.runtime_resources,
            &self.mmio_dispatcher,
            self.run_cancel_handle(),
            self.run_loop_wakeup.clone(),
        )
    }

    fn take_run_loop_wakeup_request(&mut self) -> bool {
        self.run_loop_wakeup.take_wakeup_request()
    }

    fn take_run_loop_control_wakeup_request(&mut self) -> bool {
        self.control_wakeup.take_wakeup_request()
    }

    fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
        self.block_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_block_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.block_retry_wakeup.take_wakeup_request();
        }
        self.block_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
        self.entropy_retry_wakeup.take_wakeup_request()
    }

    fn schedule_run_loop_entropy_retry_wakeup(&mut self, retry_after: Option<Duration>) {
        if retry_after.is_none() {
            let _ = self.entropy_retry_wakeup.take_wakeup_request();
        }
        self.entropy_retry_wakeup_scheduler
            .schedule_after(retry_after);
    }

    fn run_loop_vcpu_step(&mut self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
        self.run_once_and_handle_mmio()
    }

    fn handle_run_loop_virtual_timer(&mut self) -> Result<(), HvfVcpuRunnerError> {
        self.runner
            .set_gic_ppi_pending(self.gic.timer_interrupts.el1_virtual_timer_intid)
    }

    fn dispatch_run_loop_block_notifications(
        &mut self,
    ) -> Result<HvfArm64BootBlockNotificationDispatches, HvfArm64BootBlockNotificationDispatchError>
    {
        self.dispatch_block_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_pmem_notifications(
        &mut self,
    ) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError>
    {
        self.dispatch_pmem_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_network_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootNetworkNotificationDispatches,
        HvfArm64BootNetworkNotificationDispatchError,
    > {
        self.dispatch_network_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_vsock_notifications(
        &mut self,
    ) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError>
    {
        self.dispatch_vsock_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_balloon_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootBalloonNotificationDispatches,
        HvfArm64BootBalloonNotificationDispatchError,
    > {
        self.dispatch_balloon_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_memory_hotplug_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootMemoryHotplugNotificationDispatches,
        HvfArm64BootMemoryHotplugNotificationDispatchError,
    > {
        self.dispatch_memory_hotplug_queue_notifications_and_signal_interrupts()
    }

    fn dispatch_run_loop_entropy_notifications(
        &mut self,
    ) -> Result<
        HvfArm64BootEntropyNotificationDispatches,
        HvfArm64BootEntropyNotificationDispatchError,
    > {
        dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
            self.backend,
            &self.mmio_dispatcher,
            &mut self.runtime_resources,
            &self.gic,
            &self.entropy_device_metrics,
            &mut self.entropy_source,
        )
    }
}

fn run_boot_session_loop(
    session: &mut impl BootSessionRunLoopSession,
    stop_token: &HvfArm64BootRunLoopStopToken,
    max_steps: NonZeroUsize,
) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
    run_boot_session_loop_with_observer(session, stop_token, max_steps, |_| {})
}

fn dispatch_run_loop_block_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<(), HvfArm64BootRunLoopError> {
    let dispatches = session
        .dispatch_run_loop_block_notifications()
        .map_err(
            |source| HvfArm64BootRunLoopError::DispatchBlockNotifications {
                steps_completed,
                source: Box::new(source),
            },
        )?;
    session.schedule_run_loop_block_retry_wakeup(dispatches.rate_limiter_retry_after());

    Ok(())
}

fn dispatch_run_loop_block_retry_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<bool, HvfArm64BootRunLoopError> {
    if !session.take_run_loop_block_retry_wakeup_request() {
        return Ok(false);
    }

    dispatch_run_loop_block_notifications_for_step(session, steps_completed)?;

    Ok(true)
}

fn dispatch_run_loop_entropy_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<(), HvfArm64BootRunLoopError> {
    let dispatches = session
        .dispatch_run_loop_entropy_notifications()
        .map_err(
            |source| HvfArm64BootRunLoopError::DispatchEntropyNotifications {
                steps_completed,
                source: Box::new(source),
            },
        )?;
    session.schedule_run_loop_entropy_retry_wakeup(dispatches.rate_limiter_retry_after());

    Ok(())
}

fn dispatch_run_loop_entropy_retry_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<bool, HvfArm64BootRunLoopError> {
    if !session.take_run_loop_entropy_retry_wakeup_request() {
        return Ok(false);
    }

    dispatch_run_loop_entropy_notifications_for_step(session, steps_completed)?;

    Ok(true)
}

fn dispatch_run_loop_vsock_notifications_for_step(
    session: &mut impl BootSessionRunLoopSession,
    steps_completed: usize,
) -> Result<(), HvfArm64BootRunLoopError> {
    session
        .dispatch_run_loop_vsock_notifications()
        .map_err(
            |source| HvfArm64BootRunLoopError::DispatchVsockNotifications {
                steps_completed,
                source: Box::new(source),
            },
        )?;

    Ok(())
}

fn run_boot_session_loop_with_observer(
    session: &mut impl BootSessionRunLoopSession,
    stop_token: &HvfArm64BootRunLoopStopToken,
    max_steps: NonZeroUsize,
    mut observe_step: impl FnMut(&HvfVcpuRunStepOutcome),
) -> Result<HvfArm64BootRunLoopOutcome, HvfArm64BootRunLoopError> {
    let max_steps = max_steps.get();
    let mut steps = 0usize;

    loop {
        if stop_token.is_stop_requested() {
            return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
        }

        let monitor = session
            .start_run_loop_vsock_wakeup_monitor()
            .map_err(|source| HvfArm64BootRunLoopError::StartVsockWakeupMonitor {
                steps_completed: steps,
                source: Box::new(source),
            })?;
        let outcome_result = session.run_loop_vcpu_step();
        let finish_result = monitor.finish();
        let monitor_wakeup_requested = match &outcome_result {
            Ok(_) => finish_result.map_err(|source| {
                HvfArm64BootRunLoopError::StopVsockWakeupMonitor {
                    steps_completed: steps.saturating_add(1),
                    source: Box::new(source),
                }
            })?,
            Err(_) => finish_result.map_err(|source| {
                HvfArm64BootRunLoopError::StopVsockWakeupMonitor {
                    steps_completed: steps,
                    source: Box::new(source),
                }
            })?,
        };
        let outcome = outcome_result.map_err(|source| HvfArm64BootRunLoopError::RunStep {
            steps_completed: steps,
            source: Box::new(source),
        })?;
        let canceled = matches!(outcome, HvfVcpuRunStepOutcome::Canceled);
        if !canceled && !monitor_wakeup_requested {
            let _ = session.take_run_loop_wakeup_request();
        }
        observe_step(&outcome);
        steps += 1;

        match outcome {
            HvfVcpuRunStepOutcome::Canceled => {
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let control_wakeup_requested = session.take_run_loop_control_wakeup_request();
                let wakeup_requested =
                    session.take_run_loop_wakeup_request() || monitor_wakeup_requested;
                let block_retry_wakeup_requested =
                    session.take_run_loop_block_retry_wakeup_request();
                let entropy_retry_wakeup_requested =
                    session.take_run_loop_entropy_retry_wakeup_request();
                if block_retry_wakeup_requested {
                    dispatch_run_loop_block_notifications_for_step(session, steps)?;
                    if stop_token.is_stop_requested() {
                        return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                    }
                }
                if entropy_retry_wakeup_requested {
                    dispatch_run_loop_entropy_notifications_for_step(session, steps)?;
                    if stop_token.is_stop_requested() {
                        return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                    }
                }
                if wakeup_requested {
                    dispatch_run_loop_vsock_notifications_for_step(session, steps)?;
                    if stop_token.is_stop_requested() {
                        return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                    }
                }
                if control_wakeup_requested {
                    return Ok(HvfArm64BootRunLoopOutcome::Wakeup { steps });
                }
                if wakeup_requested
                    || block_retry_wakeup_requested
                    || entropy_retry_wakeup_requested
                {
                    if steps == max_steps {
                        return Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { steps });
                    }
                    continue;
                }
                return Ok(HvfArm64BootRunLoopOutcome::Canceled { steps });
            }
            HvfVcpuRunStepOutcome::VtimerActivated => {
                session.handle_run_loop_virtual_timer().map_err(|source| {
                    HvfArm64BootRunLoopError::HandleVirtualTimer {
                        steps_completed: steps,
                        source: Box::new(source),
                    }
                })?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_block_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_entropy_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_vsock_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                if steps == max_steps {
                    return Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { steps });
                }
            }
            HvfVcpuRunStepOutcome::Unknown { reason } => {
                return Ok(HvfArm64BootRunLoopOutcome::Unknown { steps, reason });
            }
            HvfVcpuRunStepOutcome::GuestShutdown { .. } => {
                return Ok(HvfArm64BootRunLoopOutcome::GuestShutdown { steps });
            }
            HvfVcpuRunStepOutcome::GuestReset { .. } => {
                return Ok(HvfArm64BootRunLoopOutcome::GuestReset { steps });
            }
            HvfVcpuRunStepOutcome::Hvc { .. } | HvfVcpuRunStepOutcome::Sys64 { .. } => {
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_block_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                let _ = dispatch_run_loop_entropy_retry_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_vsock_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                if steps == max_steps {
                    return Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { steps });
                }
            }
            HvfVcpuRunStepOutcome::Mmio { .. } => {
                let _ = session.take_run_loop_block_retry_wakeup_request();
                let _ = session.take_run_loop_entropy_retry_wakeup_request();
                dispatch_run_loop_block_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                session
                    .dispatch_run_loop_pmem_notifications()
                    .map_err(
                        |source| HvfArm64BootRunLoopError::DispatchPmemNotifications {
                            steps_completed: steps,
                            source: Box::new(source),
                        },
                    )?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                session
                    .dispatch_run_loop_network_notifications()
                    .map_err(
                        |source| HvfArm64BootRunLoopError::DispatchNetworkNotifications {
                            steps_completed: steps,
                            source: Box::new(source),
                        },
                    )?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_vsock_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                session
                    .dispatch_run_loop_balloon_notifications()
                    .map_err(
                        |source| HvfArm64BootRunLoopError::DispatchBalloonNotifications {
                            steps_completed: steps,
                            source: Box::new(source),
                        },
                    )?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                session
                    .dispatch_run_loop_memory_hotplug_notifications()
                    .map_err(|source| {
                        HvfArm64BootRunLoopError::DispatchMemoryHotplugNotifications {
                            steps_completed: steps,
                            source: Box::new(source),
                        }
                    })?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                dispatch_run_loop_entropy_notifications_for_step(session, steps)?;
                if stop_token.is_stop_requested() {
                    return Ok(HvfArm64BootRunLoopOutcome::Stopped { steps });
                }
                if steps == max_steps {
                    return Ok(HvfArm64BootRunLoopOutcome::StepLimitReached { steps });
                }
            }
        }
    }
}

struct HvfArm64BootRunLoopWakeupMonitor {
    stop_writer: Option<UnixStream>,
    thread: Option<JoinHandle<bool>>,
    completed_wakeup: bool,
}

impl HvfArm64BootRunLoopWakeupMonitor {
    const fn inactive() -> Self {
        Self {
            stop_writer: None,
            thread: None,
            completed_wakeup: false,
        }
    }

    #[cfg(test)]
    const fn completed_for_test(completed_wakeup: bool) -> Self {
        Self {
            stop_writer: None,
            thread: None,
            completed_wakeup,
        }
    }

    fn start(
        mut fds: Vec<RawFd>,
        cancel_handle: HvfVcpuRunCancelHandle,
        wakeup_token: HvfArm64BootRunLoopWakeupToken,
    ) -> Result<Self, HvfArm64BootRunLoopWakeupMonitorError> {
        fds.sort_unstable();
        fds.dedup();
        if fds.is_empty() {
            return Ok(Self::inactive());
        }

        let (stop_reader, stop_writer) =
            UnixStream::pair().map_err(|source| Self::create_stop_pipe_error(source.kind()))?;
        let mut pollfds = vsock_wakeup_pollfds(fds, stop_reader.as_raw_fd())?;
        let pollfd_count = libc::nfds_t::try_from(pollfds.len()).map_err(|_| {
            HvfArm64BootRunLoopWakeupMonitorError::TooManyPollFds {
                count: pollfds.len(),
            }
        })?;
        let thread = thread::Builder::new()
            .name(VSOCK_WAKEUP_MONITOR_THREAD_NAME.to_owned())
            .spawn(move || {
                run_vsock_wakeup_monitor(
                    &mut pollfds,
                    pollfd_count,
                    stop_reader,
                    cancel_handle,
                    wakeup_token,
                )
            })
            .map_err(|source| HvfArm64BootRunLoopWakeupMonitorError::ThreadSpawn { source })?;

        Ok(Self {
            stop_writer: Some(stop_writer),
            thread: Some(thread),
            completed_wakeup: false,
        })
    }

    fn finish(mut self) -> Result<bool, HvfArm64BootRunLoopWakeupMonitorError> {
        let mut stop_signal_error = None;
        if let Some(mut stop_writer) = self.stop_writer.take() {
            match stop_writer.write_all(&VSOCK_WAKEUP_MONITOR_STOP_BYTE) {
                Ok(()) => {}
                Err(source)
                    if matches!(
                        source.kind(),
                        io::ErrorKind::BrokenPipe | io::ErrorKind::NotConnected
                    ) => {}
                Err(source) => {
                    stop_signal_error = Some(source.kind());
                }
            }
        }

        let completed_wakeup = if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| HvfArm64BootRunLoopWakeupMonitorError::ThreadPanicked)?
        } else {
            self.completed_wakeup
        };

        if let Some(source) = stop_signal_error {
            return Err(HvfArm64BootRunLoopWakeupMonitorError::StopSignal { source });
        }

        Ok(completed_wakeup)
    }

    const fn create_stop_pipe_error(
        source: io::ErrorKind,
    ) -> HvfArm64BootRunLoopWakeupMonitorError {
        HvfArm64BootRunLoopWakeupMonitorError::CreateStopPipe { source }
    }
}

fn vsock_wakeup_pollfds(
    fds: Vec<RawFd>,
    stop_fd: RawFd,
) -> Result<Vec<libc::pollfd>, HvfArm64BootRunLoopWakeupMonitorError> {
    let mut pollfds = Vec::new();
    pollfds
        .try_reserve_exact(fds.len().saturating_add(1))
        .map_err(|source| HvfArm64BootRunLoopWakeupMonitorError::PollFdAllocation { source })?;
    pollfds.push(libc::pollfd {
        fd: stop_fd,
        events: libc::POLLIN,
        revents: 0,
    });
    pollfds.extend(fds.into_iter().map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    }));

    Ok(pollfds)
}

fn run_vsock_wakeup_monitor(
    pollfds: &mut [libc::pollfd],
    pollfd_count: libc::nfds_t,
    _stop_reader: UnixStream,
    cancel_handle: HvfVcpuRunCancelHandle,
    wakeup_token: HvfArm64BootRunLoopWakeupToken,
) -> bool {
    loop {
        for pollfd in pollfds.iter_mut() {
            pollfd.revents = 0;
        }

        // SAFETY: `pollfds` is a valid mutable slice for `pollfd_count` entries
        // and remains alive for the duration of this blocking `poll` call.
        let poll_result = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfd_count, POLL_FOREVER) };
        if poll_result < 0 {
            let source = io::Error::last_os_error();
            if source.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }

        let Some(stop_pollfd) = pollfds.first() else {
            return false;
        };
        if pollfd_has_wakeup_event(stop_pollfd.revents) {
            return false;
        }
        if pollfds
            .iter()
            .skip(1)
            .any(|pollfd| pollfd_has_wakeup_event(pollfd.revents))
        {
            wakeup_token.request_wakeup();
            let _ = cancel_handle.cancel();
            return true;
        }
    }
}

const fn pollfd_has_wakeup_event(revents: libc::c_short) -> bool {
    revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
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

fn collect_pmem_notification_dispatches(
    dispatches: Arm64BootPmemNotificationDispatches,
) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(|source| HvfArm64BootPmemNotificationDispatchError::ResultAllocation { source })?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootPmemNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootPmemNotificationDispatches::new(devices))
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

fn signal_pmem_queue_interrupts(
    dispatches: Arm64BootPmemNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(|source| HvfArm64BootPmemNotificationDispatchError::ResultAllocation { source })?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootPmemNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootPmemNotificationDispatches::new(devices))
}

fn collect_network_notification_dispatches(
    dispatches: Arm64BootNetworkNotificationDispatches,
) -> Result<HvfArm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootNetworkNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootNetworkNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootNetworkNotificationDispatches::new(devices))
}

fn collect_vsock_notification_dispatches(
    dispatches: Arm64BootVsockNotificationDispatches,
) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootVsockNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootVsockNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootVsockNotificationDispatches::new(devices))
}

fn collect_balloon_notification_dispatches(
    dispatches: Arm64BootBalloonNotificationDispatches,
) -> Result<HvfArm64BootBalloonNotificationDispatches, HvfArm64BootBalloonNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootBalloonNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootBalloonNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootBalloonNotificationDispatches::new(devices))
}

fn collect_memory_hotplug_notification_dispatches(
    dispatches: Arm64BootMemoryHotplugNotificationDispatches,
) -> Result<
    HvfArm64BootMemoryHotplugNotificationDispatches,
    HvfArm64BootMemoryHotplugNotificationDispatchError,
> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootMemoryHotplugNotificationDispatchError::ResultAllocation {
                source,
            },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootMemoryHotplugNotificationDispatch::new(
            dispatch, None,
        ));
    }

    Ok(HvfArm64BootMemoryHotplugNotificationDispatches::new(
        devices,
    ))
}

fn collect_entropy_notification_dispatches(
    dispatches: Arm64BootEntropyNotificationDispatches,
) -> Result<HvfArm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootEntropyNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        devices.push(HvfArm64BootEntropyNotificationDispatch::new(dispatch, None));
    }

    Ok(HvfArm64BootEntropyNotificationDispatches::new(devices))
}

fn dispatch_pmem_queue_notifications_and_signal_interrupts(
    backend: &mut HvfBackend,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    runtime_resources: &mut Arm64BootRuntimeResources,
    gic: &HvfGicMetadata,
    metrics: &SharedPmemDeviceMetricsRegistry,
) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    let flush_status = {
        let mut mmio_dispatcher = lock_boot_mmio_dispatcher(dispatcher).map_err(|source| {
            HvfArm64BootPmemNotificationDispatchError::MmioDispatcher { source }
        })?;

        if runtime_resources.has_pending_pmem_queue_notifications(&mut mmio_dispatcher) {
            VirtioPmemFlushStatus::from_result(backend.flush_mapped_pmem_shadows().is_ok())
        } else {
            VirtioPmemFlushStatus::Success
        }
    };

    let dispatches = {
        let memory = backend.mapped_guest_memory_mut().map_err(|source| {
            HvfArm64BootPmemNotificationDispatchError::MapGuestMemory { source }
        })?;
        let mut mmio_dispatcher = lock_boot_mmio_dispatcher(dispatcher).map_err(|source| {
            HvfArm64BootPmemNotificationDispatchError::MmioDispatcher { source }
        })?;

        dispatch_pmem_runtime_notifications(
            memory,
            runtime_resources,
            &mut mmio_dispatcher,
            flush_status,
        )?
    };

    record_pmem_runtime_dispatch_metrics(metrics, dispatches.as_slice());
    let result = collect_or_signal_pmem_queue_interrupts(dispatches, gic);
    match &result {
        Ok(dispatches) => record_pmem_signal_metrics(metrics, dispatches),
        Err(_) => metrics.record_event_failure(),
    }
    result
}

fn dispatch_pmem_runtime_notifications(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
    flush_status: VirtioPmemFlushStatus,
) -> Result<Arm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    runtime_resources
        .dispatch_pmem_queue_notifications(memory, mmio_dispatcher, flush_status)
        .map_err(
            |source| HvfArm64BootPmemNotificationDispatchError::DispatchNotifications { source },
        )
}

fn dispatch_network_runtime_notifications(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
) -> Result<Arm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError> {
    runtime_resources
        .dispatch_network_queue_notifications(memory, mmio_dispatcher)
        .map_err(
            |source| HvfArm64BootNetworkNotificationDispatchError::DispatchNotifications { source },
        )
}

fn dispatch_network_runtime_notifications_with_packet_io(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
    packet_io: &mut impl Arm64BootNetworkPacketIoProvider,
) -> Result<Arm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError> {
    runtime_resources
        .dispatch_network_queue_notifications_with_packet_io(memory, mmio_dispatcher, packet_io)
        .map_err(
            |source| HvfArm64BootNetworkNotificationDispatchError::DispatchNotifications { source },
        )
}

fn dispatch_vsock_runtime_notifications(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
) -> Result<Arm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError> {
    runtime_resources
        .dispatch_vsock_queue_notifications(memory, mmio_dispatcher)
        .map_err(
            |source| HvfArm64BootVsockNotificationDispatchError::DispatchNotifications { source },
        )
}

fn dispatch_balloon_runtime_notifications(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
) -> Result<Arm64BootBalloonNotificationDispatches, HvfArm64BootBalloonNotificationDispatchError> {
    runtime_resources
        .dispatch_balloon_queue_notifications(memory, mmio_dispatcher)
        .map_err(
            |source| HvfArm64BootBalloonNotificationDispatchError::DispatchNotifications { source },
        )
}

fn dispatch_memory_hotplug_runtime_notifications_with_executor(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
    mutation_executor: &mut impl VirtioMemMutationExecutor,
) -> Result<
    Arm64BootMemoryHotplugNotificationDispatches,
    HvfArm64BootMemoryHotplugNotificationDispatchError,
> {
    runtime_resources
        .dispatch_memory_hotplug_queue_notifications_with_executor(
            memory,
            mmio_dispatcher,
            mutation_executor,
        )
        .map_err(|source| {
            HvfArm64BootMemoryHotplugNotificationDispatchError::DispatchNotifications { source }
        })
}

fn update_memory_hotplug_requested_size_and_signal_interrupt(
    runtime_resources: &Arm64BootRuntimeResources,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    gic: &HvfGicMetadata,
    update: MemoryHotplugSizeUpdate,
) -> Result<(), MemoryHotplugUpdateError> {
    let device = runtime_resources
        .memory_hotplug_device
        .as_ref()
        .ok_or(MemoryHotplugUpdateError::ActiveSessionUnavailable)?;
    {
        let mut mmio_dispatcher = lock_boot_mmio_dispatcher(dispatcher).map_err(|source| {
            MemoryHotplugUpdateError::ActiveSessionCommand {
                message: source.to_string(),
            }
        })?;
        update_memory_hotplug_config_for_device(device, &mut mmio_dispatcher, update)?;
    }

    if let Ok(signaler) = HvfGicSpiSignaler::from_metadata(gic) {
        let _ = signal_device_interrupt(
            device.fdt_device.interrupt_line,
            DeviceInterruptKind::Config,
            &signaler,
        );
    }

    Ok(())
}

fn memory_hotplug_status(
    runtime_resources: &Arm64BootRuntimeResources,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    config: MemoryHotplugConfig,
    requested_size_mib: u64,
) -> Result<MemoryHotplugStatus, MemoryHotplugStatusError> {
    let device = runtime_resources
        .memory_hotplug_device
        .as_ref()
        .ok_or(MemoryHotplugStatusError::ActiveSessionUnavailable)?;
    let mut mmio_dispatcher = lock_boot_mmio_dispatcher(dispatcher).map_err(|source| {
        MemoryHotplugStatusError::ActiveSessionCommand {
            message: source.to_string(),
        }
    })?;

    memory_hotplug_status_for_device(device, &mut mmio_dispatcher, config, requested_size_mib)
}

fn balloon_update_error_from_display(source: impl fmt::Display) -> BalloonUpdateError {
    BalloonUpdateError::ActiveSessionCommand {
        message: source.to_string(),
    }
}

fn usize_to_u64_saturating(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
fn record_block_dispatch_metrics(
    metrics: &SharedBlockDeviceMetricsRegistry,
    dispatches: &HvfArm64BootBlockNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootBlockNotificationDispatch::dispatch);
    record_block_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_block_signal_metrics(metrics, dispatches);
}

fn record_block_runtime_dispatch_metrics<'a>(
    metrics: &SharedBlockDeviceMetricsRegistry,
    dispatches: impl IntoIterator<Item = &'a Arm64BootBlockNotificationDispatch>,
) {
    for dispatch in dispatches {
        let drive_id = dispatch.device().registration.drive_id();
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch_for_drive(drive_id, dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_queue_events_for_drive(
                drive_id,
                usize_to_u64_saturating(source.drained_notifications().len()),
            );
            metrics.record_event_failure_for_drive(drive_id);
            if let Some(completed) = source.completed_dispatch() {
                metrics.record_queue_dispatch_for_drive(drive_id, completed);
            }
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_event_failure_for_drive(drive_id);
        }
    }
}

fn record_block_signal_metrics(
    metrics: &SharedBlockDeviceMetricsRegistry,
    dispatches: &HvfArm64BootBlockNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            let drive_id = dispatch.dispatch().device().registration.drive_id();
            metrics.record_event_failure_for_drive(drive_id);
        }
    }
}

#[cfg(test)]
fn record_pmem_dispatch_metrics(
    metrics: &SharedPmemDeviceMetricsRegistry,
    dispatches: &HvfArm64BootPmemNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootPmemNotificationDispatch::dispatch);
    record_pmem_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_pmem_signal_metrics(metrics, dispatches);
}

fn record_pmem_runtime_dispatch_metrics<'a>(
    metrics: &SharedPmemDeviceMetricsRegistry,
    dispatches: impl IntoIterator<Item = &'a Arm64BootPmemNotificationDispatch>,
) {
    for dispatch in dispatches {
        let device_id = dispatch.device().registration.pmem_id();
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch_for_device(device_id, dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_notification_error_for_device(device_id, source);
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_event_failure_for_device(device_id);
        }
    }
}

fn record_pmem_signal_metrics(
    metrics: &SharedPmemDeviceMetricsRegistry,
    dispatches: &HvfArm64BootPmemNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            let device_id = dispatch.dispatch().device().registration.pmem_id();
            metrics.record_event_failure_for_device(device_id);
        }
    }
}

#[cfg(test)]
fn record_network_dispatch_metrics(
    metrics: &SharedNetworkInterfaceMetricsRegistry,
    dispatches: &HvfArm64BootNetworkNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootNetworkNotificationDispatch::dispatch);
    record_network_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_network_signal_metrics(metrics, dispatches);
}

fn record_network_runtime_dispatch_metrics<'a>(
    metrics: &SharedNetworkInterfaceMetricsRegistry,
    dispatches: impl IntoIterator<Item = &'a Arm64BootNetworkNotificationDispatch>,
) {
    for dispatch in dispatches {
        let iface_id = dispatch.device().registration.iface_id();
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch_for_interface(iface_id, dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_notification_error_for_interface(iface_id, source);
        }
        if dispatch.outcome().handler_lookup_error().is_some()
            || dispatch.outcome().packet_io_error().is_some()
        {
            metrics.record_event_failure_for_interface(iface_id);
        }
    }
}

fn record_network_signal_metrics(
    metrics: &SharedNetworkInterfaceMetricsRegistry,
    dispatches: &HvfArm64BootNetworkNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            let iface_id = dispatch.dispatch().device().registration.iface_id();
            metrics.record_event_failure_for_interface(iface_id);
        }
    }
}

#[cfg(test)]
fn record_vsock_dispatch_metrics(
    metrics: &SharedVsockDeviceMetrics,
    dispatches: &HvfArm64BootVsockNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootVsockNotificationDispatch::dispatch);
    record_vsock_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_vsock_signal_metrics(metrics, dispatches);
}

fn record_vsock_runtime_dispatch_metrics<'a>(
    metrics: &SharedVsockDeviceMetrics,
    dispatches: impl IntoIterator<Item = &'a Arm64BootVsockNotificationDispatch>,
) {
    for dispatch in dispatches {
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch(dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_notification_error(source);
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_muxer_event_failure();
        }
    }
}

fn record_vsock_signal_metrics(
    metrics: &SharedVsockDeviceMetrics,
    dispatches: &HvfArm64BootVsockNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            metrics.record_muxer_event_failure();
        }
    }
}

#[cfg(test)]
fn record_balloon_dispatch_metrics(
    metrics: &SharedBalloonDeviceMetrics,
    dispatches: &HvfArm64BootBalloonNotificationDispatches,
    inactive_is_failure: bool,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootBalloonNotificationDispatch::dispatch);
    record_balloon_runtime_dispatch_metrics(metrics, runtime_dispatches, inactive_is_failure);
    record_balloon_signal_metrics(metrics, dispatches);
}

fn record_balloon_runtime_dispatch_metrics<'a>(
    metrics: &SharedBalloonDeviceMetrics,
    dispatches: impl IntoIterator<Item = &'a Arm64BootBalloonNotificationDispatch>,
    inactive_is_failure: bool,
) {
    for dispatch in dispatches {
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch(dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            if inactive_is_failure
                || !matches!(
                    source,
                    &VirtioBalloonDeviceNotificationError::Inactive { .. }
                )
            {
                metrics.record_event_failure();
            }
            if let Some(completed) = source.completed_notification_dispatch() {
                metrics.record_notification_dispatch(completed);
            }
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_event_failure();
        }
    }
}

fn record_balloon_signal_metrics(
    metrics: &SharedBalloonDeviceMetrics,
    dispatches: &HvfArm64BootBalloonNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            metrics.record_event_failure();
        }
    }
}

fn balloon_update_result_from_hvf_dispatches(
    dispatches: &HvfArm64BootBalloonNotificationDispatches,
) -> Result<(), BalloonUpdateError> {
    for dispatch in dispatches.as_slice() {
        if let Some(source) = dispatch.dispatch().outcome().handler_lookup_error() {
            return Err(balloon_update_error_from_display(source));
        }
        if let Some(source) = dispatch.dispatch().outcome().dispatch_error() {
            if matches!(
                source,
                &VirtioBalloonDeviceNotificationError::Inactive { .. }
            ) {
                continue;
            }
            return Err(balloon_update_error_from_display(source));
        }
        if let Some(source) = dispatch.signal_error() {
            return Err(balloon_update_error_from_display(source));
        }
    }

    Ok(())
}

#[cfg(test)]
fn record_entropy_dispatch_metrics(
    metrics: &SharedEntropyDeviceMetrics,
    dispatches: &HvfArm64BootEntropyNotificationDispatches,
) {
    let runtime_dispatches = dispatches
        .as_slice()
        .iter()
        .map(HvfArm64BootEntropyNotificationDispatch::dispatch);
    record_entropy_runtime_dispatch_metrics(metrics, runtime_dispatches);
    record_entropy_signal_metrics(metrics, dispatches);
}

fn record_entropy_runtime_dispatch_metrics<'a>(
    metrics: &SharedEntropyDeviceMetrics,
    dispatches: impl IntoIterator<Item = &'a Arm64BootEntropyNotificationDispatch>,
) {
    for dispatch in dispatches {
        if let Some(dispatched) = dispatch.outcome().dispatched() {
            metrics.record_notification_dispatch(dispatched);
        }
        if let Some(source) = dispatch.outcome().dispatch_error() {
            metrics.record_notification_error(source);
        }
        if dispatch.outcome().handler_lookup_error().is_some() {
            metrics.record_event_failure();
        }
        if dispatch.outcome().entropy_source_error().is_some() {
            metrics.record_entropy_source_provider_failure();
        }
    }
}

fn record_entropy_signal_metrics(
    metrics: &SharedEntropyDeviceMetrics,
    dispatches: &HvfArm64BootEntropyNotificationDispatches,
) {
    for dispatch in dispatches.as_slice() {
        if dispatch.signal_error().is_some() {
            metrics.record_event_failure();
        }
    }
}

fn dispatch_entropy_queue_notifications_and_signal_interrupts_with_source(
    backend: &mut HvfBackend,
    dispatcher: &Arc<Mutex<MmioDispatcher>>,
    runtime_resources: &mut Arm64BootRuntimeResources,
    gic: &HvfGicMetadata,
    metrics: &SharedEntropyDeviceMetrics,
    entropy_source: &mut impl Arm64BootEntropySourceProvider,
) -> Result<HvfArm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError>
{
    let dispatches = {
        let memory = backend.mapped_guest_memory_mut().map_err(|source| {
            HvfArm64BootEntropyNotificationDispatchError::MapGuestMemory { source }
        })?;
        let mut mmio_dispatcher = lock_boot_mmio_dispatcher(dispatcher).map_err(|source| {
            HvfArm64BootEntropyNotificationDispatchError::MmioDispatcher { source }
        })?;

        dispatch_entropy_runtime_notifications_with_source(
            memory,
            runtime_resources,
            &mut mmio_dispatcher,
            entropy_source,
        )?
    };

    record_entropy_runtime_dispatch_metrics(metrics, dispatches.as_slice());
    let result = collect_or_signal_entropy_queue_interrupts(dispatches, gic);
    match &result {
        Ok(dispatches) => record_entropy_signal_metrics(metrics, dispatches),
        Err(_) => metrics.record_event_failure(),
    }
    result
}

fn dispatch_entropy_runtime_notifications_with_source(
    memory: &mut GuestMemory,
    runtime_resources: &mut Arm64BootRuntimeResources,
    mmio_dispatcher: &mut MmioDispatcher,
    entropy_source: &mut impl Arm64BootEntropySourceProvider,
) -> Result<Arm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError> {
    runtime_resources
        .dispatch_entropy_queue_notifications_with_source(memory, mmio_dispatcher, entropy_source)
        .map_err(
            |source| HvfArm64BootEntropyNotificationDispatchError::DispatchNotifications { source },
        )
}

fn collect_or_signal_pmem_queue_interrupts(
    dispatches: Arm64BootPmemNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootPmemNotificationDispatches, HvfArm64BootPmemNotificationDispatchError> {
    if !dispatches.needs_queue_interrupt() {
        return collect_pmem_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic)
        .map_err(|source| HvfArm64BootPmemNotificationDispatchError::CreateSignalSink { source })?;

    signal_pmem_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_network_queue_interrupts(
    dispatches: Arm64BootNetworkNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError>
{
    if !dispatches.needs_queue_interrupt() {
        return collect_network_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootNetworkNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_network_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_vsock_queue_interrupts(
    dispatches: Arm64BootVsockNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError> {
    if !dispatches.needs_queue_interrupt() {
        return collect_vsock_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootVsockNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_vsock_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_balloon_queue_interrupts(
    dispatches: Arm64BootBalloonNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootBalloonNotificationDispatches, HvfArm64BootBalloonNotificationDispatchError>
{
    if !dispatches.needs_queue_interrupt() {
        return collect_balloon_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootBalloonNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_balloon_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_memory_hotplug_queue_interrupts(
    dispatches: Arm64BootMemoryHotplugNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<
    HvfArm64BootMemoryHotplugNotificationDispatches,
    HvfArm64BootMemoryHotplugNotificationDispatchError,
> {
    if !dispatches.needs_queue_interrupt() {
        return collect_memory_hotplug_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootMemoryHotplugNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_memory_hotplug_queue_interrupts(dispatches, &signaler)
}

fn collect_or_signal_entropy_queue_interrupts(
    dispatches: Arm64BootEntropyNotificationDispatches,
    gic: &HvfGicMetadata,
) -> Result<HvfArm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError>
{
    if !dispatches.needs_queue_interrupt() {
        return collect_entropy_notification_dispatches(dispatches);
    }

    let signaler = HvfGicSpiSignaler::from_metadata(gic).map_err(|source| {
        HvfArm64BootEntropyNotificationDispatchError::CreateSignalSink { source }
    })?;

    signal_entropy_queue_interrupts(dispatches, &signaler)
}

fn signal_network_queue_interrupts(
    dispatches: Arm64BootNetworkNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootNetworkNotificationDispatches, HvfArm64BootNetworkNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootNetworkNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootNetworkNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootNetworkNotificationDispatches::new(devices))
}

fn signal_vsock_queue_interrupts(
    dispatches: Arm64BootVsockNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootVsockNotificationDispatches, HvfArm64BootVsockNotificationDispatchError> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootVsockNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootVsockNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootVsockNotificationDispatches::new(devices))
}

fn signal_balloon_queue_interrupts(
    dispatches: Arm64BootBalloonNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootBalloonNotificationDispatches, HvfArm64BootBalloonNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootBalloonNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootBalloonNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootBalloonNotificationDispatches::new(devices))
}

fn signal_memory_hotplug_queue_interrupts(
    dispatches: Arm64BootMemoryHotplugNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<
    HvfArm64BootMemoryHotplugNotificationDispatches,
    HvfArm64BootMemoryHotplugNotificationDispatchError,
> {
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootMemoryHotplugNotificationDispatchError::ResultAllocation {
                source,
            },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootMemoryHotplugNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootMemoryHotplugNotificationDispatches::new(
        devices,
    ))
}

fn signal_entropy_queue_interrupts(
    dispatches: Arm64BootEntropyNotificationDispatches,
    signaler: &dyn InterruptSink,
) -> Result<HvfArm64BootEntropyNotificationDispatches, HvfArm64BootEntropyNotificationDispatchError>
{
    let runtime_dispatches = dispatches.into_vec();
    let mut devices = Vec::new();
    devices
        .try_reserve_exact(runtime_dispatches.len())
        .map_err(
            |source| HvfArm64BootEntropyNotificationDispatchError::ResultAllocation { source },
        )?;

    for dispatch in runtime_dispatches {
        let signal_error = if dispatch.needs_queue_interrupt() {
            signal_queue_interrupt(dispatch.device().fdt_device.interrupt_line, signaler).err()
        } else {
            None
        };
        devices.push(HvfArm64BootEntropyNotificationDispatch::new(
            dispatch,
            signal_error,
        ));
    }

    Ok(HvfArm64BootEntropyNotificationDispatches::new(devices))
}

fn signal_queue_interrupt(
    line: GuestInterruptLine,
    signaler: &dyn InterruptSink,
) -> Result<(), DeviceInterruptTriggerError> {
    signal_device_interrupt(line, DeviceInterruptKind::Queue, signaler)
}

fn signal_device_interrupt(
    line: GuestInterruptLine,
    kind: DeviceInterruptKind,
    signaler: &dyn InterruptSink,
) -> Result<(), DeviceInterruptTriggerError> {
    signaler
        .signal(line)
        .map_err(|source| DeviceInterruptTriggerError::Signal { line, kind, source })
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
    StartBlockRetryWakeupScheduler {
        source: io::Error,
    },
    StartEntropyRetryWakeupScheduler {
        source: io::Error,
    },
    ReadMpidr {
        source: HvfVcpuRunnerError,
    },
    AssembleResources {
        source: Arm64BootResourceError,
    },
    RegisterBootTimerMmio {
        source: BootTimerMmioRegistrationError,
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
            Self::StartBlockRetryWakeupScheduler { source } => {
                write!(
                    f,
                    "failed to start HVF block retry wakeup scheduler: {source}"
                )
            }
            Self::StartEntropyRetryWakeupScheduler { source } => {
                write!(
                    f,
                    "failed to start HVF entropy retry wakeup scheduler: {source}"
                )
            }
            Self::ReadMpidr { source } => {
                write!(f, "failed to read primary vCPU MPIDR_EL1: {source}")
            }
            Self::AssembleResources { source } => {
                write!(f, "failed to assemble arm64 boot resources: {source}")
            }
            Self::RegisterBootTimerMmio { source } => {
                write!(f, "failed to register boot timer MMIO: {source}")
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
            Self::StartBlockRetryWakeupScheduler { source } => Some(source),
            Self::StartEntropyRetryWakeupScheduler { source } => Some(source),
            Self::ReadMpidr { source } => Some(source),
            Self::AssembleResources { source } => Some(source),
            Self::RegisterBootTimerMmio { source } => Some(source),
            Self::MapGuestMemory { source } => Some(source),
            Self::ConfigureBootRegisters { source } => Some(source),
            Self::BackendAlreadyInitialized | Self::UnsupportedVcpuCount { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HvfArm64BootInterruptLinePurpose {
    BlockDevice,
    PmemDevice,
    NetworkDevice,
    VsockDevice,
    BalloonDevice,
    EntropyDevice,
    MemoryHotplugDevice,
    SerialDevice,
    VmGenIdDevice,
    VmClockDevice,
}

impl fmt::Display for HvfArm64BootInterruptLinePurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlockDevice => f.write_str("block device"),
            Self::PmemDevice => f.write_str("pmem device"),
            Self::NetworkDevice => f.write_str("network device"),
            Self::VsockDevice => f.write_str("vsock device"),
            Self::BalloonDevice => f.write_str("balloon device"),
            Self::EntropyDevice => f.write_str("entropy device"),
            Self::MemoryHotplugDevice => f.write_str("memory hotplug device"),
            Self::SerialDevice => f.write_str("serial device"),
            Self::VmGenIdDevice => f.write_str("VMGenID device"),
            Self::VmClockDevice => f.write_str("VMClock device"),
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
    control_wakeup: HvfArm64BootRunLoopControlWakeupToken,
    run_loop_wakeup: HvfArm64BootRunLoopWakeupToken,
    block_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    block_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    entropy_retry_wakeup: HvfArm64BootLimiterRetryWakeupToken,
    entropy_retry_wakeup_scheduler: HvfArm64BootLimiterRetryWakeupScheduler,
    block_device_metrics: SharedBlockDeviceMetricsRegistry,
    pmem_device_metrics: SharedPmemDeviceMetricsRegistry,
    balloon_device_metrics: SharedBalloonDeviceMetrics,
    network_interface_metrics: SharedNetworkInterfaceMetricsRegistry,
    vsock_device_metrics: SharedVsockDeviceMetrics,
    entropy_device_metrics: SharedEntropyDeviceMetrics,
    gic: HvfGicMetadata,
    primary_mpidr: u64,
    block_interrupt_lines: Vec<GuestInterruptLine>,
    pmem_interrupt_lines: Vec<GuestInterruptLine>,
    network_interrupt_lines: Vec<GuestInterruptLine>,
    vsock_interrupt_line: Option<GuestInterruptLine>,
    balloon_interrupt_line: Option<GuestInterruptLine>,
    entropy_interrupt_line: Option<GuestInterruptLine>,
    memory_hotplug_interrupt_line: Option<GuestInterruptLine>,
    serial_interrupt_line: Option<GuestInterruptLine>,
    vmgenid_interrupt_line: GuestInterruptLine,
    vmclock_interrupt_line: GuestInterruptLine,
    boot_registers: HvfArm64BootRegisters,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HvfArm64BootInterruptLines {
    block: Vec<GuestInterruptLine>,
    pmem: Vec<GuestInterruptLine>,
    network: Vec<GuestInterruptLine>,
    vsock: Option<GuestInterruptLine>,
    balloon: Option<GuestInterruptLine>,
    entropy: Option<GuestInterruptLine>,
    memory_hotplug: Option<GuestInterruptLine>,
    serial: Option<GuestInterruptLine>,
    vmgenid: GuestInterruptLine,
    vmclock: GuestInterruptLine,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct HvfArm64BootInterruptRequest {
    block_device_count: usize,
    pmem_device_count: usize,
    network_device_count: usize,
    vsock_configured: bool,
    balloon_configured: bool,
    entropy_configured: bool,
    memory_hotplug_configured: bool,
    serial_configured: bool,
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
            control_wakeup: prepared.control_wakeup,
            run_loop_wakeup: prepared.run_loop_wakeup,
            block_retry_wakeup: prepared.block_retry_wakeup,
            block_retry_wakeup_scheduler: prepared.block_retry_wakeup_scheduler,
            entropy_retry_wakeup: prepared.entropy_retry_wakeup,
            entropy_retry_wakeup_scheduler: prepared.entropy_retry_wakeup_scheduler,
            entropy_source: VirtioRngOsEntropySource::new(),
            block_device_metrics: prepared.block_device_metrics,
            pmem_device_metrics: prepared.pmem_device_metrics,
            balloon_device_metrics: prepared.balloon_device_metrics,
            network_interface_metrics: prepared.network_interface_metrics,
            vsock_device_metrics: prepared.vsock_device_metrics,
            entropy_device_metrics: prepared.entropy_device_metrics,
            gic: prepared.gic,
            primary_mpidr: prepared.primary_mpidr,
            block_interrupt_lines: prepared.block_interrupt_lines,
            pmem_interrupt_lines: prepared.pmem_interrupt_lines,
            network_interrupt_lines: prepared.network_interrupt_lines,
            vsock_interrupt_line: prepared.vsock_interrupt_line,
            balloon_interrupt_line: prepared.balloon_interrupt_line,
            entropy_interrupt_line: prepared.entropy_interrupt_line,
            memory_hotplug_interrupt_line: prepared.memory_hotplug_interrupt_line,
            serial_interrupt_line: prepared.serial_interrupt_line,
            vmgenid_interrupt_line: prepared.vmgenid_interrupt_line,
            vmclock_interrupt_line: prepared.vmclock_interrupt_line,
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
        HvfArm64BootInterruptRequest {
            block_device_count: controller.drive_configs().len(),
            pmem_device_count: controller.pmem_configs().len(),
            network_device_count: controller.network_interface_configs().len(),
            vsock_configured: controller.vsock_config().is_some(),
            balloon_configured: controller.balloon_config().is_some()
                && config.balloon_device.is_some(),
            entropy_configured: config.entropy_device.is_some(),
            memory_hotplug_configured: controller.memory_hotplug_config().is_some()
                && config.memory_hotplug_device.is_some(),
            serial_configured: config.serial_device.is_some(),
        },
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
    let runtime_entropy = config
        .entropy_device
        .zip(interrupt_lines.entropy)
        .map(|(entropy, interrupt_line)| entropy.into_runtime(interrupt_line));
    let runtime_memory_hotplug = config
        .memory_hotplug_device
        .zip(interrupt_lines.memory_hotplug)
        .map(|(memory_hotplug, interrupt_line)| memory_hotplug.into_runtime(interrupt_line));
    let resources = Arm64BootResources::assemble_from_controller(
        controller,
        Arm64BootResourceConfig {
            vcpu_mpidrs: &[primary_mpidr],
            gic: gic.arm64_fdt_gic(),
            timer,
            rtc_device: Some(RuntimeArm64BootRtcDeviceConfig::new(config.rtc_mmio_layout)),
            serial_device: runtime_serial,
            vmgenid_interrupt_line: interrupt_lines.vmgenid,
            vmclock_interrupt_line: interrupt_lines.vmclock,
            block_mmio_layout: config.block_mmio_layout,
            block_interrupt_lines: &interrupt_lines.block,
            pmem_mmio_layout: config.pmem_mmio_layout,
            pmem_interrupt_lines: &interrupt_lines.pmem,
            network_mmio_layout: config.network_mmio_layout,
            network_interrupt_lines: &interrupt_lines.network,
            vsock_mmio_layout: config.vsock_mmio_layout,
            vsock_interrupt_line: interrupt_lines.vsock,
            balloon_mmio_layout: config
                .balloon_device
                .map(|balloon| balloon.mmio_layout)
                .unwrap_or_else(|| {
                    BalloonMmioLayout::new(GuestAddress::new(0), MmioRegionId::new(0))
                }),
            balloon_interrupt_line: interrupt_lines.balloon,
            memory_hotplug_device: runtime_memory_hotplug,
            entropy_device: runtime_entropy,
        },
    )
    .map_err(|source| HvfArm64BootSessionError::AssembleResources { source })?;
    let Arm64BootResourceParts {
        memory,
        mut mmio_dispatcher,
        runtime,
    } = resources.into_parts();
    if let Some(boot_timer) = config.boot_timer_device {
        register_boot_timer_mmio(
            &mut mmio_dispatcher,
            boot_timer.mmio_layout,
            controller.boot_timer_logger(),
        )
        .map_err(|source| HvfArm64BootSessionError::RegisterBootTimerMmio { source })?;
    }

    backend
        .map_guest_memory_with_pmem_devices(
            memory,
            runtime.pmem_devices.as_slice(),
            HvfMemoryPermissions::GUEST_RAM,
        )
        .map_err(|source| HvfArm64BootSessionError::MapGuestMemory { source })?;
    let boot_registers = HvfArm64BootRegisters {
        kernel_entry: runtime.loaded_boot_source.kernel.entry_address,
        fdt_address: runtime.fdt.address,
    };
    runner
        .configure_arm64_boot_registers(boot_registers)
        .map_err(|source| HvfArm64BootSessionError::ConfigureBootRegisters { source })?;
    let block_device_metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(
        runtime
            .block_devices
            .iter()
            .map(|device| device.registration.drive_id()),
    );
    let pmem_device_metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(
        runtime.pmem_devices.iter().map(|device| device.id()),
    );
    let network_interface_metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(
        runtime
            .network_devices
            .iter()
            .map(|device| device.registration.iface_id()),
    );
    let block_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
    let block_retry_wakeup_scheduler = if runtime.block_devices.is_empty() {
        HvfArm64BootLimiterRetryWakeupScheduler::inactive()
    } else {
        HvfArm64BootLimiterRetryWakeupScheduler::start(
            BLOCK_RETRY_WAKEUP_SCHEDULER_THREAD_NAME,
            runner.run_cancel_handle(),
            block_retry_wakeup.clone(),
        )
        .map_err(|source| HvfArm64BootSessionError::StartBlockRetryWakeupScheduler { source })?
    };
    let entropy_retry_wakeup = HvfArm64BootLimiterRetryWakeupToken::default();
    let entropy_retry_wakeup_scheduler = if runtime.entropy_device.is_none() {
        HvfArm64BootLimiterRetryWakeupScheduler::inactive()
    } else {
        HvfArm64BootLimiterRetryWakeupScheduler::start(
            ENTROPY_RETRY_WAKEUP_SCHEDULER_THREAD_NAME,
            runner.run_cancel_handle(),
            entropy_retry_wakeup.clone(),
        )
        .map_err(|source| HvfArm64BootSessionError::StartEntropyRetryWakeupScheduler { source })?
    };

    Ok(PreparedHvfArm64BootSession {
        runner,
        mmio_dispatcher: Arc::new(Mutex::new(mmio_dispatcher)),
        runtime_resources: runtime,
        control_wakeup: HvfArm64BootRunLoopControlWakeupToken::default(),
        run_loop_wakeup: HvfArm64BootRunLoopWakeupToken::default(),
        block_retry_wakeup,
        block_retry_wakeup_scheduler,
        entropy_retry_wakeup,
        entropy_retry_wakeup_scheduler,
        block_device_metrics,
        pmem_device_metrics,
        balloon_device_metrics: SharedBalloonDeviceMetrics::default(),
        network_interface_metrics,
        vsock_device_metrics: SharedVsockDeviceMetrics::default(),
        entropy_device_metrics: SharedEntropyDeviceMetrics::default(),
        gic,
        primary_mpidr,
        block_interrupt_lines: interrupt_lines.block,
        pmem_interrupt_lines: interrupt_lines.pmem,
        network_interrupt_lines: interrupt_lines.network,
        vsock_interrupt_line: interrupt_lines.vsock,
        balloon_interrupt_line: interrupt_lines.balloon,
        entropy_interrupt_line: interrupt_lines.entropy,
        memory_hotplug_interrupt_line: interrupt_lines.memory_hotplug,
        serial_interrupt_line: interrupt_lines.serial,
        vmgenid_interrupt_line: interrupt_lines.vmgenid,
        vmclock_interrupt_line: interrupt_lines.vmclock,
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
    request: HvfArm64BootInterruptRequest,
) -> Result<HvfArm64BootInterruptLines, HvfArm64BootSessionError> {
    let mut allocator = HvfGicInterruptLineAllocator::from_metadata(gic).map_err(|source| {
        HvfArm64BootSessionError::AllocateInterruptLine {
            purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
            source,
        }
    })?;
    let mut block = Vec::new();
    block
        .try_reserve_exact(request.block_device_count)
        .map_err(|source| HvfArm64BootSessionError::InterruptLineStorage { source })?;

    for _ in 0..request.block_device_count {
        block.push(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BlockDevice,
                source,
            }
        })?);
    }

    let mut pmem = Vec::new();
    pmem.try_reserve_exact(request.pmem_device_count)
        .map_err(|source| HvfArm64BootSessionError::InterruptLineStorage { source })?;

    for _ in 0..request.pmem_device_count {
        pmem.push(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::PmemDevice,
                source,
            }
        })?);
    }

    let mut network = Vec::new();
    network
        .try_reserve_exact(request.network_device_count)
        .map_err(|source| HvfArm64BootSessionError::InterruptLineStorage { source })?;

    for _ in 0..request.network_device_count {
        network.push(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::NetworkDevice,
                source,
            }
        })?);
    }

    let vsock = if request.vsock_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VsockDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let balloon = if request.balloon_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BalloonDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let entropy = if request.entropy_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::EntropyDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let memory_hotplug = if request.memory_hotplug_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::MemoryHotplugDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let serial = if request.serial_configured {
        Some(allocator.allocate().map_err(|source| {
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::SerialDevice,
                source,
            }
        })?)
    } else {
        None
    };

    let vmgenid =
        allocator
            .allocate()
            .map_err(|source| HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VmGenIdDevice,
                source,
            })?;
    let vmclock =
        allocator
            .allocate()
            .map_err(|source| HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VmClockDevice,
                source,
            })?;

    Ok(HvfArm64BootInterruptLines {
        block,
        pmem,
        network,
        vsock,
        balloon,
        entropy,
        memory_hotplug,
        serial,
        vmgenid,
        vmclock,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::error::Error as _;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::Duration;

    use bangbang_runtime::VmmAction;
    use bangbang_runtime::balloon::{
        BalloonConfigInput, BalloonMmioLayout, VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
        VIRTIO_BALLOON_INFLATE_QUEUE_INDEX, VirtioBalloonDeviceNotificationError,
    };
    use bangbang_runtime::block::{
        BlockMmioLayout, DriveConfigInput, VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
        VIRTIO_BLOCK_REQUEST_TYPE_FLUSH, VIRTIO_BLOCK_REQUEST_TYPE_IN, VIRTIO_BLOCK_SECTOR_SIZE,
        VIRTIO_BLOCK_STATUS_OK, VIRTIO_BLOCK_STATUS_SIZE,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::boot_timer::BootTimerMmioLayout;
    use bangbang_runtime::entropy::{
        EntropyMmioLayout, VirtioRngEntropySource, VirtioRngEntropySourceError,
    };
    use bangbang_runtime::fdt::{Arm64FdtGic, Arm64FdtRegion, Arm64FdtTimerInterrupts};
    use bangbang_runtime::interrupt::{
        DeviceInterruptKind, GuestInterruptLine, InterruptSignalError, InterruptSink,
    };
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryRange};
    use bangbang_runtime::memory_hotplug::{
        MemoryHotplugConfig, MemoryHotplugConfigInput, MemoryHotplugSizeUpdateInput,
        VIRTIO_MEM_DEFAULT_REGION_ADDRESS, VIRTIO_MEM_REQUEST_SIZE, VIRTIO_MEM_RESPONSE_SIZE,
        VirtioMemAppliedMutation, VirtioMemMmioLayout, VirtioMemMutation, VirtioMemMutationError,
        VirtioMemMutationExecutor, VirtioMemMutationKind, VirtioMemMutationRollbackError,
    };
    use bangbang_runtime::metrics::{
        BalloonDeviceMetrics, BlockDeviceMetrics, BlockDeviceMetricsByDrive, EntropyDeviceMetrics,
        NetworkInterfaceMetrics, NetworkInterfaceMetricsByInterface, PmemDeviceMetrics,
        PmemDeviceMetricsByDevice, SharedBalloonDeviceMetrics, SharedBlockDeviceMetricsRegistry,
        SharedEntropyDeviceMetrics, SharedNetworkInterfaceMetricsRegistry,
        SharedPmemDeviceMetricsRegistry, SharedVsockDeviceMetrics, VsockDeviceMetrics,
    };
    use bangbang_runtime::mmio::{
        MmioAccess, MmioAccessBytes, MmioDispatchOutcome, MmioDispatcher, MmioHandler,
        MmioHandlerError, MmioOperation, MmioRegionId,
    };
    use bangbang_runtime::network::{
        NetworkInterfaceConfigInput, NetworkMmioLayout, VIRTIO_NET_RX_QUEUE_INDEX,
        VIRTIO_NET_TX_HEADER_SIZE, VIRTIO_NET_TX_QUEUE_INDEX, VirtioNetworkRxPacket,
        VirtioNetworkRxPacketSource, VirtioNetworkRxPacketSourceError, VirtioNetworkTxFrame,
        VirtioNetworkTxPacketSink, VirtioNetworkTxPacketSinkError,
    };
    use bangbang_runtime::pmem::{
        PmemConfigInput, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT, VIRTIO_PMEM_REQUEST_SIZE,
        VIRTIO_PMEM_REQUEST_TYPE_FLUSH, VIRTIO_PMEM_STATUS_SIZE, VirtioPmemFlushStatus,
    };
    use bangbang_runtime::rtc::RtcMmioLayout;
    use bangbang_runtime::serial::{SharedSerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::startup::{
        Arm64BootBalloonNotificationDispatches, Arm64BootBlockNotificationDispatches,
        Arm64BootEntropyDeviceConfig, Arm64BootEntropyNotificationDispatches,
        Arm64BootEntropySource, Arm64BootEntropySourceError, Arm64BootEntropySourceProvider,
        Arm64BootMemoryHotplugDeviceConfig, Arm64BootMemoryHotplugNotificationDispatches,
        Arm64BootNetworkNotificationDispatches, Arm64BootNetworkNotificationOutcome,
        Arm64BootNetworkPacketIo, Arm64BootNetworkPacketIoError, Arm64BootNetworkPacketIoProvider,
        Arm64BootPmemNotificationDispatches, Arm64BootResourceConfig, Arm64BootResources,
        Arm64BootRuntimeResources, Arm64BootVsockNotificationDispatches,
        update_memory_hotplug_config_for_device,
    };
    use bangbang_runtime::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK, VirtioMmioRegister,
    };
    use bangbang_runtime::virtio_queue::{
        VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE,
    };
    use bangbang_runtime::vsock::{
        VIRTIO_VSOCK_EVENT_QUEUE_INDEX, VIRTIO_VSOCK_PACKET_HEADER_SIZE,
        VIRTIO_VSOCK_RX_QUEUE_INDEX, VIRTIO_VSOCK_TX_QUEUE_INDEX, VirtioVsockPacketHeader,
        VsockConfigInput, VsockMmioLayout,
    };

    use super::{
        HvfArm64BootBalloonDeviceConfig, HvfArm64BootBalloonNotificationDispatchError,
        HvfArm64BootBlockNotificationDispatchError, HvfArm64BootEntropyDeviceConfig,
        HvfArm64BootEntropyNotificationDispatchError, HvfArm64BootInterruptLinePurpose,
        HvfArm64BootInterruptRequest, HvfArm64BootLimiterRetryWakeupQuiescenceError,
        HvfArm64BootLimiterRetryWakeupScheduler,
        HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError,
        HvfArm64BootLimiterRetryWakeupSchedulerStatus, HvfArm64BootLimiterRetryWakeupToken,
        HvfArm64BootMemoryHotplugDeviceConfig, HvfArm64BootMemoryHotplugNotificationDispatchError,
        HvfArm64BootMmioDispatcherError, HvfArm64BootNetworkNotificationDispatchError,
        HvfArm64BootPmemNotificationDispatchError, HvfArm64BootRunLoopOutcome,
        HvfArm64BootRunLoopStopToken, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSessionError, HvfArm64BootTimerDeviceConfig,
        HvfArm64BootVsockNotificationDispatchError, allocate_interrupt_lines,
        collect_balloon_notification_dispatches, collect_block_notification_dispatches,
        collect_entropy_notification_dispatches, collect_memory_hotplug_notification_dispatches,
        collect_network_notification_dispatches, collect_vsock_notification_dispatches,
        dispatch_memory_hotplug_runtime_notifications_with_executor,
        dispatch_network_runtime_notifications_with_packet_io, lock_boot_mmio_dispatcher,
        quiesce_limiter_retry_wakeups, record_entropy_dispatch_metrics,
        record_pmem_dispatch_metrics, run_boot_session_loop, run_boot_session_vcpu_step,
        signal_balloon_queue_interrupts, signal_block_queue_interrupts,
        signal_entropy_queue_interrupts, signal_memory_hotplug_queue_interrupts,
        signal_network_queue_interrupts, signal_pmem_queue_interrupts,
        signal_vsock_queue_interrupts, validate_single_vcpu,
    };
    use crate::exit::{
        HvfExceptionExit, HvfHvcExit, HvfMmioAccessSize, HvfMmioDirection, HvfMmioRegister,
        HvfSys64Exit,
    };
    use crate::gic::{HvfGicInterruptRange, HvfGicMetadata, HvfGicRedistributor, HvfGicRegion};
    use crate::runner::{HvfVcpuRunStepOutcome, HvfVcpuRunnerError};

    static NEXT_TEST_FILE_ID: AtomicU64 = AtomicU64::new(0);

    const TEST_MEMORY_MIB: u64 = 8;
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;

    fn wait_for_limiter_retry_scheduler_status(
        scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
        expected: HvfArm64BootLimiterRetryWakeupSchedulerStatus,
    ) {
        let mut state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
        while state.status != expected {
            state = super::wait_limiter_retry_wakeup_state(&scheduler.shared, state);
        }
    }

    fn wait_for_limiter_retry_publication_idle(
        scheduler: &HvfArm64BootLimiterRetryWakeupScheduler,
    ) {
        let mut state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
        while state.publication_in_flight {
            state = super::wait_limiter_retry_wakeup_state(&scheduler.shared, state);
        }
    }

    #[test]
    fn limiter_retry_quiescence_waits_for_in_flight_and_republishes_drained_wakeup() {
        let wakeup_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (publication_sender, publication_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-in-flight",
            wakeup_token.clone(),
            move || {
                publication_sender
                    .send(())
                    .expect("test should observe retry wakeup publication");
                release_receiver
                    .recv()
                    .expect("test should release retry wakeup publication");
                Err::<(), &'static str>("expected cancellation failure")
            },
        )
        .expect("test retry wakeup scheduler should start");
        scheduler.schedule_after(Some(Duration::ZERO));
        publication_receiver
            .recv()
            .expect("scheduled retry wakeup should begin publication");

        thread::scope(|scope| {
            let (guard_sender, guard_receiver) = mpsc::channel();
            let scheduler_ref = &scheduler;
            scope.spawn(move || {
                guard_sender
                    .send(scheduler_ref.quiesce())
                    .expect("test should receive quiescence result");
            });

            wait_for_limiter_retry_scheduler_status(
                &scheduler,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Quiesced,
            );
            assert!(matches!(
                guard_receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));

            release_sender
                .send(())
                .expect("test should release in-flight publication");
            let guard = guard_receiver
                .recv()
                .expect("quiescence acquisition should complete")
                .expect("running scheduler should quiesce");
            assert!(wakeup_token.take_wakeup_request());
            guard.defer_publication();
            assert_eq!(
                publication_receiver.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            );

            drop(guard);
            publication_receiver
                .recv()
                .expect("drained retry wakeup should be republished");
            assert!(wakeup_token.take_wakeup_request());
            release_sender
                .send(())
                .expect("test should release deferred publication");
        });

        wait_for_limiter_retry_publication_idle(&scheduler);
        scheduler.stop();
    }

    #[test]
    fn limiter_retry_quiescence_coalesces_deferred_and_overdue_work() {
        let wakeup_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (publication_sender, publication_receiver) = mpsc::channel();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-coalesce",
            wakeup_token.clone(),
            move || {
                publication_sender
                    .send(())
                    .expect("test should observe retry wakeup publication");
            },
        )
        .expect("test retry wakeup scheduler should start");

        let guard = scheduler
            .quiesce()
            .expect("running scheduler should quiesce");
        scheduler.schedule_after(Some(Duration::ZERO));
        guard.defer_publication();
        assert_eq!(
            publication_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        drop(guard);
        publication_receiver
            .recv()
            .expect("overdue retry wakeup should publish after release");
        wait_for_limiter_retry_publication_idle(&scheduler);
        assert!(wakeup_token.take_wakeup_request());
        assert_eq!(
            publication_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );
        {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert_eq!(state.deadline, None);
            assert!(!state.deferred_publication);
        }

        scheduler.stop();
    }

    #[test]
    fn limiter_retry_quiescence_preserves_future_deadline_after_deferred_publication() {
        let wakeup_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (publication_sender, publication_receiver) = mpsc::channel();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-future",
            wakeup_token.clone(),
            move || {
                publication_sender
                    .send(())
                    .expect("test should observe retry wakeup publication");
            },
        )
        .expect("test retry wakeup scheduler should start");

        let guard = scheduler
            .quiesce()
            .expect("running scheduler should quiesce");
        scheduler.schedule_after(Some(Duration::from_secs(3_600)));
        guard.defer_publication();
        drop(guard);

        publication_receiver
            .recv()
            .expect("deferred retry wakeup should publish immediately");
        wait_for_limiter_retry_publication_idle(&scheduler);
        assert!(wakeup_token.take_wakeup_request());
        {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert!(state.deadline.is_some());
            assert!(!state.deferred_publication);
        }

        scheduler.schedule_after(None);
        {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert_eq!(state.deadline, None);
            assert!(!state.deferred_publication);
        }
        scheduler.stop();
    }

    #[test]
    fn limiter_retry_schedule_cancellation_discards_deferred_publication() {
        let (publication_sender, publication_receiver) = mpsc::channel();
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-limiter-retry-cancel",
            HvfArm64BootLimiterRetryWakeupToken::default(),
            move || {
                publication_sender
                    .send(())
                    .expect("test should observe retry wakeup publication");
            },
        )
        .expect("test retry wakeup scheduler should start");

        let guard = scheduler
            .quiesce()
            .expect("running scheduler should quiesce");
        guard.defer_publication();
        scheduler.schedule_after(None);
        drop(guard);
        wait_for_limiter_retry_scheduler_status(
            &scheduler,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running,
        );
        assert_eq!(
            publication_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        );

        scheduler.stop();
    }

    #[test]
    fn limiter_retry_session_quiescence_drains_and_republishes_both_tokens() {
        let block_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let entropy_token = HvfArm64BootLimiterRetryWakeupToken::default();
        let (block_sender, block_receiver) = mpsc::channel();
        let (entropy_sender, entropy_receiver) = mpsc::channel();
        let mut block_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
            "bangbang-hvf-test-block-retry-quiescence",
            block_token.clone(),
            move || {
                block_sender
                    .send(())
                    .expect("test should observe block retry wakeup publication");
            },
        )
        .expect("test block retry wakeup scheduler should start");
        let mut entropy_scheduler =
            HvfArm64BootLimiterRetryWakeupScheduler::start_with_cancellation(
                "bangbang-hvf-test-entropy-retry-quiescence",
                entropy_token.clone(),
                move || {
                    entropy_sender
                        .send(())
                        .expect("test should observe entropy retry wakeup publication");
                },
            )
            .expect("test entropy retry wakeup scheduler should start");
        block_token.request_wakeup();
        entropy_token.request_wakeup();

        let guard = quiesce_limiter_retry_wakeups(
            &block_token,
            &block_scheduler,
            &entropy_token,
            &entropy_scheduler,
        )
        .expect("both running retry wakeup schedulers should quiesce");
        assert!(!block_token.take_wakeup_request());
        assert!(!entropy_token.take_wakeup_request());
        assert_eq!(
            quiesce_limiter_retry_wakeups(
                &block_token,
                &block_scheduler,
                &entropy_token,
                &entropy_scheduler,
            )
            .expect_err("duplicate session quiescence should fail"),
            HvfArm64BootLimiterRetryWakeupQuiescenceError::BlockAlreadyQuiesced
        );
        assert_eq!(block_receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
        assert_eq!(entropy_receiver.try_recv(), Err(mpsc::TryRecvError::Empty));

        drop(guard);
        block_receiver
            .recv()
            .expect("block retry wakeup should be republished");
        entropy_receiver
            .recv()
            .expect("entropy retry wakeup should be republished");
        wait_for_limiter_retry_publication_idle(&block_scheduler);
        wait_for_limiter_retry_publication_idle(&entropy_scheduler);
        assert!(block_token.take_wakeup_request());
        assert!(entropy_token.take_wakeup_request());

        block_scheduler.stop();
        entropy_scheduler.stop();
    }

    #[test]
    fn limiter_retry_session_quiescence_rolls_back_when_entropy_is_stopped() {
        let mut block_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let mut entropy_scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        entropy_scheduler.stop();

        let err = quiesce_limiter_retry_wakeups(
            &HvfArm64BootLimiterRetryWakeupToken::default(),
            &block_scheduler,
            &HvfArm64BootLimiterRetryWakeupToken::default(),
            &entropy_scheduler,
        )
        .expect_err("stopped entropy scheduler should reject quiescence");

        assert_eq!(
            err,
            HvfArm64BootLimiterRetryWakeupQuiescenceError::EntropyStopped
        );
        {
            let state = super::lock_limiter_retry_wakeup_state(&block_scheduler.shared);
            assert_eq!(
                state.status,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running
            );
        }
        block_scheduler.stop();
    }

    #[test]
    fn limiter_retry_quiescence_guard_resumes_scheduler_during_unwind() {
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = scheduler
                .quiesce()
                .expect("inactive running scheduler should quiesce");
            panic!("test should unwind through quiescence guard");
        }));

        assert!(unwind.is_err());
        {
            let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
            assert_eq!(
                state.status,
                HvfArm64BootLimiterRetryWakeupSchedulerStatus::Running
            );
        }
        scheduler.stop();
    }

    #[test]
    fn limiter_retry_stop_wins_over_late_quiescence_guard_drop() {
        let mut scheduler = HvfArm64BootLimiterRetryWakeupScheduler::inactive();
        let guard = scheduler
            .quiesce()
            .expect("inactive running scheduler should quiesce");

        scheduler.stop();
        drop(guard);

        assert_eq!(
            scheduler
                .quiesce()
                .expect_err("stopped scheduler should reject quiescence"),
            HvfArm64BootLimiterRetryWakeupSchedulerQuiescenceError::Stopped
        );
        let state = super::lock_limiter_retry_wakeup_state(&scheduler.shared);
        assert_eq!(
            state.status,
            HvfArm64BootLimiterRetryWakeupSchedulerStatus::Stopped
        );
    }
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;
    const ESR_EC_HVC: u64 = 0x16;
    const ESR_EC_SYS64: u64 = 0x18;
    const ESR_EC_DATA_ABORT_LOWER_EL: u64 = 0x24;
    const ESR_EC_SHIFT: u64 = 26;
    const ESR_ISS_ISV: u64 = 1 << 24;
    const ESR_ISS_SYS64_DIRECTION: u64 = 1;
    const ESR_ISS_SAS_SHIFT: u64 = 22;
    const ESR_ISS_SRT_SHIFT: u64 = 16;
    const ESR_ISS_WNR: u64 = 1 << 6;
    const ESR_ISS_SF: u64 = 1 << 15;
    const TEST_BLOCK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_0000);
    const TEST_PMEM_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_9000);
    const TEST_QUEUE_SIZE: u16 = 4;
    const TEST_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8040_0000);
    const TEST_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8041_0000);
    const TEST_USED_RING: GuestAddress = GuestAddress::new(0x8042_0000);
    const HEADER_ADDR: GuestAddress = GuestAddress::new(0x8043_0000);
    const DATA_ADDR: GuestAddress = GuestAddress::new(0x8044_0000);
    const STATUS_ADDR: GuestAddress = GuestAddress::new(0x8045_0000);
    const TEST_NETWORK_RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8050_0000);
    const TEST_NETWORK_RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8051_0000);
    const TEST_NETWORK_RX_USED_RING: GuestAddress = GuestAddress::new(0x8052_0000);
    const TEST_NETWORK_TX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8060_0000);
    const TEST_NETWORK_TX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8061_0000);
    const TEST_NETWORK_TX_USED_RING: GuestAddress = GuestAddress::new(0x8062_0000);
    const TEST_NETWORK_TX_HEADER: GuestAddress = GuestAddress::new(0x8063_0000);
    const TEST_NETWORK_TX_PAYLOAD: GuestAddress = GuestAddress::new(0x8064_0000);
    const TEST_VSOCK_RX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8070_0000);
    const TEST_VSOCK_RX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8071_0000);
    const TEST_VSOCK_RX_USED_RING: GuestAddress = GuestAddress::new(0x8072_0000);
    const TEST_VSOCK_TX_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8073_0000);
    const TEST_VSOCK_TX_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8074_0000);
    const TEST_VSOCK_TX_USED_RING: GuestAddress = GuestAddress::new(0x8075_0000);
    const TEST_VSOCK_EVENT_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x8076_0000);
    const TEST_VSOCK_EVENT_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x8077_0000);
    const TEST_VSOCK_EVENT_USED_RING: GuestAddress = GuestAddress::new(0x8078_0000);
    const TEST_VSOCK_HEADER: GuestAddress = GuestAddress::new(0x8079_0000);
    const TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x807a_0000);
    const TEST_BALLOON_INFLATE_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x807a_1000);
    const TEST_BALLOON_INFLATE_USED_RING: GuestAddress = GuestAddress::new(0x807a_2000);
    const TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE: GuestAddress = GuestAddress::new(0x807a_3000);
    const TEST_BALLOON_DEFLATE_AVAILABLE_RING: GuestAddress = GuestAddress::new(0x807a_4000);
    const TEST_BALLOON_DEFLATE_USED_RING: GuestAddress = GuestAddress::new(0x807a_5000);
    const TEST_BALLOON_PFN_PAYLOAD: GuestAddress = GuestAddress::new(0x807a_6000);
    const TEST_BALLOON_MAPPED_PFN: u32 = 0x80000;
    const TEST_VIRTIO_MEM_REQ_PLUG: u16 = 0;
    const TEST_VIRTIO_MEM_REQ_STATE: u16 = 3;
    const TEST_AVAILABLE_RING_IDX_OFFSET: u64 = 2;
    const TEST_AVAILABLE_RING_RING_OFFSET: u64 = 4;
    const TEST_AVAILABLE_RING_ENTRY_SIZE: u64 = 2;
    const QUEUE_CONFIG_STATUS: u32 = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    const DRIVER_OK_STATUS: u32 = QUEUE_CONFIG_STATUS | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    const PSCI_VERSION: u64 = 0x8400_0000;
    const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
    const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
    const PSCI_VERSION_0_2: u64 = 0x0000_0002;
    const PSCI_RET_SUCCESS: u64 = 0;
    const TEST_NETWORK_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_4000);
    const TEST_RTC_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_1000);
    const TEST_ENTROPY_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_7000);
    const TEST_BALLOON_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_8000);
    const TEST_MEMORY_HOTPLUG_MMIO_BASE: GuestAddress = GuestAddress::new(0x4000_a000);

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

    #[derive(Debug, Default)]
    struct RecordingTxPacketSink {
        packets: Vec<Vec<u8>>,
    }

    impl VirtioNetworkTxPacketSink for RecordingTxPacketSink {
        fn transmit_frame(
            &mut self,
            memory: &GuestMemory,
            frame: &VirtioNetworkTxFrame,
        ) -> Result<(), VirtioNetworkTxPacketSinkError> {
            let payload_len = usize::try_from(frame.payload_len())
                .expect("test TX payload length should fit in usize");
            let mut packet = Vec::new();
            packet
                .try_reserve_exact(payload_len)
                .expect("test packet allocation should succeed");
            for segment in frame.payload_segments() {
                let len =
                    usize::try_from(segment.len()).expect("test TX segment should fit in usize");
                let mut bytes = vec![0; len];
                memory
                    .read_slice(&mut bytes, segment.address())
                    .expect("test TX segment should read");
                packet.extend(bytes);
            }
            self.packets.push(packet);

            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct EmptyRxPacketSource {
        peek_calls: usize,
    }

    impl VirtioNetworkRxPacketSource for EmptyRxPacketSource {
        fn peek_packet(
            &mut self,
        ) -> Result<Option<VirtioNetworkRxPacket<'_>>, VirtioNetworkRxPacketSourceError> {
            self.peek_calls += 1;
            Ok(None)
        }

        fn consume_packet(&mut self) {}
    }

    #[derive(Debug)]
    struct RecordingNetworkPacketIoProvider {
        iface_id: String,
        tx_sink: RecordingTxPacketSink,
        rx_source: EmptyRxPacketSource,
        requested_ifaces: Vec<String>,
        fail: bool,
    }

    impl RecordingNetworkPacketIoProvider {
        fn for_iface(iface_id: &str) -> Self {
            Self {
                iface_id: iface_id.to_string(),
                tx_sink: RecordingTxPacketSink::default(),
                rx_source: EmptyRxPacketSource::default(),
                requested_ifaces: Vec::new(),
                fail: false,
            }
        }

        fn failing_for(iface_id: &str) -> Self {
            Self {
                fail: true,
                ..Self::for_iface(iface_id)
            }
        }
    }

    impl Arm64BootNetworkPacketIoProvider for RecordingNetworkPacketIoProvider {
        fn packet_io(
            &mut self,
            device: &bangbang_runtime::startup::Arm64BootNetworkDevice,
        ) -> Result<Arm64BootNetworkPacketIo<'_>, Arm64BootNetworkPacketIoError> {
            let iface_id = device.registration.iface_id();
            self.requested_ifaces.push(iface_id.to_string());
            if iface_id != self.iface_id {
                return Err(Arm64BootNetworkPacketIoError::new(format!(
                    "missing test packet I/O for interface {iface_id}"
                )));
            }
            if self.fail {
                return Err(Arm64BootNetworkPacketIoError::new(format!(
                    "test packet I/O unavailable for interface {iface_id}"
                )));
            }

            Ok(Arm64BootNetworkPacketIo::new(
                &mut self.tx_sink,
                &mut self.rx_source,
            ))
        }
    }

    #[derive(Debug, Default)]
    struct RecordingEntropySource {
        calls: Vec<usize>,
        next_byte: u8,
    }

    impl VirtioRngEntropySource for RecordingEntropySource {
        fn fill_entropy(
            &mut self,
            destination: &mut [u8],
        ) -> Result<(), VirtioRngEntropySourceError> {
            self.calls.push(destination.len());
            for byte in destination {
                *byte = self.next_byte;
                self.next_byte = self.next_byte.wrapping_add(1);
            }

            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingEntropySourceProvider {
        source: RecordingEntropySource,
        requested_regions: Vec<MmioRegionId>,
    }

    impl Arm64BootEntropySourceProvider for RecordingEntropySourceProvider {
        fn entropy_source(
            &mut self,
            device: &bangbang_runtime::startup::Arm64BootEntropyDevice,
        ) -> Result<Arm64BootEntropySource<'_>, Arm64BootEntropySourceError> {
            self.requested_regions.push(device.registration.region_id());
            Ok(Arm64BootEntropySource::new(&mut self.source))
        }
    }

    #[derive(Debug, Default)]
    struct RecordingVirtioMemMutationExecutor {
        applied: Vec<VirtioMemMutation>,
        rolled_back: Vec<VirtioMemMutation>,
    }

    impl VirtioMemMutationExecutor for RecordingVirtioMemMutationExecutor {
        fn apply(
            &mut self,
            _memory: &mut GuestMemory,
            mutation: VirtioMemMutation,
        ) -> Result<VirtioMemAppliedMutation, VirtioMemMutationError> {
            self.applied.push(mutation.clone());
            Ok(VirtioMemAppliedMutation::new(mutation))
        }

        fn rollback(
            &mut self,
            _memory: &mut GuestMemory,
            applied: VirtioMemAppliedMutation,
        ) -> Result<(), VirtioMemMutationRollbackError> {
            self.rolled_back.push(applied.mutation().clone());
            Ok(())
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

    #[derive(Debug)]
    struct WrongNetworkHandler;

    impl MmioHandler for WrongNetworkHandler {
        fn read(&mut self, _access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
            Err(MmioHandlerError::new("wrong network handler read"))
        }

        fn write(
            &mut self,
            _access: MmioAccess,
            _data: MmioAccessBytes,
        ) -> Result<(), MmioHandlerError> {
            Err(MmioHandlerError::new("wrong network handler write"))
        }
    }

    #[derive(Debug)]
    struct WrongBalloonHandler;

    impl MmioHandler for WrongBalloonHandler {
        fn read(&mut self, _access: MmioAccess) -> Result<MmioAccessBytes, MmioHandlerError> {
            Err(MmioHandlerError::new("wrong balloon handler read"))
        }

        fn write(
            &mut self,
            _access: MmioAccess,
            _data: MmioAccessBytes,
        ) -> Result<(), MmioHandlerError> {
            Err(MmioHandlerError::new("wrong balloon handler write"))
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

    #[derive(Debug)]
    struct RecordingBootSessionRunLoopSession {
        run_results: VecDeque<Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError>>,
        dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootBlockNotificationDispatches,
                HvfArm64BootBlockNotificationDispatchError,
            >,
        >,
        pmem_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootPmemNotificationDispatches,
                HvfArm64BootPmemNotificationDispatchError,
            >,
        >,
        monitor_wakeup_results: VecDeque<bool>,
        network_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootNetworkNotificationDispatches,
                HvfArm64BootNetworkNotificationDispatchError,
            >,
        >,
        vsock_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootVsockNotificationDispatches,
                HvfArm64BootVsockNotificationDispatchError,
            >,
        >,
        balloon_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootBalloonNotificationDispatches,
                HvfArm64BootBalloonNotificationDispatchError,
            >,
        >,
        memory_hotplug_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootMemoryHotplugNotificationDispatches,
                HvfArm64BootMemoryHotplugNotificationDispatchError,
            >,
        >,
        entropy_dispatch_results: VecDeque<
            Result<
                super::HvfArm64BootEntropyNotificationDispatches,
                HvfArm64BootEntropyNotificationDispatchError,
            >,
        >,
        timer_results: VecDeque<Result<(), HvfVcpuRunnerError>>,
        events: Vec<&'static str>,
        request_stop_on_run: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_pmem_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_network_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_vsock_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_balloon_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_memory_hotplug_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_entropy_dispatch: Option<HvfArm64BootRunLoopStopToken>,
        request_stop_on_timer: Option<HvfArm64BootRunLoopStopToken>,
        control_wakeup_requested: bool,
        wakeup_requested: bool,
        block_retry_wakeup_requested: bool,
        entropy_retry_wakeup_requested: bool,
        scheduled_block_retry_wakeups: Vec<Option<Duration>>,
        scheduled_entropy_retry_wakeups: Vec<Option<Duration>>,
    }

    impl RecordingBootSessionRunLoopSession {
        fn new(run_results: impl IntoIterator<Item = HvfVcpuRunStepOutcome>) -> Self {
            Self {
                run_results: run_results.into_iter().map(Ok).collect(),
                dispatch_results: VecDeque::new(),
                pmem_dispatch_results: VecDeque::new(),
                monitor_wakeup_results: VecDeque::new(),
                network_dispatch_results: VecDeque::new(),
                vsock_dispatch_results: VecDeque::new(),
                balloon_dispatch_results: VecDeque::new(),
                memory_hotplug_dispatch_results: VecDeque::new(),
                entropy_dispatch_results: VecDeque::new(),
                timer_results: VecDeque::new(),
                events: Vec::new(),
                request_stop_on_run: None,
                request_stop_on_dispatch: None,
                request_stop_on_pmem_dispatch: None,
                request_stop_on_network_dispatch: None,
                request_stop_on_vsock_dispatch: None,
                request_stop_on_balloon_dispatch: None,
                request_stop_on_memory_hotplug_dispatch: None,
                request_stop_on_entropy_dispatch: None,
                request_stop_on_timer: None,
                control_wakeup_requested: false,
                wakeup_requested: false,
                block_retry_wakeup_requested: false,
                entropy_retry_wakeup_requested: false,
                scheduled_block_retry_wakeups: Vec::new(),
                scheduled_entropy_retry_wakeups: Vec::new(),
            }
        }

        fn with_run_error(source: HvfVcpuRunnerError) -> Self {
            Self {
                run_results: VecDeque::from([Err(source)]),
                dispatch_results: VecDeque::new(),
                pmem_dispatch_results: VecDeque::new(),
                monitor_wakeup_results: VecDeque::new(),
                network_dispatch_results: VecDeque::new(),
                vsock_dispatch_results: VecDeque::new(),
                balloon_dispatch_results: VecDeque::new(),
                memory_hotplug_dispatch_results: VecDeque::new(),
                entropy_dispatch_results: VecDeque::new(),
                timer_results: VecDeque::new(),
                events: Vec::new(),
                request_stop_on_run: None,
                request_stop_on_dispatch: None,
                request_stop_on_pmem_dispatch: None,
                request_stop_on_network_dispatch: None,
                request_stop_on_vsock_dispatch: None,
                request_stop_on_balloon_dispatch: None,
                request_stop_on_memory_hotplug_dispatch: None,
                request_stop_on_entropy_dispatch: None,
                request_stop_on_timer: None,
                control_wakeup_requested: false,
                wakeup_requested: false,
                block_retry_wakeup_requested: false,
                entropy_retry_wakeup_requested: false,
                scheduled_block_retry_wakeups: Vec::new(),
                scheduled_entropy_retry_wakeups: Vec::new(),
            }
        }

        fn push_dispatch_error(&mut self, source: HvfArm64BootBlockNotificationDispatchError) {
            self.dispatch_results.push_back(Err(source));
        }

        fn push_block_retry_dispatch(&mut self, retry_after: Duration) {
            self.dispatch_results.push_back(Ok(
                super::HvfArm64BootBlockNotificationDispatches::new_for_test_with_rate_limiter_retry_after(
                    retry_after,
                ),
            ));
        }

        fn push_entropy_retry_dispatch(&mut self, retry_after: Duration) {
            self.entropy_dispatch_results.push_back(Ok(
                super::HvfArm64BootEntropyNotificationDispatches::new_for_test_with_rate_limiter_retry_after(
                    retry_after,
                ),
            ));
        }

        fn push_pmem_dispatch_error(&mut self, source: HvfArm64BootPmemNotificationDispatchError) {
            self.pmem_dispatch_results.push_back(Err(source));
        }

        fn push_monitor_wakeup(&mut self) {
            self.monitor_wakeup_results.push_back(true);
        }

        fn push_network_dispatch_error(
            &mut self,
            source: HvfArm64BootNetworkNotificationDispatchError,
        ) {
            self.network_dispatch_results.push_back(Err(source));
        }

        fn push_vsock_dispatch_error(
            &mut self,
            source: HvfArm64BootVsockNotificationDispatchError,
        ) {
            self.vsock_dispatch_results.push_back(Err(source));
        }

        fn push_balloon_dispatch_error(
            &mut self,
            source: HvfArm64BootBalloonNotificationDispatchError,
        ) {
            self.balloon_dispatch_results.push_back(Err(source));
        }

        fn push_memory_hotplug_dispatch_error(
            &mut self,
            source: HvfArm64BootMemoryHotplugNotificationDispatchError,
        ) {
            self.memory_hotplug_dispatch_results.push_back(Err(source));
        }

        fn push_entropy_dispatch_error(
            &mut self,
            source: HvfArm64BootEntropyNotificationDispatchError,
        ) {
            self.entropy_dispatch_results.push_back(Err(source));
        }

        fn push_timer_error(&mut self, source: HvfVcpuRunnerError) {
            self.timer_results.push_back(Err(source));
        }

        fn request_stop_on_run(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_run = Some(stop_token);
        }

        fn request_stop_on_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_dispatch = Some(stop_token);
        }

        fn request_stop_on_pmem_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_pmem_dispatch = Some(stop_token);
        }

        fn request_stop_on_network_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_network_dispatch = Some(stop_token);
        }

        fn request_stop_on_vsock_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_vsock_dispatch = Some(stop_token);
        }

        fn request_stop_on_balloon_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_balloon_dispatch = Some(stop_token);
        }

        fn request_stop_on_memory_hotplug_dispatch(
            &mut self,
            stop_token: HvfArm64BootRunLoopStopToken,
        ) {
            self.request_stop_on_memory_hotplug_dispatch = Some(stop_token);
        }

        fn request_stop_on_entropy_dispatch(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_entropy_dispatch = Some(stop_token);
        }

        fn request_stop_on_timer(&mut self, stop_token: HvfArm64BootRunLoopStopToken) {
            self.request_stop_on_timer = Some(stop_token);
        }

        const fn request_run_loop_wakeup(&mut self) {
            self.wakeup_requested = true;
        }

        const fn request_run_loop_block_retry_wakeup(&mut self) {
            self.block_retry_wakeup_requested = true;
        }

        const fn request_run_loop_entropy_retry_wakeup(&mut self) {
            self.entropy_retry_wakeup_requested = true;
        }

        const fn request_run_loop_control_wakeup(&mut self) {
            self.control_wakeup_requested = true;
        }
    }

    impl super::BootSessionRunLoopSession for RecordingBootSessionRunLoopSession {
        fn start_run_loop_vsock_wakeup_monitor(
            &mut self,
        ) -> Result<
            super::HvfArm64BootRunLoopWakeupMonitor,
            super::HvfArm64BootRunLoopWakeupMonitorError,
        > {
            let completed_wakeup = self.monitor_wakeup_results.pop_front().unwrap_or(false);
            if completed_wakeup {
                self.wakeup_requested = true;
            }

            Ok(super::HvfArm64BootRunLoopWakeupMonitor::completed_for_test(
                completed_wakeup,
            ))
        }

        fn take_run_loop_wakeup_request(&mut self) -> bool {
            let wakeup_requested = self.wakeup_requested;
            self.wakeup_requested = false;
            wakeup_requested
        }

        fn take_run_loop_control_wakeup_request(&mut self) -> bool {
            let wakeup_requested = self.control_wakeup_requested;
            self.control_wakeup_requested = false;
            wakeup_requested
        }

        fn take_run_loop_block_retry_wakeup_request(&mut self) -> bool {
            let wakeup_requested = self.block_retry_wakeup_requested;
            self.block_retry_wakeup_requested = false;
            wakeup_requested
        }

        fn schedule_run_loop_block_retry_wakeup(&mut self, retry_after: Option<Duration>) {
            self.scheduled_block_retry_wakeups.push(retry_after);
        }

        fn take_run_loop_entropy_retry_wakeup_request(&mut self) -> bool {
            let wakeup_requested = self.entropy_retry_wakeup_requested;
            self.entropy_retry_wakeup_requested = false;
            wakeup_requested
        }

        fn schedule_run_loop_entropy_retry_wakeup(&mut self, retry_after: Option<Duration>) {
            self.scheduled_entropy_retry_wakeups.push(retry_after);
        }

        fn run_loop_vcpu_step(&mut self) -> Result<HvfVcpuRunStepOutcome, HvfVcpuRunnerError> {
            self.events.push("run");
            if let Some(stop_token) = self.request_stop_on_run.take() {
                stop_token.request_stop();
            }

            self.run_results
                .pop_front()
                .expect("test run result should be queued")
        }

        fn handle_run_loop_virtual_timer(&mut self) -> Result<(), HvfVcpuRunnerError> {
            self.events.push("timer");
            if let Some(stop_token) = self.request_stop_on_timer.take() {
                stop_token.request_stop();
            }

            self.timer_results.pop_front().unwrap_or(Ok(()))
        }

        fn dispatch_run_loop_block_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootBlockNotificationDispatches,
            HvfArm64BootBlockNotificationDispatchError,
        > {
            self.events.push("dispatch");
            if let Some(stop_token) = self.request_stop_on_dispatch.take() {
                stop_token.request_stop();
            }

            self.dispatch_results.pop_front().unwrap_or_else(|| {
                Ok(super::HvfArm64BootBlockNotificationDispatches::new(
                    Vec::new(),
                ))
            })
        }

        fn dispatch_run_loop_pmem_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootPmemNotificationDispatches,
            HvfArm64BootPmemNotificationDispatchError,
        > {
            self.events.push("pmem-dispatch");
            if let Some(stop_token) = self.request_stop_on_pmem_dispatch.take() {
                stop_token.request_stop();
            }

            self.pmem_dispatch_results.pop_front().unwrap_or_else(|| {
                Ok(super::HvfArm64BootPmemNotificationDispatches::new(
                    Vec::new(),
                ))
            })
        }

        fn dispatch_run_loop_network_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootNetworkNotificationDispatches,
            HvfArm64BootNetworkNotificationDispatchError,
        > {
            self.events.push("network-dispatch");
            if let Some(stop_token) = self.request_stop_on_network_dispatch.take() {
                stop_token.request_stop();
            }

            self.network_dispatch_results
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(super::HvfArm64BootNetworkNotificationDispatches::new(
                        Vec::new(),
                    ))
                })
        }

        fn dispatch_run_loop_vsock_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootVsockNotificationDispatches,
            HvfArm64BootVsockNotificationDispatchError,
        > {
            self.events.push("vsock-dispatch");
            if let Some(stop_token) = self.request_stop_on_vsock_dispatch.take() {
                stop_token.request_stop();
            }

            self.vsock_dispatch_results.pop_front().unwrap_or_else(|| {
                Ok(super::HvfArm64BootVsockNotificationDispatches::new(
                    Vec::new(),
                ))
            })
        }

        fn dispatch_run_loop_balloon_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootBalloonNotificationDispatches,
            HvfArm64BootBalloonNotificationDispatchError,
        > {
            self.events.push("balloon-dispatch");
            if let Some(stop_token) = self.request_stop_on_balloon_dispatch.take() {
                stop_token.request_stop();
            }

            self.balloon_dispatch_results
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(super::HvfArm64BootBalloonNotificationDispatches::new(
                        Vec::new(),
                    ))
                })
        }

        fn dispatch_run_loop_memory_hotplug_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootMemoryHotplugNotificationDispatches,
            HvfArm64BootMemoryHotplugNotificationDispatchError,
        > {
            self.events.push("memory-hotplug-dispatch");
            if let Some(stop_token) = self.request_stop_on_memory_hotplug_dispatch.take() {
                stop_token.request_stop();
            }

            self.memory_hotplug_dispatch_results
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(super::HvfArm64BootMemoryHotplugNotificationDispatches::new(
                        Vec::new(),
                    ))
                })
        }

        fn dispatch_run_loop_entropy_notifications(
            &mut self,
        ) -> Result<
            super::HvfArm64BootEntropyNotificationDispatches,
            HvfArm64BootEntropyNotificationDispatchError,
        > {
            self.events.push("entropy-dispatch");
            if let Some(stop_token) = self.request_stop_on_entropy_dispatch.take() {
                stop_token.request_stop();
            }

            self.entropy_dispatch_results
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(super::HvfArm64BootEntropyNotificationDispatches::new(
                        Vec::new(),
                    ))
                })
        }
    }

    fn max_steps(steps: usize) -> NonZeroUsize {
        NonZeroUsize::new(steps).expect("test step limit should be non-zero")
    }

    fn mmio_run_step_outcome() -> HvfVcpuRunStepOutcome {
        let mut dispatcher = MmioDispatcher::new();
        dispatcher
            .insert_region(MmioRegionId::new(7), GuestAddress::new(0x1000), 0x100)
            .expect("test MMIO region should insert");
        let exit = HvfExceptionExit {
            syndrome: data_abort_syndrome(
                HvfMmioAccessSize::Byte,
                HvfMmioDirection::Write,
                HvfMmioRegister::new(0).expect("test MMIO register should exist"),
            ),
            virtual_address: 0x2000,
            physical_address: 0x1040,
        };
        let resolved = exit
            .decode_mmio_access()
            .expect("test MMIO exit should decode")
            .resolve(dispatcher.bus())
            .expect("test MMIO access should resolve");

        HvfVcpuRunStepOutcome::Mmio {
            access: resolved,
            outcome: MmioDispatchOutcome::Write,
        }
    }

    fn hvc_run_step_outcome() -> HvfVcpuRunStepOutcome {
        HvfVcpuRunStepOutcome::Hvc {
            exit: hvc_exit(0),
            function_id: PSCI_VERSION,
            return_value: PSCI_VERSION_0_2,
        }
    }

    fn guest_shutdown_run_step_outcome() -> HvfVcpuRunStepOutcome {
        HvfVcpuRunStepOutcome::GuestShutdown {
            exit: hvc_exit(0),
            function_id: PSCI_SYSTEM_OFF,
            return_value: PSCI_RET_SUCCESS,
        }
    }

    fn guest_reset_run_step_outcome() -> HvfVcpuRunStepOutcome {
        HvfVcpuRunStepOutcome::GuestReset {
            exit: hvc_exit(0),
            function_id: PSCI_SYSTEM_RESET,
            return_value: PSCI_RET_SUCCESS,
        }
    }

    fn sys64_run_step_outcome() -> HvfVcpuRunStepOutcome {
        HvfVcpuRunStepOutcome::Sys64 { exit: sys64_exit() }
    }

    fn sys64_exit() -> HvfSys64Exit {
        let exit = HvfExceptionExit {
            syndrome: sys64_osdlr_syndrome(),
            virtual_address: 0,
            physical_address: 0,
        };

        exit.decode_sys64().expect("test SYS64 exit should decode")
    }

    fn sys64_osdlr_syndrome() -> u64 {
        (ESR_EC_SYS64 << ESR_EC_SHIFT)
            | ESR_ISS_SYS64_DIRECTION
            | (2 << 20)
            | (1 << 10)
            | (3 << 1)
            | (4 << 17)
    }

    fn hvc_exit(immediate: u16) -> HvfHvcExit {
        let exit = HvfExceptionExit {
            syndrome: hvc_syndrome(immediate),
            virtual_address: 0,
            physical_address: 0,
        };

        exit.decode_hvc().expect("test HVC exit should decode")
    }

    fn hvc_syndrome(immediate: u16) -> u64 {
        (ESR_EC_HVC << ESR_EC_SHIFT) | u64::from(immediate)
    }

    fn data_abort_syndrome(
        size: HvfMmioAccessSize,
        direction: HvfMmioDirection,
        register: HvfMmioRegister,
    ) -> u64 {
        let size_bits = match size {
            HvfMmioAccessSize::Byte => 0,
            HvfMmioAccessSize::Halfword => 1,
            HvfMmioAccessSize::Word => 2,
            HvfMmioAccessSize::Doubleword => 3,
        };
        let write_bit = match direction {
            HvfMmioDirection::Read => 0,
            HvfMmioDirection::Write => ESR_ISS_WNR,
        };

        (ESR_EC_DATA_ABORT_LOWER_EL << ESR_EC_SHIFT)
            | ESR_ISS_ISV
            | (size_bits << ESR_ISS_SAS_SHIFT)
            | (u64::from(register.raw_value()) << ESR_ISS_SRT_SHIFT)
            | write_bit
            | ESR_ISS_SF
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

    fn temp_sized_file(name: &str, len: u64) -> TempFile {
        let path = temp_path(name);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("test file should be created");
        file.set_len(len).expect("test file length should be set");
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

    fn add_network(
        controller: &mut bangbang_runtime::VmmController,
        iface_id: &str,
        host_dev_name: &str,
    ) {
        controller
            .handle_action(VmmAction::PutNetworkInterface(
                NetworkInterfaceConfigInput::new(iface_id, iface_id, host_dev_name),
            ))
            .expect("network interface config should be stored");
    }

    fn add_vsock(
        controller: &mut bangbang_runtime::VmmController,
        guest_cid: u32,
        uds_path: &Path,
    ) {
        controller
            .handle_action(VmmAction::PutVsock(VsockConfigInput::new(
                guest_cid,
                uds_path.to_string_lossy().into_owned(),
            )))
            .expect("vsock config should be stored");
    }

    fn add_balloon(controller: &mut bangbang_runtime::VmmController, amount_mib: u32) {
        controller
            .handle_action(VmmAction::PutBalloon(BalloonConfigInput::new(
                amount_mib, false,
            )))
            .expect("balloon config should be stored");
    }

    fn add_memory_hotplug(controller: &mut bangbang_runtime::VmmController) {
        controller
            .handle_action(VmmAction::PutMemoryHotplug(MemoryHotplugConfigInput::new(
                1024, 2, 128,
            )))
            .expect("memory-hotplug config should be stored");
    }

    fn add_pmem(controller: &mut bangbang_runtime::VmmController, id: &str, path: &Path) {
        controller
            .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
                id,
                path.to_string_lossy().into_owned(),
            )))
            .expect("pmem config should be stored");
    }

    fn valid_boot_resource_config(lines: &[GuestInterruptLine]) -> Arm64BootResourceConfig<'_> {
        valid_boot_resource_config_with_network_lines(lines, &[])
    }

    fn valid_boot_resource_config_with_network_lines<'a>(
        block_lines: &'a [GuestInterruptLine],
        network_lines: &'a [GuestInterruptLine],
    ) -> Arm64BootResourceConfig<'a> {
        valid_boot_resource_config_with_network_and_vsock_lines(block_lines, network_lines, None)
    }

    fn valid_boot_resource_config_with_network_and_vsock_lines<'a>(
        block_lines: &'a [GuestInterruptLine],
        network_lines: &'a [GuestInterruptLine],
        vsock_line: Option<GuestInterruptLine>,
    ) -> Arm64BootResourceConfig<'a> {
        Arm64BootResourceConfig {
            vcpu_mpidrs: &[0],
            gic: valid_fdt_gic(),
            timer: Arm64FdtTimerInterrupts::firecracker_default(),
            rtc_device: None,
            serial_device: None,
            vmgenid_interrupt_line: line(127),
            vmclock_interrupt_line: line(126),
            block_mmio_layout: BlockMmioLayout::new(TEST_BLOCK_MMIO_BASE, MmioRegionId::new(1)),
            block_interrupt_lines: block_lines,
            pmem_mmio_layout: PmemMmioLayout::new(TEST_PMEM_MMIO_BASE, MmioRegionId::new(25)),
            pmem_interrupt_lines: &[],
            network_mmio_layout: NetworkMmioLayout::new(
                TEST_NETWORK_MMIO_BASE,
                MmioRegionId::new(50),
            ),
            network_interrupt_lines: network_lines,
            vsock_mmio_layout: VsockMmioLayout::new(
                GuestAddress::new(0x4000_6000),
                MmioRegionId::new(90),
            ),
            vsock_interrupt_line: vsock_line,
            balloon_mmio_layout: BalloonMmioLayout::new(
                TEST_BALLOON_MMIO_BASE,
                MmioRegionId::new(110),
            ),
            balloon_interrupt_line: None,
            memory_hotplug_device: None,
            entropy_device: None,
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

    fn boot_runtime_with_pmem(
        devices: &[&str],
    ) -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-pmem", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        let mut pmem_files = Vec::new();
        pmem_files
            .try_reserve_exact(devices.len())
            .expect("test pmem files should reserve");

        for id in devices {
            let pmem = temp_sized_file(id, VIRTIO_PMEM_ALIGNMENT);
            add_pmem(&mut controller, id, pmem.path());
            pmem_files.push(pmem);
        }

        let interrupt_lines: Vec<_> = (0..devices.len())
            .map(|index| {
                line(32 + u32::try_from(index).expect("test pmem device index should fit in u32"))
            })
            .collect();
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                pmem_interrupt_lines: &interrupt_lines,
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
        drop(pmem_files);
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_networks(
        interfaces: &[(&str, &str)],
    ) -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-networks", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());

        for (iface_id, host_dev_name) in interfaces {
            add_network(&mut controller, iface_id, host_dev_name);
        }

        let interrupt_lines: Vec<_> = (0..interfaces.len())
            .map(|index| {
                line(
                    32 + u32::try_from(index).expect("test network device index should fit in u32"),
                )
            })
            .collect();
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_boot_resource_config_with_network_lines(&[], &interrupt_lines),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_vsock() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-vsock", &arm64_image());
        let uds_path = temp_path("vsock-test.sock");
        let mut controller = controller_with_kernel(kernel.path());
        add_vsock(&mut controller, 42, &uds_path);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            valid_boot_resource_config_with_network_and_vsock_lines(&[], &[], Some(line(32))),
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_balloon() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-balloon", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_balloon(&mut controller, TEST_MEMORY_MIB as u32);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                balloon_interrupt_line: Some(line(32)),
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_memory_hotplug() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher)
    {
        let kernel = temp_file("kernel-with-memory-hotplug", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        add_memory_hotplug(&mut controller);
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                memory_hotplug_device: Some(Arm64BootMemoryHotplugDeviceConfig::new(
                    VirtioMemMmioLayout::new(TEST_MEMORY_HOTPLUG_MMIO_BASE, MmioRegionId::new(120)),
                    line(32),
                )),
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_balloon_stats() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher)
    {
        let kernel = temp_file("kernel-with-balloon-stats", &arm64_image());
        let mut controller = controller_with_kernel(kernel.path());
        controller
            .handle_action(VmmAction::PutBalloon(
                BalloonConfigInput::new(TEST_MEMORY_MIB as u32, false)
                    .with_stats_polling_interval_s(1),
            ))
            .expect("balloon config should be stored");
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                balloon_interrupt_line: Some(line(32)),
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
        let parts = resources.into_parts();

        (parts.memory, parts.runtime, parts.mmio_dispatcher)
    }

    fn boot_runtime_with_entropy() -> (GuestMemory, Arm64BootRuntimeResources, MmioDispatcher) {
        let kernel = temp_file("kernel-with-entropy", &arm64_image());
        let controller = controller_with_kernel(kernel.path());
        let resources = Arm64BootResources::assemble_from_controller(
            &controller,
            Arm64BootResourceConfig {
                entropy_device: Some(Arm64BootEntropyDeviceConfig::new(
                    EntropyMmioLayout::new(TEST_ENTROPY_MMIO_BASE, MmioRegionId::new(100)),
                    line(32),
                )),
                ..valid_boot_resource_config(&[])
            },
        )
        .expect("boot resources should assemble");
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

    fn dispatch_boot_pmem_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootPmemNotificationDispatches {
        runtime
            .dispatch_pmem_queue_notifications(
                memory,
                mmio_dispatcher,
                VirtioPmemFlushStatus::Success,
            )
            .expect("pmem notification dispatch result should allocate")
    }

    fn dispatch_boot_network_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootNetworkNotificationDispatches {
        runtime
            .dispatch_network_queue_notifications(memory, mmio_dispatcher)
            .expect("network notification dispatch result should allocate")
    }

    fn dispatch_boot_network_notifications_with_packet_io(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        provider: &mut impl Arm64BootNetworkPacketIoProvider,
    ) -> Arm64BootNetworkNotificationDispatches {
        dispatch_network_runtime_notifications_with_packet_io(
            memory,
            runtime,
            mmio_dispatcher,
            provider,
        )
        .expect("network packet I/O notification dispatch result should allocate")
    }

    fn dispatch_boot_vsock_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootVsockNotificationDispatches {
        runtime
            .dispatch_vsock_queue_notifications(memory, mmio_dispatcher)
            .expect("vsock notification dispatch result should allocate")
    }

    fn dispatch_boot_balloon_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootBalloonNotificationDispatches {
        runtime
            .dispatch_balloon_queue_notifications(memory, mmio_dispatcher)
            .expect("balloon notification dispatch result should allocate")
    }

    fn dispatch_boot_memory_hotplug_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) -> Arm64BootMemoryHotplugNotificationDispatches {
        runtime
            .dispatch_memory_hotplug_queue_notifications(memory, mmio_dispatcher)
            .expect("memory-hotplug notification dispatch result should allocate")
    }

    fn dispatch_boot_memory_hotplug_notifications_with_executor(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        mutation_executor: &mut impl VirtioMemMutationExecutor,
    ) -> Arm64BootMemoryHotplugNotificationDispatches {
        dispatch_memory_hotplug_runtime_notifications_with_executor(
            memory,
            runtime,
            mmio_dispatcher,
            mutation_executor,
        )
        .expect("memory-hotplug notification dispatch result should allocate")
    }

    fn dispatch_boot_entropy_notifications(
        memory: &mut GuestMemory,
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        provider: &mut impl Arm64BootEntropySourceProvider,
    ) -> Arm64BootEntropyNotificationDispatches {
        runtime
            .dispatch_entropy_queue_notifications_with_source(memory, mmio_dispatcher, provider)
            .expect("entropy notification dispatch result should allocate")
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

    fn write_boot_pmem_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime.pmem_mmio_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("pmem MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("pmem MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_pmem_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime.pmem_mmio_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("pmem MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("pmem MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_network_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime.network_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("network MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("network MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_network_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime.network_devices[device_index]
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("network MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("network MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_vsock_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .vsock_device
            .as_ref()
            .expect("vsock device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("vsock MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("vsock MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_vsock_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .vsock_device
            .as_ref()
            .expect("vsock device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("vsock MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("vsock MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_balloon_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("balloon MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("balloon MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_balloon_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("balloon MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("balloon MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_memory_hotplug_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .memory_hotplug_device
            .as_ref()
            .expect("memory-hotplug device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("memory-hotplug MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("memory-hotplug MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_memory_hotplug_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .memory_hotplug_device
            .as_ref()
            .expect("memory-hotplug device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("memory-hotplug MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("memory-hotplug MMIO read should dispatch");

        match outcome {
            MmioDispatchOutcome::Read { data } => u32::from_le_bytes(
                data.as_slice()
                    .try_into()
                    .expect("u32 read should return four bytes"),
            ),
            MmioDispatchOutcome::Write => panic!("read operation should not return write outcome"),
        }
    }

    fn write_boot_entropy_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
        value: u32,
    ) {
        let address = runtime
            .entropy_device
            .as_ref()
            .expect("entropy device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("entropy MMIO access should resolve");
        let data = MmioAccessBytes::new(&value.to_le_bytes()).expect("u32 bytes should be valid");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::write(access, data).expect("u32 write should be valid"))
            .expect("entropy MMIO write should dispatch");

        assert_eq!(outcome, MmioDispatchOutcome::Write);
    }

    fn read_boot_entropy_mmio_u32(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        register: VirtioMmioRegister,
    ) -> u32 {
        let address = runtime
            .entropy_device
            .as_ref()
            .expect("entropy device should exist")
            .registration
            .address()
            .checked_add(register.offset())
            .expect("test MMIO address should not overflow");
        let access = mmio_dispatcher
            .lookup(address, 4)
            .expect("entropy MMIO access should resolve");
        let outcome = mmio_dispatcher
            .dispatch(MmioOperation::read(access).expect("u32 read should be valid"))
            .expect("entropy MMIO read should dispatch");

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

    fn configure_boot_pmem_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        device_ring: GuestAddress,
    ) {
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(TEST_DESCRIPTOR_TABLE),
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(TEST_AVAILABLE_RING),
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueReady,
            1,
        );
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_pmem_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        write_boot_pmem_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNotify,
            0,
        );
    }

    fn configure_boot_entropy_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_ring: GuestAddress,
    ) {
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(TEST_DESCRIPTOR_TABLE),
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(TEST_AVAILABLE_RING),
        );
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_entropy_mmio_u32(runtime, mmio_dispatcher, VirtioMmioRegister::QueueReady, 1);
        write_boot_entropy_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_entropy_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_entropy_mmio_u32(runtime, mmio_dispatcher, VirtioMmioRegister::QueueNotify, 0);
    }

    fn configure_boot_network_queues(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        configure_boot_network_queue(
            runtime,
            mmio_dispatcher,
            device_index,
            VIRTIO_NET_RX_QUEUE_INDEX,
        );
        configure_boot_network_queue(
            runtime,
            mmio_dispatcher,
            device_index,
            VIRTIO_NET_TX_QUEUE_INDEX,
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn configure_boot_network_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
        queue_index: usize,
    ) {
        let queue_index_u32 = u32::try_from(queue_index).expect("test queue index should fit");
        let (descriptor_table, driver_ring, device_ring) = network_queue_addresses(queue_index_u32);
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueSel,
            queue_index_u32,
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(descriptor_table),
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(driver_ring),
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueReady,
            1,
        );
    }

    fn notify_boot_network_tx_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        device_index: usize,
    ) {
        write_boot_network_mmio_u32(
            runtime,
            mmio_dispatcher,
            device_index,
            VirtioMmioRegister::QueueNotify,
            VIRTIO_NET_TX_QUEUE_INDEX
                .try_into()
                .expect("TX queue index should fit"),
        );
    }

    fn configure_boot_vsock_queues(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        configure_boot_vsock_queue(runtime, mmio_dispatcher, VIRTIO_VSOCK_RX_QUEUE_INDEX);
        configure_boot_vsock_queue(runtime, mmio_dispatcher, VIRTIO_VSOCK_TX_QUEUE_INDEX);
        configure_boot_vsock_queue(runtime, mmio_dispatcher, VIRTIO_VSOCK_EVENT_QUEUE_INDEX);
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn configure_boot_vsock_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        let queue_index_u32 = u32::try_from(queue_index).expect("test queue index should fit");
        let (descriptor_table, driver_ring, device_ring) = vsock_queue_addresses(queue_index_u32);
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueSel,
            queue_index_u32,
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(descriptor_table),
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(driver_ring),
        );
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_vsock_mmio_u32(runtime, mmio_dispatcher, VirtioMmioRegister::QueueReady, 1);
    }

    fn notify_boot_vsock_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        write_boot_vsock_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNotify,
            queue_index.try_into().expect("queue index should fit"),
        );
    }

    fn configure_boot_balloon_queues(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        configure_boot_balloon_queue(runtime, mmio_dispatcher, VIRTIO_BALLOON_INFLATE_QUEUE_INDEX);
        configure_boot_balloon_queue(runtime, mmio_dispatcher, VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX);
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn configure_boot_balloon_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        let queue_index_u32 = u32::try_from(queue_index).expect("test queue index should fit");
        let (descriptor_table, driver_ring, device_ring) = balloon_queue_addresses(queue_index_u32);
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueSel,
            queue_index_u32,
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(descriptor_table),
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(driver_ring),
        );
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(device_ring),
        );
        write_boot_balloon_mmio_u32(runtime, mmio_dispatcher, VirtioMmioRegister::QueueReady, 1);
    }

    fn notify_boot_balloon_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        queue_index: usize,
    ) {
        write_boot_balloon_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNotify,
            queue_index.try_into().expect("queue index should fit"),
        );
    }

    fn configure_boot_memory_hotplug_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            QUEUE_CONFIG_STATUS,
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNum,
            u32::from(TEST_QUEUE_SIZE),
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDescLow,
            guest_address_low(TEST_DESCRIPTOR_TABLE),
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDriverLow,
            guest_address_low(TEST_AVAILABLE_RING),
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueDeviceLow,
            guest_address_low(TEST_USED_RING),
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueReady,
            1,
        );
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::Status,
            DRIVER_OK_STATUS,
        );
    }

    fn notify_boot_memory_hotplug_queue(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
    ) {
        write_boot_memory_hotplug_mmio_u32(
            runtime,
            mmio_dispatcher,
            VirtioMmioRegister::QueueNotify,
            0,
        );
    }

    fn update_boot_memory_hotplug_requested_size(
        runtime: &mut Arm64BootRuntimeResources,
        mmio_dispatcher: &mut MmioDispatcher,
        requested_size_mib: u64,
    ) {
        let config = MemoryHotplugConfig::try_from(MemoryHotplugConfigInput::new(1024, 2, 128))
            .expect("test memory-hotplug config should be valid");
        let update = config
            .validate_size_update(MemoryHotplugSizeUpdateInput::new(requested_size_mib))
            .expect("test memory-hotplug requested size should be valid");
        let device = runtime
            .memory_hotplug_device
            .as_ref()
            .expect("test memory-hotplug device should exist");
        update_memory_hotplug_config_for_device(device, mmio_dispatcher, update)
            .expect("test memory-hotplug requested size update should succeed");
    }

    fn network_queue_addresses(queue_index: u32) -> (GuestAddress, GuestAddress, GuestAddress) {
        match queue_index {
            0 => (
                TEST_NETWORK_RX_DESCRIPTOR_TABLE,
                TEST_NETWORK_RX_AVAILABLE_RING,
                TEST_NETWORK_RX_USED_RING,
            ),
            1 => (
                TEST_NETWORK_TX_DESCRIPTOR_TABLE,
                TEST_NETWORK_TX_AVAILABLE_RING,
                TEST_NETWORK_TX_USED_RING,
            ),
            other => panic!("unsupported test network queue index {other}"),
        }
    }

    fn vsock_queue_addresses(queue_index: u32) -> (GuestAddress, GuestAddress, GuestAddress) {
        match usize::try_from(queue_index).expect("queue index should fit") {
            VIRTIO_VSOCK_RX_QUEUE_INDEX => (
                TEST_VSOCK_RX_DESCRIPTOR_TABLE,
                TEST_VSOCK_RX_AVAILABLE_RING,
                TEST_VSOCK_RX_USED_RING,
            ),
            VIRTIO_VSOCK_TX_QUEUE_INDEX => (
                TEST_VSOCK_TX_DESCRIPTOR_TABLE,
                TEST_VSOCK_TX_AVAILABLE_RING,
                TEST_VSOCK_TX_USED_RING,
            ),
            VIRTIO_VSOCK_EVENT_QUEUE_INDEX => (
                TEST_VSOCK_EVENT_DESCRIPTOR_TABLE,
                TEST_VSOCK_EVENT_AVAILABLE_RING,
                TEST_VSOCK_EVENT_USED_RING,
            ),
            other => panic!("unsupported test vsock queue index {other}"),
        }
    }

    fn balloon_queue_addresses(queue_index: u32) -> (GuestAddress, GuestAddress, GuestAddress) {
        match usize::try_from(queue_index).expect("queue index should fit") {
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX => (
                TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE,
                TEST_BALLOON_INFLATE_AVAILABLE_RING,
                TEST_BALLOON_INFLATE_USED_RING,
            ),
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX => (
                TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE,
                TEST_BALLOON_DEFLATE_AVAILABLE_RING,
                TEST_BALLOON_DEFLATE_USED_RING,
            ),
            other => panic!("unsupported test balloon queue index {other}"),
        }
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

    fn write_queued_pmem_flush_request(memory: &mut GuestMemory) {
        memory
            .write_slice(&VIRTIO_PMEM_REQUEST_TYPE_FLUSH.to_le_bytes(), HEADER_ADDR)
            .expect("pmem request type should write");
        write_descriptor(
            memory,
            0,
            TestDescriptor::readable(HEADER_ADDR, VIRTIO_PMEM_REQUEST_SIZE, Some(1)),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(STATUS_ADDR, VIRTIO_PMEM_STATUS_SIZE, None),
        );
        write_available_heads(memory, &[0]);
    }

    fn write_partially_invalid_queued_pmem_flush_request(memory: &mut GuestMemory) {
        memory
            .write_slice(&VIRTIO_PMEM_REQUEST_TYPE_FLUSH.to_le_bytes(), HEADER_ADDR)
            .expect("pmem request type should write");
        write_descriptor(
            memory,
            0,
            TestDescriptor::writable(HEADER_ADDR, VIRTIO_PMEM_REQUEST_SIZE, Some(1)),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(STATUS_ADDR, VIRTIO_PMEM_STATUS_SIZE, None),
        );
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

    fn write_entropy_request(memory: &mut GuestMemory, len: u32) {
        write_descriptor(memory, 0, TestDescriptor::writable(DATA_ADDR, len, None));
        write_available_heads(memory, &[0]);
    }

    fn write_partially_invalid_entropy_request(memory: &mut GuestMemory) {
        write_descriptor(memory, 0, TestDescriptor::writable(DATA_ADDR, 16, None));
        write_available_heads(memory, &[0, TEST_QUEUE_SIZE]);
    }

    fn write_queued_balloon_inflate_request(memory: &mut GuestMemory) {
        memory
            .write_slice(
                &TEST_BALLOON_MAPPED_PFN.to_le_bytes(),
                TEST_BALLOON_PFN_PAYLOAD,
            )
            .expect("balloon PFN payload should write");
        write_balloon_inflate_descriptor(
            memory,
            0,
            TestDescriptor::readable(TEST_BALLOON_PFN_PAYLOAD, 4, None),
        );
        write_balloon_inflate_available_heads(memory, &[0]);
    }

    fn write_partially_invalid_balloon_inflate_request(memory: &mut GuestMemory) {
        memory
            .write_slice(
                &TEST_BALLOON_MAPPED_PFN.to_le_bytes(),
                TEST_BALLOON_PFN_PAYLOAD,
            )
            .expect("balloon PFN payload should write");
        write_balloon_inflate_descriptor(
            memory,
            0,
            TestDescriptor::readable(TEST_BALLOON_PFN_PAYLOAD, 4, None),
        );
        write_balloon_inflate_available_heads(memory, &[0, TEST_QUEUE_SIZE]);
    }

    fn write_queued_balloon_deflate_request(memory: &mut GuestMemory) {
        memory
            .write_slice(
                &TEST_BALLOON_MAPPED_PFN.to_le_bytes(),
                TEST_BALLOON_PFN_PAYLOAD,
            )
            .expect("balloon PFN payload should write");
        write_balloon_deflate_descriptor(
            memory,
            0,
            TestDescriptor::readable(TEST_BALLOON_PFN_PAYLOAD, 4, None),
        );
        write_balloon_deflate_available_heads(memory, &[0]);
    }

    fn write_queued_memory_hotplug_request(
        memory: &mut GuestMemory,
        request_type: u16,
        address: GuestAddress,
        block_count: u16,
    ) {
        let mut request = Vec::new();
        request.extend_from_slice(&request_type.to_le_bytes());
        request.extend_from_slice(&[0; 6]);
        request.extend_from_slice(&address.raw_value().to_le_bytes());
        request.extend_from_slice(&block_count.to_le_bytes());
        request.extend_from_slice(&[0; 6]);
        assert_eq!(request.len(), VIRTIO_MEM_REQUEST_SIZE);
        memory
            .write_slice(&request, HEADER_ADDR)
            .expect("virtio-mem request should write");
        write_descriptor(
            memory,
            0,
            TestDescriptor::readable(
                HEADER_ADDR,
                u32::try_from(VIRTIO_MEM_REQUEST_SIZE).expect("request size should fit"),
                Some(1),
            ),
        );
        write_descriptor(
            memory,
            1,
            TestDescriptor::writable(
                DATA_ADDR,
                u32::try_from(VIRTIO_MEM_RESPONSE_SIZE).expect("response size should fit"),
                None,
            ),
        );
        write_available_heads(memory, &[0]);
    }

    fn write_queued_memory_hotplug_state_request(memory: &mut GuestMemory) {
        write_queued_memory_hotplug_request(
            memory,
            TEST_VIRTIO_MEM_REQ_STATE,
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS,
            1,
        );
    }

    fn write_queued_memory_hotplug_plug_request(memory: &mut GuestMemory) {
        write_queued_memory_hotplug_request(
            memory,
            TEST_VIRTIO_MEM_REQ_PLUG,
            VIRTIO_MEM_DEFAULT_REGION_ADDRESS,
            1,
        );
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

    fn write_balloon_inflate_descriptor(
        memory: &mut GuestMemory,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_balloon_descriptor_at(
            memory,
            TEST_BALLOON_INFLATE_DESCRIPTOR_TABLE,
            index,
            descriptor,
        );
    }

    fn write_balloon_deflate_descriptor(
        memory: &mut GuestMemory,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        write_balloon_descriptor_at(
            memory,
            TEST_BALLOON_DEFLATE_DESCRIPTOR_TABLE,
            index,
            descriptor,
        );
    }

    fn write_balloon_descriptor_at(
        memory: &mut GuestMemory,
        descriptor_table: GuestAddress,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = descriptor_table
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("balloon descriptor should write");
    }

    fn write_network_tx_header(memory: &mut GuestMemory) {
        let mut bytes = [0; VIRTIO_NET_TX_HEADER_SIZE as usize];
        let (flags, tail) = bytes.split_at_mut(1);
        let (gso_type, tail) = tail.split_at_mut(1);
        let (header_len, tail) = tail.split_at_mut(2);
        let (gso_size, tail) = tail.split_at_mut(2);
        let (checksum_start, tail) = tail.split_at_mut(2);
        let (checksum_offset, num_buffers) = tail.split_at_mut(2);

        flags.copy_from_slice(&[0x1]);
        gso_type.copy_from_slice(&[0x2]);
        header_len.copy_from_slice(&0x0304_u16.to_le_bytes());
        gso_size.copy_from_slice(&0x0506_u16.to_le_bytes());
        checksum_start.copy_from_slice(&0x0708_u16.to_le_bytes());
        checksum_offset.copy_from_slice(&0x090a_u16.to_le_bytes());
        num_buffers.copy_from_slice(&0x0b0c_u16.to_le_bytes());

        memory
            .write_slice(&bytes, TEST_NETWORK_TX_HEADER)
            .expect("virtio-net TX header should write");
    }

    fn write_network_tx_descriptor(
        memory: &mut GuestMemory,
        index: u16,
        descriptor: TestDescriptor,
    ) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_NETWORK_TX_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("network TX descriptor should write");
    }

    fn write_network_tx_descriptors(memory: &mut GuestMemory, descriptors: &[TestDescriptor]) {
        for (index, descriptor) in descriptors.iter().copied().enumerate() {
            write_network_tx_descriptor(
                memory,
                u16::try_from(index).expect("test descriptor index should fit"),
                descriptor,
            );
        }
    }

    fn write_vsock_tx_packet_header(memory: &mut GuestMemory) {
        memory
            .write_slice(
                &VirtioVsockPacketHeader::new().to_bytes(),
                TEST_VSOCK_HEADER,
            )
            .expect("virtio-vsock TX header should write");
    }

    fn write_vsock_tx_descriptor(memory: &mut GuestMemory, index: u16, descriptor: TestDescriptor) {
        let mut bytes = [0; VIRTQUEUE_DESCRIPTOR_SIZE];
        let (address_bytes, tail) = bytes.split_at_mut(8);
        let (len_bytes, tail) = tail.split_at_mut(4);
        let (flags_bytes, next_bytes) = tail.split_at_mut(2);
        address_bytes.copy_from_slice(&descriptor.address.raw_value().to_le_bytes());
        len_bytes.copy_from_slice(&descriptor.len.to_le_bytes());
        flags_bytes.copy_from_slice(&descriptor.flags.to_le_bytes());
        next_bytes.copy_from_slice(&descriptor.next.to_le_bytes());

        let descriptor_address = TEST_VSOCK_TX_DESCRIPTOR_TABLE
            .checked_add(
                u64::from(index)
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE).expect("descriptor size should fit"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&bytes, descriptor_address)
            .expect("vsock TX descriptor should write");
    }

    fn write_vsock_tx_descriptors(memory: &mut GuestMemory, descriptors: &[TestDescriptor]) {
        for (index, descriptor) in descriptors.iter().copied().enumerate() {
            write_vsock_tx_descriptor(
                memory,
                u16::try_from(index).expect("test descriptor index should fit"),
                descriptor,
            );
        }
    }

    fn write_guest_u16(memory: &mut GuestMemory, address: GuestAddress, value: u16) {
        memory
            .write_slice(&value.to_le_bytes(), address)
            .expect("u16 field should write");
    }

    fn read_guest_u16(memory: &GuestMemory, address: GuestAddress) -> u16 {
        let mut bytes = [0; 2];
        memory
            .read_slice(&mut bytes, address)
            .expect("u16 field should read");
        u16::from_le_bytes(bytes)
    }

    fn read_guest_u32(memory: &GuestMemory, address: GuestAddress) -> u32 {
        let mut bytes = [0; 4];
        memory
            .read_slice(&mut bytes, address)
            .expect("u32 field should read");
        u32::from_le_bytes(bytes)
    }

    fn read_guest_bytes(memory: &GuestMemory, address: GuestAddress, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        memory
            .read_slice(&mut bytes, address)
            .expect("guest bytes should read");
        bytes
    }

    fn read_entropy_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_USED_RING
                .checked_add(2)
                .expect("entropy used idx address should not overflow"),
        )
    }

    fn read_memory_hotplug_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_USED_RING
                .checked_add(2)
                .expect("memory-hotplug used idx address should not overflow"),
        )
    }

    fn read_pmem_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_USED_RING
                .checked_add(2)
                .expect("pmem used idx address should not overflow"),
        )
    }

    fn read_pmem_used_element(memory: &GuestMemory, ring_index: u16) -> (u32, u32) {
        let element = TEST_USED_RING
            .checked_add(4 + u64::from(ring_index) * 8)
            .expect("pmem used element address should not overflow");
        (
            read_guest_u32(memory, element),
            read_guest_u32(
                memory,
                element
                    .checked_add(4)
                    .expect("pmem used element len address should not overflow"),
            ),
        )
    }

    fn read_entropy_used_element(memory: &GuestMemory, ring_index: u16) -> (u32, u32) {
        let element = TEST_USED_RING
            .checked_add(4 + u64::from(ring_index) * 8)
            .expect("entropy used element address should not overflow");
        (
            read_guest_u32(memory, element),
            read_guest_u32(
                memory,
                element
                    .checked_add(4)
                    .expect("entropy used len address should not overflow"),
            ),
        )
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

    fn network_tx_available_ring_idx_address() -> GuestAddress {
        TEST_NETWORK_TX_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("network TX available idx address should not overflow")
    }

    fn network_tx_available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_NETWORK_TX_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("network TX available entry address should not overflow")
    }

    fn write_network_tx_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                network_tx_available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            network_tx_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn network_tx_used_ring_idx_address() -> GuestAddress {
        TEST_NETWORK_TX_USED_RING
            .checked_add(2)
            .expect("network TX used idx address should not overflow")
    }

    fn network_tx_used_ring_entry_address(index: usize) -> GuestAddress {
        TEST_NETWORK_TX_USED_RING
            .checked_add(4 + u64::try_from(index).expect("test index should fit") * 8)
            .expect("network TX used entry address should not overflow")
    }

    fn read_network_tx_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, network_tx_used_ring_idx_address())
    }

    fn read_network_tx_used_element(memory: &GuestMemory, index: usize) -> (u32, u32) {
        let address = network_tx_used_ring_entry_address(index);
        let descriptor_head = read_guest_u32(memory, address);
        let len = read_guest_u32(
            memory,
            address
                .checked_add(4)
                .expect("network TX used-ring len address should not overflow"),
        );
        (descriptor_head, len)
    }

    fn vsock_tx_available_ring_idx_address() -> GuestAddress {
        TEST_VSOCK_TX_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("vsock TX available idx address should not overflow")
    }

    fn vsock_tx_available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_VSOCK_TX_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("vsock TX available entry address should not overflow")
    }

    fn write_vsock_tx_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                vsock_tx_available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            vsock_tx_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn vsock_tx_used_ring_idx_address() -> GuestAddress {
        TEST_VSOCK_TX_USED_RING
            .checked_add(2)
            .expect("vsock TX used idx address should not overflow")
    }

    fn read_vsock_tx_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(memory, vsock_tx_used_ring_idx_address())
    }

    fn balloon_inflate_available_ring_idx_address() -> GuestAddress {
        TEST_BALLOON_INFLATE_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("balloon inflate available idx address should not overflow")
    }

    fn balloon_inflate_available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_BALLOON_INFLATE_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("balloon inflate available entry address should not overflow")
    }

    fn write_balloon_inflate_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                balloon_inflate_available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            balloon_inflate_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn balloon_deflate_available_ring_idx_address() -> GuestAddress {
        TEST_BALLOON_DEFLATE_AVAILABLE_RING
            .checked_add(TEST_AVAILABLE_RING_IDX_OFFSET)
            .expect("balloon deflate available idx address should not overflow")
    }

    fn balloon_deflate_available_ring_entry_address(ring_index: u16) -> GuestAddress {
        TEST_BALLOON_DEFLATE_AVAILABLE_RING
            .checked_add(
                TEST_AVAILABLE_RING_RING_OFFSET
                    + u64::from(ring_index) * TEST_AVAILABLE_RING_ENTRY_SIZE,
            )
            .expect("balloon deflate available entry address should not overflow")
    }

    fn write_balloon_deflate_available_heads(memory: &mut GuestMemory, heads: &[u16]) {
        for (ring_index, head) in heads.iter().copied().enumerate() {
            write_guest_u16(
                memory,
                balloon_deflate_available_ring_entry_address(
                    u16::try_from(ring_index).expect("test ring index should fit in u16"),
                ),
                head,
            );
        }
        write_guest_u16(
            memory,
            balloon_deflate_available_ring_idx_address(),
            u16::try_from(heads.len()).expect("test available length should fit in u16"),
        );
    }

    fn read_balloon_inflate_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_BALLOON_INFLATE_USED_RING
                .checked_add(2)
                .expect("balloon inflate used idx address should not overflow"),
        )
    }

    fn read_balloon_deflate_used_index(memory: &GuestMemory) -> u16 {
        read_guest_u16(
            memory,
            TEST_BALLOON_DEFLATE_USED_RING
                .checked_add(2)
                .expect("balloon deflate used idx address should not overflow"),
        )
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
    fn block_metrics_record_successful_signal_dispatch() {
        let payload = vec![0x74; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_drives(&[
            ("rootfs", payload.as_slice(), true),
            ("data", payload.as_slice(), false),
        ]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 1);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (_, sink) = RecordingSink::successful();
        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued dispatch should collect");
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs", "data"]);

        super::record_block_dispatch_metrics(&metrics, &result);

        let aggregate = metrics.aggregate_snapshot();
        let read_agg = aggregate.read_agg();
        assert!(!read_agg.is_empty());
        assert_eq!(
            aggregate,
            BlockDeviceMetrics::default()
                .with_queue_event_count(1)
                .with_read_bytes(VIRTIO_BLOCK_SECTOR_SIZE)
                .with_read_count(1)
                .with_read_agg(read_agg)
        );
        assert_eq!(
            metrics.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new().with_drive_metrics(
                "data",
                BlockDeviceMetrics::default()
                    .with_queue_event_count(1)
                    .with_read_bytes(VIRTIO_BLOCK_SECTOR_SIZE)
                    .with_read_count(1)
                    .with_read_agg(read_agg),
            )
        );
    }

    #[test]
    fn block_metrics_preserve_partial_dispatch_before_signal() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_partially_invalid_queued_flush_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (_, sink) = RecordingSink::successful();
        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial dispatch result should collect");
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs"]);

        super::record_block_dispatch_metrics(&metrics, &result);

        assert_eq!(
            metrics.aggregate_snapshot(),
            BlockDeviceMetrics::default()
                .with_event_fails(1)
                .with_flush_count(1)
                .with_queue_event_count(1)
        );
        assert_eq!(
            metrics.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new().with_drive_metrics(
                "rootfs",
                BlockDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_flush_count(1)
                    .with_queue_event_count(1),
            )
        );
    }

    #[test]
    fn block_metrics_record_signal_failure() {
        let payload = vec![0x74; VIRTIO_BLOCK_SECTOR_SIZE as usize];
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_drives(&[("rootfs", payload.as_slice(), true)]);
        configure_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_queued_read_request(&mut memory);
        notify_boot_block_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (_, sink) = RecordingSink::failing("injected signal failure");
        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("signal failure should stay per-device");
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs"]);

        super::record_block_dispatch_metrics(&metrics, &result);

        let aggregate = metrics.aggregate_snapshot();
        let read_agg = aggregate.read_agg();
        assert!(!read_agg.is_empty());
        assert_eq!(
            aggregate,
            BlockDeviceMetrics::default()
                .with_event_fails(1)
                .with_queue_event_count(1)
                .with_read_bytes(VIRTIO_BLOCK_SECTOR_SIZE)
                .with_read_count(1)
                .with_read_agg(read_agg)
        );
        assert_eq!(
            metrics.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new().with_drive_metrics(
                "rootfs",
                BlockDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_queue_event_count(1)
                    .with_read_bytes(VIRTIO_BLOCK_SECTOR_SIZE)
                    .with_read_count(1)
                    .with_read_agg(read_agg),
            )
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
    fn block_metrics_record_handler_lookup_failure() {
        let (mut memory, mut runtime, _) =
            boot_runtime_with_drives(&[("rootfs", &[0x5a; 512], true)]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_block_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (_, sink) = RecordingSink::successful();
        let result = signal_block_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing handler dispatch should collect");
        let metrics = SharedBlockDeviceMetricsRegistry::from_drive_ids(["rootfs"]);

        super::record_block_dispatch_metrics(&metrics, &result);

        assert_eq!(
            metrics.aggregate_snapshot(),
            BlockDeviceMetrics::default().with_event_fails(1)
        );
        assert_eq!(
            metrics.per_drive_snapshot(),
            BlockDeviceMetricsByDrive::new()
                .with_drive_metrics("rootfs", BlockDeviceMetrics::default().with_event_fails(1),)
        );
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
    fn pmem_metrics_record_successful_signal_dispatch() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_pmem(&["pmem0", "pmem1"]);
        configure_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 1, TEST_USED_RING);
        write_queued_pmem_flush_request(&mut memory);
        notify_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 1);
        let dispatches =
            dispatch_boot_pmem_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();
        let result = signal_pmem_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued pmem dispatch should collect");
        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0", "pmem1"]);

        record_pmem_dispatch_metrics(&metrics, &result);

        assert_eq!(recorded_lines(&lines), vec![33]);
        assert_eq!(
            read_boot_pmem_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                1,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_pmem_used_index(&memory), 1);
        assert_eq!(
            read_pmem_used_element(&memory, 0),
            (0, VIRTIO_PMEM_STATUS_SIZE)
        );
        assert_eq!(
            metrics.aggregate_snapshot(),
            PmemDeviceMetrics::default().with_queue_event_count(1)
        );
        assert_eq!(
            metrics.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new().with_device_metrics(
                "pmem1",
                PmemDeviceMetrics::default().with_queue_event_count(1),
            )
        );
    }

    #[test]
    fn pmem_metrics_record_parse_failure_before_signal() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_pmem(&["pmem0"]);
        configure_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_partially_invalid_queued_pmem_flush_request(&mut memory);
        notify_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_pmem_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();
        let result = signal_pmem_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial pmem dispatch result should collect");
        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0"]);

        record_pmem_dispatch_metrics(&metrics, &result);

        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(read_pmem_used_index(&memory), 1);
        assert_eq!(read_pmem_used_element(&memory, 0), (0, 0));
        assert_eq!(
            metrics.aggregate_snapshot(),
            PmemDeviceMetrics::default()
                .with_event_fails(1)
                .with_queue_event_count(1)
        );
        assert_eq!(
            metrics.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new().with_device_metrics(
                "pmem0",
                PmemDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_queue_event_count(1),
            )
        );
    }

    #[test]
    fn pmem_metrics_record_signal_failure() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_pmem(&["pmem0"]);
        configure_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0, TEST_USED_RING);
        write_queued_pmem_flush_request(&mut memory);
        notify_boot_pmem_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_pmem_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::failing("injected pmem signal failure");
        let result = signal_pmem_queue_interrupts(dispatches, sink.as_ref())
            .expect("pmem signal failure should stay per-device");
        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0"]);

        record_pmem_dispatch_metrics(&metrics, &result);

        assert_eq!(recorded_lines(&lines), vec![32]);
        assert!(result.has_signal_failure());
        assert_eq!(
            metrics.aggregate_snapshot(),
            PmemDeviceMetrics::default()
                .with_event_fails(1)
                .with_queue_event_count(1)
        );
        assert_eq!(
            metrics.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new().with_device_metrics(
                "pmem0",
                PmemDeviceMetrics::default()
                    .with_event_fails(1)
                    .with_queue_event_count(1),
            )
        );
    }

    #[test]
    fn pmem_metrics_record_handler_lookup_failure() {
        let (mut memory, mut runtime, _) = boot_runtime_with_pmem(&["pmem0"]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_pmem_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result =
            signal_pmem_queue_interrupts(dispatches, RecordingSink::successful().1.as_ref())
                .expect("missing pmem handler dispatch should collect");
        let metrics = SharedPmemDeviceMetricsRegistry::from_device_ids(["pmem0"]);

        record_pmem_dispatch_metrics(&metrics, &result);

        assert_eq!(
            metrics.aggregate_snapshot(),
            PmemDeviceMetrics::default().with_event_fails(1)
        );
        assert_eq!(
            metrics.per_device_snapshot(),
            PmemDeviceMetricsByDevice::new()
                .with_device_metrics("pmem0", PmemDeviceMetrics::default().with_event_fails(1),)
        );
    }

    #[test]
    fn entropy_notification_signal_dispatch_accepts_empty_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let result = collect_entropy_notification_dispatches(dispatches)
            .expect("empty entropy dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
        assert!(provider.requested_regions.is_empty());
    }

    #[test]
    fn entropy_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop entropy dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(provider.requested_regions.is_empty());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn entropy_notification_signal_dispatch_signals_queued_request() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_entropy_request(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued entropy dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(provider.requested_regions, [MmioRegionId::new(100)]);
        assert_eq!(provider.source.calls, [16]);
        assert_eq!(
            read_guest_bytes(&memory, DATA_ADDR, 16),
            (0_u8..16).collect::<Vec<_>>()
        );
        assert_eq!(read_entropy_used_index(&memory), 1);
        assert_eq!(read_entropy_used_element(&memory, 0), (0, 16));
        assert_eq!(
            read_boot_entropy_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn entropy_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_partially_invalid_entropy_request(&mut memory);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial entropy dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.dispatch().outcome().dispatch_error().is_some());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(provider.source.calls, [16]);
        assert_eq!(read_entropy_used_index(&memory), 1);
        assert_eq!(read_entropy_used_element(&memory, 0), (0, 16));
    }

    #[test]
    fn entropy_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_entropy_request(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::failing("injected entropy signal failure");

        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("entropy signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected entropy signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_boot_entropy_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus,
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn entropy_metrics_record_signal_failure() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_entropy();
        configure_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher, TEST_USED_RING);
        write_entropy_request(&mut memory, 16);
        notify_boot_entropy_queue(&mut runtime, &mut mmio_dispatcher);
        let mut provider = RecordingEntropySourceProvider::default();
        let dispatches = dispatch_boot_entropy_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::failing("injected entropy signal failure");
        let result = signal_entropy_queue_interrupts(dispatches, sink.as_ref())
            .expect("entropy signal failure should stay per-device");
        let metrics = SharedEntropyDeviceMetrics::default();

        record_entropy_dispatch_metrics(&metrics, &result);

        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            metrics.snapshot(),
            EntropyDeviceMetrics::default()
                .with_entropy_event_fails(1)
                .with_entropy_event_count(1)
                .with_entropy_bytes(16)
        );
    }

    #[test]
    fn network_notification_signal_dispatch_accepts_empty_devices() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result = collect_network_notification_dispatches(dispatches)
            .expect("empty network dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
    }

    #[test]
    fn network_notification_packet_io_dispatch_accepts_empty_devices() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let mut provider = RecordingNetworkPacketIoProvider::for_iface("eth0");
        let dispatches = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let result = collect_network_notification_dispatches(dispatches)
            .expect("empty network dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
        assert!(provider.requested_ifaces.is_empty());
    }

    #[test]
    fn network_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop network dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(device.dispatch().device().registration.iface_id(), "eth0");
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
    }

    #[test]
    fn network_notification_packet_io_dispatch_skips_provider_without_pending() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        let mut provider = RecordingNetworkPacketIoProvider::for_iface("eth0");
        let dispatches = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop network packet I/O dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(device.dispatch().device().registration.iface_id(), "eth0");
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert!(provider.requested_ifaces.is_empty());
    }

    #[test]
    fn network_notification_signal_dispatch_signals_queued_tx_frame() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        memory
            .write_slice(&[0xde, 0xad, 0xbe, 0xef], TEST_NETWORK_TX_PAYLOAD)
            .expect("network TX payload should write");
        write_network_tx_descriptors(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_NETWORK_TX_HEADER,
                    VIRTIO_NET_TX_HEADER_SIZE,
                    Some(1),
                ),
                TestDescriptor::readable(TEST_NETWORK_TX_PAYLOAD, 4, None),
            ],
        );
        write_network_tx_available_heads(&mut memory, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued network dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("network notification should dispatch");
        let tx = dispatch
            .tx_queue_dispatch()
            .expect("TX queue dispatch should be present");
        assert_eq!(tx.processed_frames(), 1);
        assert_eq!(tx.successful_frames(), 1);
        assert_eq!(tx.parse_failures(), 0);
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_network_tx_used_index(&memory), 1);
        assert_eq!(read_network_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn network_notification_packet_io_dispatch_routes_tx_frame_and_signals() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        memory
            .write_slice(&[0xde, 0xad, 0xbe, 0xef], TEST_NETWORK_TX_PAYLOAD)
            .expect("network TX payload should write");
        write_network_tx_descriptors(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_NETWORK_TX_HEADER,
                    VIRTIO_NET_TX_HEADER_SIZE,
                    Some(1),
                ),
                TestDescriptor::readable(TEST_NETWORK_TX_PAYLOAD, 4, None),
            ],
        );
        write_network_tx_available_heads(&mut memory, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let mut provider = RecordingNetworkPacketIoProvider::for_iface("eth0");
        let dispatches = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued network packet I/O dispatch should collect");

        assert_eq!(provider.requested_ifaces, ["eth0".to_string()]);
        assert_eq!(provider.tx_sink.packets, [vec![0xde, 0xad, 0xbe, 0xef]]);
        assert_eq!(provider.rx_source.peek_calls, 0);
        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("network notification should dispatch");
        let tx = dispatch
            .tx_queue_dispatch()
            .expect("TX queue dispatch should be present");
        assert_eq!(tx.processed_frames(), 1);
        assert_eq!(tx.sink_successful_frames(), 1);
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        let expected = NetworkInterfaceMetrics::default()
            .with_tx_queue_event_count(1)
            .with_tx_bytes_count(16)
            .with_tx_packets_count(1)
            .with_tx_count(1);
        assert_eq!(metrics.aggregate_snapshot(), expected);
        assert_eq!(
            metrics.per_interface_snapshot(),
            NetworkInterfaceMetricsByInterface::new().with_interface_metrics("eth0", expected)
        );
        assert_eq!(read_network_tx_used_index(&memory), 1);
        assert_eq!(read_network_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn network_notification_packet_io_dispatch_preserves_pending_on_provider_failure() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        memory
            .write_slice(&[0xde, 0xad, 0xbe, 0xef], TEST_NETWORK_TX_PAYLOAD)
            .expect("network TX payload should write");
        write_network_tx_descriptors(
            &mut memory,
            &[
                TestDescriptor::readable(
                    TEST_NETWORK_TX_HEADER,
                    VIRTIO_NET_TX_HEADER_SIZE,
                    Some(1),
                ),
                TestDescriptor::readable(TEST_NETWORK_TX_PAYLOAD, 4, None),
            ],
        );
        write_network_tx_available_heads(&mut memory, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let mut provider = RecordingNetworkPacketIoProvider::failing_for("eth0");
        let failed = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );

        assert_eq!(provider.requested_ifaces, ["eth0".to_string()]);
        assert!(!failed.needs_queue_interrupt());
        match failed.as_slice()[0].outcome() {
            Arm64BootNetworkNotificationOutcome::PacketIoProviderFailed(source) => {
                assert_eq!(
                    source.message(),
                    "test packet I/O unavailable for interface eth0"
                );
            }
            other => panic!("expected packet I/O provider failure, got {other:?}"),
        }
        let (lines, sink) = RecordingSink::successful();
        let result = signal_network_queue_interrupts(failed, sink.as_ref())
            .expect("failed network packet I/O dispatch should collect");
        assert!(!result.has_signal_failure());
        assert!(recorded_lines(&lines).is_empty());
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.aggregate_snapshot(),
            NetworkInterfaceMetrics::default().with_event_fails(1)
        );
        assert_eq!(
            metrics.per_interface_snapshot(),
            NetworkInterfaceMetricsByInterface::new().with_interface_metrics(
                "eth0",
                NetworkInterfaceMetrics::default().with_event_fails(1),
            )
        );

        let retried =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        assert!(retried.needs_queue_interrupt());
        let dispatch = retried.as_slice()[0]
            .outcome()
            .dispatched()
            .expect("retry should dispatch preserved notification");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_NET_TX_QUEUE_INDEX]
        );
        assert_eq!(
            dispatch
                .tx_queue_dispatch()
                .expect("TX queue dispatch should be present")
                .processed_frames(),
            1
        );
    }

    #[test]
    fn network_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        write_network_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_NETWORK_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 4,
                None,
            )],
        );
        write_network_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial network dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        let err = device
            .dispatch()
            .outcome()
            .dispatch_error()
            .expect("partial TX dispatch error should be preserved");
        let completed = err
            .completed_tx_dispatch()
            .expect("completed TX metadata should be preserved");
        assert_eq!(completed.processed_frames(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.aggregate_snapshot(),
            NetworkInterfaceMetrics::default()
                .with_event_fails(1)
                .with_tx_queue_event_count(1)
                .with_tx_bytes_count(16)
                .with_tx_packets_count(1)
                .with_tx_count(1)
        );
        assert_eq!(read_network_tx_used_index(&memory), 1);
        assert_eq!(read_network_tx_used_element(&memory, 0), (0, 0));
    }

    #[test]
    fn network_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) =
            boot_runtime_with_networks(&[("eth0", "tap0")]);
        configure_boot_network_queues(&mut runtime, &mut mmio_dispatcher, 0);
        write_network_tx_header(&mut memory);
        write_network_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_NETWORK_TX_HEADER,
                VIRTIO_NET_TX_HEADER_SIZE + 4,
                None,
            )],
        );
        write_network_tx_available_heads(&mut memory, &[0]);
        notify_boot_network_tx_queue(&mut runtime, &mut mmio_dispatcher, 0);
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::failing("injected network signal failure");

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("network signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("network signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected network signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.aggregate_snapshot(),
            NetworkInterfaceMetrics::default()
                .with_event_fails(1)
                .with_tx_queue_event_count(1)
                .with_tx_bytes_count(16)
                .with_tx_packets_count(1)
                .with_tx_count(1)
        );
        assert_eq!(
            read_boot_network_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                0,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
    }

    #[test]
    fn network_notification_signal_dispatch_preserves_missing_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_networks(&[("eth0", "tap0")]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing network handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        let metrics = SharedNetworkInterfaceMetricsRegistry::from_interface_ids(["eth0"]);
        super::record_network_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.aggregate_snapshot(),
            NetworkInterfaceMetrics::default().with_event_fails(1)
        );
    }

    #[test]
    fn network_notification_packet_io_dispatch_skips_provider_when_handler_missing() {
        let (mut memory, mut runtime, _) = boot_runtime_with_networks(&[("eth0", "tap0")]);
        let mut mmio_dispatcher = MmioDispatcher::new();
        let mut provider = RecordingNetworkPacketIoProvider::for_iface("eth0");
        let dispatches = dispatch_boot_network_notifications_with_packet_io(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut provider,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing network handler packet I/O dispatch should collect");

        assert!(provider.requested_ifaces.is_empty());
        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn network_notification_signal_dispatch_preserves_wrong_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_networks(&[("eth0", "tap0")]);
        let region = runtime.network_devices[0].registration.region();
        let mut mmio_dispatcher = MmioDispatcher::new();
        mmio_dispatcher
            .insert_region(region.id(), region.range().start(), region.range().size())
            .expect("replacement network region should insert");
        mmio_dispatcher
            .register_handler(region.id(), WrongNetworkHandler)
            .expect("wrong network handler should register");
        let dispatches =
            dispatch_boot_network_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_network_queue_interrupts(dispatches, sink.as_ref())
            .expect("wrong network handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn vsock_notification_signal_dispatch_accepts_empty_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result = collect_vsock_notification_dispatches(dispatches)
            .expect("empty vsock dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
    }

    #[test]
    fn vsock_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop vsock dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(device.dispatch().device().registration.guest_cid(), 42);
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(metrics.snapshot(), VsockDeviceMetrics::default());
    }

    #[test]
    fn vsock_notification_signal_dispatch_signals_queued_tx_packet() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        write_vsock_tx_packet_header(&mut memory);
        write_vsock_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued vsock dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("vsock notification should dispatch");
        let tx = dispatch
            .tx_queue_dispatch()
            .expect("TX queue dispatch should be present");
        assert_eq!(tx.processed_packets(), 1);
        assert_eq!(tx.successful_packets(), 1);
        assert_eq!(tx.parse_failures(), 0);
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default()
                .with_tx_queue_event_count(1)
                .with_tx_packets_count(1)
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_skips_rx_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_RX_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("unsupported vsock dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("RX noop should dispatch");
        let rx = dispatch
            .rx_queue_dispatch()
            .expect("RX dispatch summary should be present");
        assert_eq!(rx.processed_buffers(), 0);
        assert_eq!(rx.delivered_requests(), 0);
        assert!(!rx.needs_queue_interrupt());
        assert!(dispatch.tx_queue_dispatch().is_none());
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default().with_rx_queue_event_count(1)
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_preserves_event_noop_without_signal() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_EVENT_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("event no-op vsock dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("event notification should be accepted as no-op dispatch");
        assert_eq!(
            dispatch.drained_notifications(),
            [VIRTIO_VSOCK_EVENT_QUEUE_INDEX]
        );
        assert_eq!(dispatch.event_notifications(), 1);
        assert!(dispatch.rx_queue_dispatch().is_none());
        assert!(dispatch.tx_queue_dispatch().is_none());
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        write_vsock_tx_packet_header(&mut memory);
        write_vsock_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );
        write_vsock_tx_available_heads(&mut memory, &[0, TEST_QUEUE_SIZE]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial vsock dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        let err = device
            .dispatch()
            .outcome()
            .dispatch_error()
            .expect("partial TX dispatch error should be preserved");
        let completed = err
            .completed_tx_dispatch()
            .expect("completed TX metadata should be preserved");
        assert_eq!(completed.processed_packets(), 1);
        assert!(completed.needs_queue_interrupt());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(read_vsock_tx_used_index(&memory), 1);
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default()
                .with_tx_queue_event_fails(1)
                .with_tx_queue_event_count(1)
                .with_tx_packets_count(1)
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_vsock();
        configure_boot_vsock_queues(&mut runtime, &mut mmio_dispatcher);
        write_vsock_tx_packet_header(&mut memory);
        write_vsock_tx_descriptors(
            &mut memory,
            &[TestDescriptor::readable(
                TEST_VSOCK_HEADER,
                VIRTIO_VSOCK_PACKET_HEADER_SIZE as u32,
                None,
            )],
        );
        write_vsock_tx_available_heads(&mut memory, &[0]);
        notify_boot_vsock_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_VSOCK_TX_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::failing("injected vsock signal failure");

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("vsock signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("vsock signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected vsock signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_boot_vsock_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default()
                .with_muxer_event_fails(1)
                .with_tx_queue_event_count(1)
                .with_tx_packets_count(1)
        );
    }

    #[test]
    fn vsock_notification_signal_dispatch_preserves_missing_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_vsock();
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_vsock_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_vsock_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing vsock handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        let metrics = SharedVsockDeviceMetrics::default();
        super::record_vsock_dispatch_metrics(&metrics, &result);
        assert_eq!(
            metrics.snapshot(),
            VsockDeviceMetrics::default().with_muxer_event_fails(1)
        );
    }

    #[test]
    fn balloon_notification_signal_dispatch_accepts_empty_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let result = collect_balloon_notification_dispatches(dispatches)
            .expect("empty balloon dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
    }

    #[test]
    fn memory_hotplug_notification_signal_dispatch_accepts_empty_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_without_drives();
        let dispatches = dispatch_boot_memory_hotplug_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
        );
        let result = collect_memory_hotplug_notification_dispatches(dispatches)
            .expect("empty memory-hotplug dispatch result should collect");

        assert!(result.is_empty());
        assert_eq!(result.len(), 0);
        assert!(!result.has_signal_failure());
    }

    #[test]
    fn balloon_notification_signal_dispatch_skips_noop_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("noop balloon dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert_eq!(
            device.dispatch().device().registration.region_id(),
            MmioRegionId::new(110)
        );
        assert!(!device.dispatch().needs_queue_interrupt());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            0
        );
    }

    #[test]
    fn balloon_statistics_trigger_treats_inactive_device_as_noop() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon_stats();
        let dispatches = runtime
            .trigger_balloon_statistics_update(&mut memory, &mut mmio_dispatcher)
            .expect("statistics trigger dispatch should allocate");
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("inactive statistics trigger should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(matches!(
            device.dispatch().outcome().dispatch_error(),
            Some(VirtioBalloonDeviceNotificationError::Inactive { .. })
        ));
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
        assert_eq!(
            super::balloon_update_result_from_hvf_dispatches(&result),
            Ok(())
        );
        let metrics = SharedBalloonDeviceMetrics::default();
        super::record_balloon_dispatch_metrics(&metrics, &result, false);
        assert_eq!(metrics.snapshot(), BalloonDeviceMetrics::default());
    }

    #[test]
    fn balloon_notification_signal_dispatch_signals_queued_inflate_descriptor() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_inflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued balloon dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("balloon notification should dispatch");
        let inflate = dispatch
            .inflate_queue_dispatch()
            .expect("inflate queue dispatch should be present");
        assert_eq!(inflate.completed_descriptors(), 1);
        assert!(inflate.needs_queue_interrupt());
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_balloon_inflate_used_index(&memory), 1);
    }

    #[test]
    fn memory_hotplug_notification_signal_dispatch_signals_queued_request() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_memory_hotplug();
        configure_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher);
        write_queued_memory_hotplug_state_request(&mut memory);
        notify_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher);
        let dispatches = dispatch_boot_memory_hotplug_notifications(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_memory_hotplug_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued memory-hotplug dispatch should collect");

        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("memory-hotplug notification should dispatch");
        let queue = dispatch
            .queue_dispatch()
            .expect("memory-hotplug queue dispatch should be present");
        assert_eq!(queue.processed_requests(), 1);
        assert_eq!(queue.policy_errors(), 1);
        assert!(queue.needs_queue_interrupt());
        assert_eq!(
            read_boot_memory_hotplug_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        assert_eq!(read_memory_hotplug_used_index(&memory), 1);
    }

    #[test]
    fn memory_hotplug_runtime_dispatch_uses_injected_mutation_executor() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_memory_hotplug();
        configure_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher);
        update_boot_memory_hotplug_requested_size(&mut runtime, &mut mmio_dispatcher, 2);
        write_queued_memory_hotplug_plug_request(&mut memory);
        notify_boot_memory_hotplug_queue(&mut runtime, &mut mmio_dispatcher);
        let mut mutation_executor = RecordingVirtioMemMutationExecutor::default();

        let dispatches = dispatch_boot_memory_hotplug_notifications_with_executor(
            &mut memory,
            &mut runtime,
            &mut mmio_dispatcher,
            &mut mutation_executor,
        );
        let (lines, sink) = RecordingSink::successful();

        let result = signal_memory_hotplug_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued memory-hotplug dispatch should collect");

        let expected_range =
            GuestMemoryRange::new(VIRTIO_MEM_DEFAULT_REGION_ADDRESS, 2 * 1024 * 1024)
                .expect("expected virtio-mem range should be valid");
        assert_eq!(
            mutation_executor.applied,
            vec![VirtioMemMutation::new(VirtioMemMutationKind::Plug(
                expected_range
            ))]
        );
        assert!(mutation_executor.rolled_back.is_empty());
        assert_eq!(result.len(), 1);
        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("memory-hotplug notification should dispatch");
        let queue = dispatch
            .queue_dispatch()
            .expect("memory-hotplug queue dispatch should be present");
        assert_eq!(queue.processed_requests(), 1);
        assert_eq!(queue.policy_errors(), 0);
        assert_eq!(queue.mutation_failures(), 0);
        assert!(queue.needs_queue_interrupt());
        assert_eq!(read_memory_hotplug_used_index(&memory), 1);
    }

    #[test]
    fn balloon_notification_signal_dispatch_signals_queued_deflate_descriptor() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_deflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_DEFLATE_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("queued balloon deflate dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        assert_eq!(recorded_lines(&lines), vec![32]);
        let dispatch = device
            .dispatch()
            .outcome()
            .dispatched()
            .expect("balloon deflate notification should dispatch");
        let deflate = dispatch
            .deflate_queue_dispatch()
            .expect("deflate queue dispatch should be present");
        assert_eq!(deflate.completed_descriptors(), 1);
        assert!(deflate.needs_queue_interrupt());
        assert_eq!(read_balloon_deflate_used_index(&memory), 1);
    }

    #[test]
    fn balloon_notification_signal_dispatch_preserves_partial_error_interrupt_intent() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_partially_invalid_balloon_inflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("partial balloon dispatch result should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().needs_queue_interrupt());
        assert!(device.queue_interrupt_signaled());
        let err = device
            .dispatch()
            .outcome()
            .dispatch_error()
            .expect("partial inflate dispatch error should be preserved");
        let completed = err
            .completed_notification_dispatch()
            .expect("completed notification metadata should be preserved");
        let inflate = completed
            .inflate_queue_dispatch()
            .expect("completed inflate metadata should be present");
        assert_eq!(inflate.completed_descriptors(), 1);
        assert!(inflate.needs_queue_interrupt());
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(read_balloon_inflate_used_index(&memory), 1);
    }

    #[test]
    fn balloon_notification_signal_dispatch_preserves_signal_failure_per_device() {
        let (mut memory, mut runtime, mut mmio_dispatcher) = boot_runtime_with_balloon();
        configure_boot_balloon_queues(&mut runtime, &mut mmio_dispatcher);
        write_queued_balloon_inflate_request(&mut memory);
        notify_boot_balloon_queue(
            &mut runtime,
            &mut mmio_dispatcher,
            VIRTIO_BALLOON_INFLATE_QUEUE_INDEX,
        );
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let runtime_metrics = SharedBalloonDeviceMetrics::default();
        super::record_balloon_runtime_dispatch_metrics(
            &runtime_metrics,
            dispatches.as_slice(),
            true,
        );
        assert_eq!(
            runtime_metrics.snapshot(),
            BalloonDeviceMetrics::new(0, 1, 0, 0, 0, 0)
        );
        let (lines, sink) = RecordingSink::failing("injected balloon signal failure");

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("balloon signal failure should stay per-device");

        let device = &result.as_slice()[0];
        assert!(result.has_signal_failure());
        assert!(!device.queue_interrupt_signaled());
        let err = device
            .signal_error()
            .expect("balloon signal failure should be preserved");
        assert_eq!(
            err.to_string(),
            "failed to signal guest interrupt line 32 for queue interrupt: injected balloon signal failure"
        );
        assert_eq!(recorded_lines(&lines), vec![32]);
        assert_eq!(
            read_boot_balloon_mmio_u32(
                &mut runtime,
                &mut mmio_dispatcher,
                VirtioMmioRegister::InterruptStatus
            ),
            DeviceInterruptKind::Queue.status().bits()
        );
        let metrics = SharedBalloonDeviceMetrics::default();
        super::record_balloon_dispatch_metrics(&metrics, &result, true);
        assert_eq!(
            metrics.snapshot(),
            BalloonDeviceMetrics::new(0, 1, 0, 0, 0, 1)
        );
    }

    #[test]
    fn balloon_notification_signal_dispatch_preserves_missing_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_balloon();
        let mut mmio_dispatcher = MmioDispatcher::new();
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("missing balloon handler dispatch should collect");

        let device = &result.as_slice()[0];
        assert!(device.dispatch().outcome().handler_lookup_error().is_some());
        assert!(!device.queue_interrupt_signaled());
        assert!(device.signal_error().is_none());
        assert!(recorded_lines(&lines).is_empty());
    }

    #[test]
    fn balloon_notification_signal_dispatch_preserves_wrong_handler_without_signal() {
        let (mut memory, mut runtime, _) = boot_runtime_with_balloon();
        let region = runtime
            .balloon_device
            .as_ref()
            .expect("balloon device should exist")
            .registration
            .region();
        let mut mmio_dispatcher = MmioDispatcher::new();
        mmio_dispatcher
            .insert_region(region.id(), region.range().start(), region.range().size())
            .expect("replacement balloon region should insert");
        mmio_dispatcher
            .register_handler(region.id(), WrongBalloonHandler)
            .expect("wrong balloon handler should register");
        let dispatches =
            dispatch_boot_balloon_notifications(&mut memory, &mut runtime, &mut mmio_dispatcher);
        let (lines, sink) = RecordingSink::successful();

        let result = signal_balloon_queue_interrupts(dispatches, sink.as_ref())
            .expect("wrong balloon handler dispatch should collect");

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
    fn boot_session_run_loop_stops_before_first_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        stop_token.request_stop();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stopped loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 0 });
        assert!(session.events.is_empty());
    }

    #[test]
    fn boot_session_run_loop_control_types_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<super::HvfArm64BootRunLoopControl>();
        assert_send_sync::<HvfArm64BootRunLoopStopToken>();
    }

    #[test]
    fn boot_session_run_loop_dispatches_after_mmio_until_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            mmio_run_step_outcome(),
            mmio_run_step_outcome(),
        ]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("step-limited loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_continues_after_hvc_until_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            hvc_run_step_outcome(),
            hvc_run_step_outcome(),
        ]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("step-limited HVC loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            ["run", "vsock-dispatch", "run", "vsock-dispatch"]
        );
    }

    #[test]
    fn boot_session_run_loop_returns_guest_shutdown_as_terminal_outcome() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([guest_shutdown_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("guest shutdown loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::GuestShutdown { steps: 1 }
        );
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_returns_guest_reset_as_terminal_outcome() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([guest_reset_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("guest reset loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::GuestReset { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_continues_after_sys64_until_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            sys64_run_step_outcome(),
            sys64_run_step_outcome(),
        ]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("step-limited SYS64 loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            ["run", "vsock-dispatch", "run", "vsock-dispatch"]
        );
    }

    #[test]
    fn boot_session_run_loop_observer_records_step_outcomes() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            hvc_run_step_outcome(),
            sys64_run_step_outcome(),
            HvfVcpuRunStepOutcome::VtimerActivated,
            mmio_run_step_outcome(),
        ]);
        let mut observed = Vec::new();

        let outcome = super::run_boot_session_loop_with_observer(
            &mut session,
            &stop_token,
            max_steps(4),
            |step| observed.push(*step),
        )
        .expect("observed run loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 4 }
        );
        assert_eq!(
            observed,
            [
                hvc_run_step_outcome(),
                sys64_run_step_outcome(),
                HvfVcpuRunStepOutcome::VtimerActivated,
                mmio_run_step_outcome(),
            ]
        );
        assert_eq!(
            session.events,
            [
                "run",
                "vsock-dispatch",
                "run",
                "vsock-dispatch",
                "run",
                "timer",
                "vsock-dispatch",
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run", "dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_pmem_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_pmem_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after pmem dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run", "dispatch", "pmem-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_network_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_network_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after network dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            ["run", "dispatch", "pmem-dispatch", "network-dispatch"]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_vsock_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_vsock_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after vsock dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_balloon_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_balloon_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after balloon dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_non_mmio_vsock_dispatch() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_stop_on_vsock_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after non-MMIO vsock dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run", "vsock-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_entropy_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_entropy_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after entropy dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_memory_hotplug_dispatch_before_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.request_stop_on_memory_hotplug_dispatch(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after memory-hotplug dispatch should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_hvc_when_requested() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_stop_on_run(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after HVC should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_canceled_step_when_requested() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_stop_on_run(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after canceled step should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_reports_canceled_without_stop_request() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("canceled loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Canceled { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_reports_control_wakeup_after_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_control_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("control wakeup loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Wakeup { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_stop_takes_priority_over_control_wakeup() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_control_wakeup();
        session.request_stop_on_run(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop should take priority over control wakeup");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_vsock_before_control_wakeup() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_wakeup();
        session.request_run_loop_control_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("combined wakeup loop should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Wakeup { steps: 1 });
        assert_eq!(session.events, ["run", "vsock-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_vsock_after_wakeup_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("wakeup cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "vsock-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_block_after_block_retry_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_block_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("block retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "dispatch"]);
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_reschedules_repeated_block_retry_after_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_block_retry_wakeup();
        session.push_block_retry_dispatch(Duration::from_millis(25));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("repeated block retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "dispatch"]);
        assert_eq!(
            session.scheduled_block_retry_wakeups,
            [Some(Duration::from_millis(25))]
        );
    }

    #[test]
    fn boot_session_run_loop_consumes_delayed_block_retry_after_non_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_run_loop_block_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("delayed block retry after HVC should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "dispatch", "vsock-dispatch"]);
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_keeps_wakeup_for_delayed_cancel_after_non_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            hvc_run_step_outcome(),
            HvfVcpuRunStepOutcome::Canceled,
        ]);
        session.push_monitor_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("delayed wakeup cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            ["run", "vsock-dispatch", "run", "vsock-dispatch"]
        );
    }

    #[test]
    fn boot_session_run_loop_schedules_block_retry_after_mmio_dispatch() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_block_retry_dispatch(Duration::from_millis(10));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("MMIO dispatch with block retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch"
            ]
        );
        assert_eq!(
            session.scheduled_block_retry_wakeups,
            [Some(Duration::from_millis(10))]
        );
    }

    #[test]
    fn boot_session_run_loop_clears_block_retry_after_mmio_dispatch_without_retry() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("MMIO dispatch without block retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_entropy_after_entropy_retry_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_entropy_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("entropy retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "entropy-dispatch"]);
        assert_eq!(session.scheduled_entropy_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_reschedules_repeated_entropy_retry_after_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_entropy_retry_wakeup();
        session.push_entropy_retry_dispatch(Duration::from_millis(25));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("repeated entropy retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "entropy-dispatch"]);
        assert_eq!(
            session.scheduled_entropy_retry_wakeups,
            [Some(Duration::from_millis(25))]
        );
    }

    #[test]
    fn boot_session_run_loop_consumes_delayed_entropy_retry_after_non_canceled_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.request_run_loop_entropy_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("delayed entropy retry after HVC should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(
            session.events,
            ["run", "entropy-dispatch", "vsock-dispatch"]
        );
        assert_eq!(session.scheduled_entropy_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_schedules_entropy_retry_after_mmio_dispatch() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_entropy_retry_dispatch(Duration::from_millis(10));

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("MMIO dispatch with entropy retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch"
            ]
        );
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
        assert_eq!(
            session.scheduled_entropy_retry_wakeups,
            [Some(Duration::from_millis(10))]
        );
    }

    #[test]
    fn boot_session_run_loop_clears_entropy_retry_after_mmio_dispatch_without_retry() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("MMIO dispatch without entropy retry should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
        assert_eq!(session.scheduled_entropy_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_dispatches_block_and_entropy_after_retry_cancel() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Canceled]);
        session.request_run_loop_block_retry_wakeup();
        session.request_run_loop_entropy_retry_wakeup();

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("combined retry cancel loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        );
        assert_eq!(session.events, ["run", "dispatch", "entropy-dispatch"]);
        assert_eq!(session.scheduled_block_retry_wakeups, [None]);
        assert_eq!(session.scheduled_entropy_retry_wakeups, [None]);
    }

    #[test]
    fn boot_session_run_loop_preserves_runner_error() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let source = HvfVcpuRunnerError::InvalidState("fake run loop step failed");
        let mut session = RecordingBootSessionRunLoopSession::with_run_error(source.clone());

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("runner error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::RunStep {
                steps_completed,
                source: actual,
            } => {
                assert_eq!(steps_completed, 0);
                assert_eq!(*actual, source);
            }
            other => panic!("expected run-step error, got {other:?}"),
        }
        assert_eq!(session.events, ["run"]);
    }

    #[test]
    fn boot_session_run_loop_preserves_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_dispatch_error(HvfArm64BootBlockNotificationDispatchError::MmioDispatcher {
            source: HvfArm64BootMmioDispatcherError::Busy,
        });

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchBlockNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected notification error, got {other:?}"),
        }
        assert_eq!(session.events, ["run", "dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_preserves_pmem_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_pmem_dispatch_error(
            HvfArm64BootPmemNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("pmem notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchPmemNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected pmem notification error, got {other:?}"),
        }
        assert_eq!(session.events, ["run", "dispatch", "pmem-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_preserves_network_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_network_dispatch_error(
            HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("network notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchNetworkNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected network notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            ["run", "dispatch", "pmem-dispatch", "network-dispatch"]
        );
    }

    #[test]
    fn boot_session_run_loop_preserves_vsock_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_vsock_dispatch_error(
            HvfArm64BootVsockNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("vsock notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchVsockNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected vsock notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_preserves_balloon_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_balloon_dispatch_error(
            HvfArm64BootBalloonNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("balloon notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchBalloonNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected balloon notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_preserves_memory_hotplug_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_memory_hotplug_dispatch_error(
            HvfArm64BootMemoryHotplugNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("memory-hotplug notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchMemoryHotplugNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected memory-hotplug notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_preserves_vsock_notification_error_after_non_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([hvc_run_step_outcome()]);
        session.push_vsock_dispatch_error(
            HvfArm64BootVsockNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("non-MMIO vsock notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchVsockNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected vsock notification error, got {other:?}"),
        }
        assert_eq!(session.events, ["run", "vsock-dispatch"]);
    }

    #[test]
    fn boot_session_run_loop_preserves_entropy_notification_error_after_mmio_step() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([mmio_run_step_outcome()]);
        session.push_entropy_dispatch_error(
            HvfArm64BootEntropyNotificationDispatchError::MmioDispatcher {
                source: HvfArm64BootMmioDispatcherError::Busy,
            },
        );

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("entropy notification error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::DispatchEntropyNotifications {
                steps_completed,
                source,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(
                    source.to_string(),
                    "failed to lock HVF boot-session MMIO dispatcher: HVF boot-session MMIO dispatcher lock is busy"
                );
            }
            other => panic!("expected entropy notification error, got {other:?}"),
        }
        assert_eq!(
            session.events,
            [
                "run",
                "dispatch",
                "pmem-dispatch",
                "network-dispatch",
                "vsock-dispatch",
                "balloon-dispatch",
                "memory-hotplug-dispatch",
                "entropy-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_handles_virtual_timer_until_step_limit() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session = RecordingBootSessionRunLoopSession::new([
            HvfVcpuRunStepOutcome::VtimerActivated,
            HvfVcpuRunStepOutcome::VtimerActivated,
        ]);

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(2))
            .expect("virtual timer loop should succeed");

        assert_eq!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 2 }
        );
        assert_eq!(
            session.events,
            [
                "run",
                "timer",
                "vsock-dispatch",
                "run",
                "timer",
                "vsock-dispatch",
            ]
        );
    }

    #[test]
    fn boot_session_run_loop_preserves_virtual_timer_handler_error() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::VtimerActivated]);
        let source = HvfVcpuRunnerError::InvalidState("fake timer handler failed");
        session.push_timer_error(source.clone());

        let err = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect_err("virtual timer handler error should stop loop");

        match err {
            super::HvfArm64BootRunLoopError::HandleVirtualTimer {
                steps_completed,
                source: actual,
            } => {
                assert_eq!(steps_completed, 1);
                assert_eq!(*actual, source);
            }
            other => panic!("expected virtual timer handler error, got {other:?}"),
        }
        assert_eq!(session.events, ["run", "timer"]);
    }

    #[test]
    fn boot_session_run_loop_reports_stop_after_virtual_timer_handler() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::VtimerActivated]);
        session.request_stop_on_timer(stop_token.clone());

        let outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("stop after virtual timer handler should succeed");

        assert_eq!(outcome, HvfArm64BootRunLoopOutcome::Stopped { steps: 1 });
        assert_eq!(session.events, ["run", "timer"]);
    }

    #[test]
    fn boot_session_run_loop_returns_unknown_as_terminal_outcome() {
        let stop_token = HvfArm64BootRunLoopStopToken::new();
        let mut session =
            RecordingBootSessionRunLoopSession::new([HvfVcpuRunStepOutcome::Unknown {
                reason: 99,
            }]);

        let unknown_outcome = run_boot_session_loop(&mut session, &stop_token, max_steps(1))
            .expect("unknown-exit loop should succeed");

        assert_eq!(
            unknown_outcome,
            HvfArm64BootRunLoopOutcome::Unknown {
                steps: 1,
                reason: 99
            }
        );
        assert_eq!(session.events, ["run"]);
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
    fn displays_network_notification_dispatch_errors() {
        let err = HvfArm64BootNetworkNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for network notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootNetworkNotificationDispatchError::MmioDispatcher {
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
    fn displays_vsock_notification_dispatch_errors() {
        let err = HvfArm64BootVsockNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for vsock notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootVsockNotificationDispatchError::MmioDispatcher {
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
    fn displays_memory_hotplug_notification_dispatch_errors() {
        let err = HvfArm64BootMemoryHotplugNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for memory-hotplug notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootMemoryHotplugNotificationDispatchError::MmioDispatcher {
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
    fn displays_entropy_notification_dispatch_errors() {
        let err = HvfArm64BootEntropyNotificationDispatchError::MapGuestMemory {
            source: crate::memory::HvfGuestMemoryMappingError::InvalidState("mapping missing"),
        };

        assert_eq!(
            err.to_string(),
            "failed to borrow HVF boot-session guest memory for entropy notifications: invalid guest memory mapping state: mapping missing"
        );
        assert_eq!(
            err.source().map(ToString::to_string),
            Some("invalid guest memory mapping state: mapping missing".to_string())
        );

        let err = HvfArm64BootEntropyNotificationDispatchError::MmioDispatcher {
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
            SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
        );

        let network_layout =
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            network_layout,
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_serial_device(serial);

        assert!(config.serial_device.is_some());
        assert_eq!(config.network_mmio_layout, network_layout);
        assert_eq!(config.rtc_mmio_layout.base(), TEST_RTC_MMIO_BASE);
    }

    #[test]
    fn session_config_stores_entropy_device() {
        let entropy = HvfArm64BootEntropyDeviceConfig::new(EntropyMmioLayout::new(
            GuestAddress::new(0x8000_0000),
            MmioRegionId::new(3000),
        ));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_entropy_device(entropy);

        assert_eq!(config.entropy_device, Some(entropy));
        assert!(config.serial_device.is_none());
    }

    #[test]
    fn session_config_stores_memory_hotplug_device() {
        let memory_hotplug = HvfArm64BootMemoryHotplugDeviceConfig::new(VirtioMemMmioLayout::new(
            GuestAddress::new(0x4000_9000),
            MmioRegionId::new(5000),
        ));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_memory_hotplug_device(memory_hotplug);

        assert_eq!(config.memory_hotplug_device, Some(memory_hotplug));
        assert_eq!(config.entropy_device, None);
        assert_eq!(config.balloon_device, None);
    }

    #[test]
    fn session_config_stores_boot_timer_device() {
        let boot_timer = HvfArm64BootTimerDeviceConfig::new(BootTimerMmioLayout::new(
            GuestAddress::new(0x4000_0000),
            MmioRegionId::new(0),
        ));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_boot_timer_device(boot_timer);

        assert_eq!(config.boot_timer_device, Some(boot_timer));
        assert_eq!(config.entropy_device, None);
        assert_eq!(config.balloon_device, None);
    }

    #[test]
    fn session_config_stores_balloon_device() {
        let balloon = HvfArm64BootBalloonDeviceConfig::new(BalloonMmioLayout::new(
            GuestAddress::new(0x4000_8000),
            MmioRegionId::new(4000),
        ));
        let config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(TEST_RTC_MMIO_BASE, MmioRegionId::new(3000)),
        )
        .with_balloon_device(balloon);

        assert_eq!(config.balloon_device, Some(balloon));
        assert_eq!(config.entropy_device, None);
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
    fn interrupt_lines_allocate_vmclock_after_vmgenid() {
        let lines = allocate_interrupt_lines(
            &gic_with_spi_range(32, 13),
            HvfArm64BootInterruptRequest {
                block_device_count: 2,
                pmem_device_count: 2,
                network_device_count: 2,
                vsock_configured: true,
                balloon_configured: true,
                entropy_configured: true,
                memory_hotplug_configured: true,
                serial_configured: true,
            },
        )
        .expect("interrupt lines should allocate");

        assert_eq!(line_values(&lines.block), vec![32, 33]);
        assert_eq!(line_values(&lines.pmem), vec![34, 35]);
        assert_eq!(line_values(&lines.network), vec![36, 37]);
        assert_eq!(lines.vsock.map(|line| line.raw_value()), Some(38));
        assert_eq!(lines.balloon.map(|line| line.raw_value()), Some(39));
        assert_eq!(lines.entropy.map(|line| line.raw_value()), Some(40));
        assert_eq!(lines.memory_hotplug.map(|line| line.raw_value()), Some(41));
        assert_eq!(lines.serial.map(|line| line.raw_value()), Some(42));
        assert_eq!(lines.vmgenid.raw_value(), 43);
        assert_eq!(lines.vmclock.raw_value(), 44);
    }

    #[test]
    fn interrupt_lines_allocate_none_for_absent_serial() {
        let lines = allocate_interrupt_lines(
            &gic_with_spi_range(40, 5),
            HvfArm64BootInterruptRequest {
                block_device_count: 2,
                network_device_count: 1,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect("interrupt lines should allocate");

        assert_eq!(line_values(&lines.block), vec![40, 41]);
        assert!(lines.pmem.is_empty());
        assert_eq!(line_values(&lines.network), vec![42]);
        assert_eq!(lines.vsock, None);
        assert_eq!(lines.balloon, None);
        assert_eq!(lines.entropy, None);
        assert_eq!(lines.memory_hotplug, None);
        assert_eq!(lines.serial, None);
        assert_eq!(lines.vmgenid.raw_value(), 43);
        assert_eq!(lines.vmclock.raw_value(), 44);
    }

    #[test]
    fn interrupt_lines_report_vmgenid_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 1),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("VMGenID allocation should exhaust range after block");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VmGenIdDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_vmclock_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 1),
            HvfArm64BootInterruptRequest::default(),
        )
        .expect_err("VMClock allocation should exhaust range after VMGenID");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VmClockDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_pmem_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 1),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                pmem_device_count: 1,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("pmem allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::PmemDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_vsock_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 2),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                vsock_configured: true,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("vsock allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::VsockDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_balloon_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 3),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                vsock_configured: true,
                balloon_configured: true,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("balloon allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::BalloonDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_entropy_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 3),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                vsock_configured: true,
                entropy_configured: true,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("entropy allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::EntropyDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_report_serial_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 3),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                vsock_configured: true,
                serial_configured: true,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
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
    fn interrupt_lines_report_network_exhaustion_with_purpose() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(32, 1),
            HvfArm64BootInterruptRequest {
                block_device_count: 1,
                network_device_count: 1,
                ..HvfArm64BootInterruptRequest::default()
            },
        )
        .expect_err("network allocation should exhaust range");

        assert!(matches!(
            err,
            HvfArm64BootSessionError::AllocateInterruptLine {
                purpose: HvfArm64BootInterruptLinePurpose::NetworkDevice,
                ..
            }
        ));
    }

    #[test]
    fn interrupt_lines_reject_invalid_gic_range() {
        let err = allocate_interrupt_lines(
            &gic_with_spi_range(31, 1),
            HvfArm64BootInterruptRequest::default(),
        )
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
